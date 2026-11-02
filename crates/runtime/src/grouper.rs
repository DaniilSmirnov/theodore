//! `app::Grouper` over the grouping pipeline (plan §5).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use theodore_app::{Grouper, GrouperError, GroupingAgent, OnPartial};
use theodore_domain::grouping::RepoContext;
use theodore_domain::{AgentId, Grouping, PrDetail, Revision};
use theodore_grouping::{GroupingOptions, PrMeta, Progress, Stage};
use theodore_ports::AgentPort;

pub struct GroupingGrouper {
    agents: Arc<dyn AgentPort>,
    opts: GroupingOptions,
}

impl GroupingGrouper {
    pub fn new(agents: Arc<dyn AgentPort>) -> Self {
        Self {
            agents,
            opts: GroupingOptions::default(),
        }
    }

    pub fn with_options(mut self, opts: GroupingOptions) -> Self {
        self.opts = opts;
        self
    }
}

fn meta(pr: &PrDetail) -> PrMeta {
    PrMeta {
        title: pr.summary.title.clone(),
        body: pr.body.clone(),
        commits: pr.commits.iter().map(|c| c.title.clone()).collect(),
    }
}

fn err(e: theodore_grouping::GroupingPipelineError) -> GrouperError {
    match e {
        theodore_grouping::GroupingPipelineError::DuplicateHunkIds(_) => {
            GrouperError::Invalid(e.to_string())
        }
        e => {
            let text = e.to_string();
            match theodore_ports::offline_message_in(&text) {
                Some(offline) => GrouperError::Offline(offline.to_string()),
                None => GrouperError::Agent(text),
            }
        }
    }
}

/// Whether a grouping run on `agent` streams its groups as they are
/// written: Claude Code and Gemini, whose runners emit `TextDelta`s. Codex
/// reports the answer whole (`codex exec --json` has no deltas), so its
/// runs stay as they were.
fn streams(agent: AgentId) -> bool {
    matches!(agent, AgentId::Claude | AgentId::Gemini)
}

/// Hands each provisional grouping on, logging the first one at info (time
/// to the first group is what streaming is for) and the rest at debug.
fn progress(on_partial: OnPartial<'_>) -> impl Fn(Progress<'_>) + Send + Sync + '_ {
    let first = std::sync::atomic::AtomicBool::new(true);
    move |p: Progress<'_>| {
        let groups = p.grouping.groups.len();
        if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            tracing::info!(elapsed_ms = p.elapsed_ms, groups, "grouping first group");
        } else if let Stage::Chunk { done, of } = p.stage {
            tracing::info!(
                elapsed_ms = p.elapsed_ms,
                done,
                of,
                groups,
                "grouping chunk landed"
            );
        } else {
            tracing::debug!(elapsed_ms = p.elapsed_ms, ?p.stage, groups, "grouping progress");
        }
        on_partial(p.grouping.clone());
    }
}

/// Logs a finished run and emits `grouping_done` (docs/telemetry.md). When
/// the first group was shown also goes on the enclosing `grouping` span.
fn done(report: theodore_grouping::GroupingReport, agent: AgentId, rev: &Revision) -> Grouping {
    let hunks = rev.hunks().count() as u32;
    let groups = report.grouping.groups.len() as u32;
    tracing::Span::current().record("first_group_ms", report.first_group_ms);
    tracing::info!(
        agent = agent.as_str(),
        duration_ms = report.duration_ms,
        first_group_ms = report.first_group_ms,
        attempts = report.attempts,
        valid_first_try = report.valid_first_try,
        chunks = report.chunks,
        groups,
        hunks,
        "grouping done"
    );
    if !report.valid_first_try {
        tracing::warn!(
            error.code = "grouping.repaired",
            agent = agent.as_str(),
            attempts = report.attempts,
            "grouping needed a repair round or the Other fallback"
        );
    }
    theodore_telemetry::product(theodore_telemetry::ProductEvent::GroupingDone {
        ms: report.duration_ms,
        valid_first_try: report.valid_first_try,
        groups,
        hunks,
        agent: agent.as_str().to_string(),
        chunks: report.chunks,
    });
    report.grouping
}

#[async_trait]
impl Grouper for GroupingGrouper {
    async fn group(
        &self,
        rev: &Revision,
        pr: &PrDetail,
        ctx: &RepoContext,
        worktree: &Path,
        run: GroupingAgent,
        on_partial: OnPartial<'_>,
    ) -> Result<Grouping, GrouperError> {
        let agent = run.id;
        let mut opts = self.opts.clone();
        opts.agent = agent;
        opts.user_mcp = run.user_mcp;
        opts.stream = streams(agent);
        let on = progress(on_partial);
        theodore_grouping::group_revision_progress(
            &*self.agents,
            &meta(pr),
            rev,
            ctx,
            worktree,
            &opts,
            &on,
        )
        .await
        .map(|r| done(r, agent, rev))
        .map_err(err)
    }

    async fn regroup(
        &self,
        prev: &Grouping,
        rev: &Revision,
        pr: &PrDetail,
        // Head files and co-change sharpen the relatedness graph that
        // suggests a group for each new hunk.
        ctx: &RepoContext,
        worktree: &Path,
        run: GroupingAgent,
    ) -> Result<Grouping, GrouperError> {
        let agent = run.id;
        let mut opts = self.opts.clone();
        opts.agent = agent;
        opts.user_mcp = run.user_mcp;
        // The previous grouping's hunk ids stand in for the old revision:
        // unchanged hunks keep their ids, so anything not in `prev` is new.
        let known: std::collections::BTreeSet<_> = prev
            .groups
            .iter()
            .flat_map(|g| g.hunks.iter().cloned())
            .collect();
        let changes = rev
            .hunks()
            .map(|h| {
                let c = if known.contains(&h.id) {
                    theodore_domain::interdiff::HunkChange::Unchanged
                } else {
                    theodore_domain::interdiff::HunkChange::New
                };
                (h.id.clone(), c)
            })
            .collect();
        theodore_grouping::regroup_with(
            &*self.agents,
            &meta(pr),
            prev,
            rev,
            &changes,
            ctx,
            worktree,
            &opts,
        )
        .await
        .map(|r| done(r, agent, rev))
        .map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_grouping::GroupingPipelineError;

    #[test]
    fn an_agent_that_could_not_connect_is_offline_not_a_failure() {
        let offline = theodore_ports::offline_message("claude");
        let e = err(GroupingPipelineError::AgentRun(format!(
            "{offline}: getaddrinfo ENOTFOUND api.anthropic.com"
        )));
        assert!(
            matches!(&e, GrouperError::Offline(m) if *m == offline),
            "{e:?}"
        );
        assert_eq!(e.code(), "agent.offline");
        let e = err(GroupingPipelineError::AgentRun("exited 1: boom".into()));
        assert!(matches!(e, GrouperError::Agent(_)), "{e:?}");
    }

    /// The app streams Claude and Gemini groupings (each group handed on
    /// as it is written, marked in progress); a Codex run publishes nothing
    /// and asks for no deltas.
    #[tokio::test]
    async fn claude_and_gemini_runs_stream_their_groups_and_codex_does_not() {
        use theodore_domain::{PrRef, RepoRef, Sha};
        use theodore_grouping::testing::FakeAgentPort;
        let (base, head) = (Sha::new("base000"), Sha::new("head111"));
        let rev = theodore_app::testkit::two_hunk_revision(&base, &head, "src/lib.rs");
        let ids = theodore_app::testkit::hunk_ids(&rev);
        let pr_ref = PrRef {
            repo: RepoRef {
                host: theodore_domain::HostKind::Github,
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number: 7,
            host_id: None,
        };
        let detail = theodore_app::testkit::sample_pr_detail(&pr_ref, &base, &head);
        let answer = format!(
            r#"{{"groups": [{{"id": "a", "title": "A", "why": "a", "hunks": ["{}"]}},
                {{"id": "b", "title": "B", "why": "b", "hunks": ["{}"]}}]}}"#,
            ids[0], ids[1]
        );
        for (agent, streamed) in [
            (AgentId::Claude, 2),
            (AgentId::Gemini, 2),
            (AgentId::Codex, 0),
        ] {
            let fake = Arc::new(FakeAgentPort::new([answer.clone()]));
            let grouper = GroupingGrouper::new(fake.clone());
            let seen = std::sync::Mutex::new(Vec::new());
            let on = |g: Grouping| seen.lock().unwrap().push(g);
            let out = grouper
                .group(
                    &rev,
                    &detail,
                    &RepoContext::default(),
                    Path::new("."),
                    GroupingAgent {
                        id: agent,
                        user_mcp: false,
                    },
                    &on,
                )
                .await
                .unwrap();
            let seen = seen.into_inner().unwrap();
            assert_eq!(seen.len(), streamed, "{agent:?}");
            assert!(seen.iter().all(|g| g.in_progress), "{agent:?}");
            assert!(!out.in_progress);
            assert_eq!(out.groups.len(), 2);
            assert_eq!(fake.requests()[0].stream_text, streamed > 0, "{agent:?}");
        }
    }
    /// Gemini's runner output, unchanged, feeds the grouping stream: its
    /// chunks split the answer anywhere (here mid-key and mid-object), and
    /// each group still comes out as soon as it closes.
    #[test]
    fn gemini_deltas_feed_the_group_stream() {
        use theodore_agents::gemini::GeminiParser;
        use theodore_grouping::stream::GroupStream;
        use theodore_ports::agent::AgentEvent;
        let answer = r#"Here: {"groups": [{"id": "a", "title": "A", "why": "a", "hunks": ["h1"]}, {"id": "b", "title": "B", "why": "b", "hunks": ["h2"]}]}"#;
        let mut parser = GeminiParser::new(true);
        let mut stream = GroupStream::new();
        let mut ids = Vec::new();
        let mut lines: Vec<String> = answer
            .as_bytes()
            .chunks(7)
            .map(|c| {
                serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": std::str::from_utf8(c).unwrap(),
                    "delta": true,
                })
                .to_string()
            })
            .collect();
        lines.push(r#"{"type":"result","status":"success","stats":{}}"#.into());
        let mut final_text = None;
        for line in &lines {
            for ev in parser.parse_line(line) {
                match ev {
                    AgentEvent::TextDelta { text } => {
                        ids.extend(stream.push(&text).into_iter().map(|g| g.id));
                    }
                    AgentEvent::Done { final_text: t, .. } => final_text = Some(t),
                    _ => {}
                }
            }
        }
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(final_text.as_deref(), Some(answer));
    }
}
