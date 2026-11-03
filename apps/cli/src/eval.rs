//! `theodore eval`: replay saved PRs (fixture = revision JSON + PR meta)
//! through the grouping pipeline and print a quality/cost table.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use theodore_domain::{AgentId, HunkId, Revision, grouping};
use theodore_grouping::{
    GroupingOptions, PrMeta, Progress, Stage, group_revision_progress, prepass, prompt,
};
use theodore_ports::AgentPort;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FixturePr {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub commits: Vec<String>,
}

impl From<FixturePr> for PrMeta {
    fn from(p: FixturePr) -> PrMeta {
        PrMeta {
            title: p.title,
            body: p.body,
            commits: p.commits,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    #[serde(default)]
    pub pr: FixturePr,
    pub revision: Revision,
    /// Code-intelligence links between its hunks, when the fixture carries
    /// them or `--symbol-links` computed them (`links::fill`).
    #[serde(default)]
    pub symbol_links: Vec<grouping::SymbolLink>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct EvalArgs {
    /// Directory of `*.json` fixtures (see apps/cli/fixtures/).
    pub fixtures_dir: PathBuf,
    #[arg(long, value_enum, default_value = "claude")]
    pub agent: crate::group::AgentChoice,
    /// Model override passed to the agent CLI (e.g. "sonnet", "haiku").
    /// Omit to use the agent's own default.
    #[arg(long)]
    pub model: Option<String>,
    /// Call an OpenAI-compatible `/v1/chat/completions` server at this base
    /// url (e.g. a local `llama-server`) instead of the agent CLI. `--model`
    /// is sent as the request's model name.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// `max_tokens` per `--endpoint` request.
    #[arg(long, default_value_t = 8192)]
    pub max_tokens: u32,
    /// With `--endpoint`: restrict hunk and group ids in the response schema
    /// to the ids listed in the prompt.
    #[arg(long)]
    pub constrain_ids: bool,
    /// Prompt size (chars) above which a PR is split into chunks.
    #[arg(long)]
    pub max_chars: Option<usize>,
    /// Max chunk prompts in flight at once.
    #[arg(long)]
    pub concurrency: Option<usize>,
    /// Write each fixture's grouping and run stats to `<dir>/<fixture>.json`.
    #[arg(long)]
    pub save: Option<PathBuf>,
    /// Stream groups as the app does for Claude: parse each chunk's answer
    /// as it is written and record when each group came out
    /// (docs/eval/streamed-groups.md). Off by default, so runs stay
    /// comparable with earlier evals; Claude only.
    #[arg(long)]
    pub stream: bool,
    /// No agent calls: report the relatedness graph and the chunks it
    /// yields for each fixture (offline check, docs/eval/graph-grouping.md).
    #[arg(long)]
    pub graph_only: bool,
    /// With `--graph-only`: a saved run directory (`--save` output) whose
    /// groupings the graph is scored against.
    #[arg(long, requires = "graph_only")]
    pub compare: Option<PathBuf>,
    /// With `--graph-only`: also print every community, rollout and chunk.
    #[arg(long, requires = "graph_only")]
    pub show: bool,
    /// Give the grouper code-intelligence symbol links between hunks
    /// (`RepoContext::symbol_links`), computed with the syntax layer over a
    /// sparse checkout of each fixture's hunks (docs/eval/symbol-links.md).
    #[arg(long)]
    pub symbol_links: bool,
    /// With `--symbol-links`: where the checkouts and the code-intelligence
    /// data (grammars, index) go. Default: `<tmp>/theodore-eval-links`.
    #[arg(long, requires = "symbol_links")]
    pub links_dir: Option<PathBuf>,
}

/// Load every `*.json` fixture in `dir`, sorted by file name.
pub fn load_fixtures(dir: &Path) -> Result<Vec<(String, Fixture)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let mut fixture: Fixture = serde_json::from_str(&text)
            .with_context(|| format!("parsing fixture {}", path.display()))?;
        // Fixtures saved before hunk ids were de-duplicated may repeat ids.
        theodore_domain::interdiff::dedupe_hunk_ids(&mut fixture.revision.files);
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push((name, fixture));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    if out.is_empty() {
        anyhow::bail!("no *.json fixtures found in {}", dir.display());
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct EvalRow {
    pub name: String,
    pub valid_first_try: bool,
    pub valid_after_repair: bool,
    pub latency_ms: u64,
    /// When the first group was shown: the first streamed one, or
    /// `latency_ms` for a run that didn't stream.
    pub first_group_ms: u64,
    pub usd: f64,
    pub groups: usize,
    pub error: Option<String>,
}

/// Run every fixture through the pipeline. Never fails outright — a single
/// fixture's agent error is recorded as a row, not a process failure.
pub async fn run_eval(
    agent: &dyn AgentPort,
    agent_id: AgentId,
    fixtures: &[(String, Fixture)],
    cwd: &Path,
) -> Vec<EvalRow> {
    run_eval_with_model(agent, agent_id, None, fixtures, cwd).await
}

/// Same as `run_eval`, with an optional model override (`--model`)
/// forwarded to `GroupingOptions::model` for every fixture.
pub async fn run_eval_with_model(
    agent: &dyn AgentPort,
    agent_id: AgentId,
    model: Option<String>,
    fixtures: &[(String, Fixture)],
    cwd: &Path,
) -> Vec<EvalRow> {
    let opts = GroupingOptions {
        agent: agent_id,
        model,
        ..Default::default()
    };
    run_eval_with_options(agent, &opts, fixtures, cwd, None).await
}

/// Same as `run_eval`, with full `GroupingOptions`; when `save` is set, each
/// fixture's grouping and run stats go to `<save>/<fixture>.json`.
pub async fn run_eval_with_options(
    agent: &dyn AgentPort,
    opts: &GroupingOptions,
    fixtures: &[(String, Fixture)],
    cwd: &Path,
    save: Option<&Path>,
) -> Vec<EvalRow> {
    let mut rows = Vec::with_capacity(fixtures.len());
    for (name, fx) in fixtures {
        let pr: PrMeta = fx.pr.clone().into();
        let seen = Previews::default();
        let on = |p: Progress<'_>| seen.record(p);
        let ctx = grouping::RepoContext {
            symbol_links: fx.symbol_links.clone(),
            ..Default::default()
        };
        match group_revision_progress(agent, &pr, &fx.revision, &ctx, cwd, opts, &on).await {
            Ok(report) => {
                let previews = seen.against(&report.grouping);
                let valid_after_repair = grouping::validate(&fx.revision, &report.grouping).is_ok();
                let chunks = prompt::build_prompts(
                    &pr,
                    &fx.revision,
                    &prepass::run(&fx.revision),
                    &opts.budget,
                )
                .len();
                if let Some(dir) = save
                    && let Err(e) =
                        save_report(dir, name, &report, chunks, valid_after_repair, &previews)
                {
                    eprintln!("could not save {name}: {e}");
                }
                rows.push(EvalRow {
                    name: name.clone(),
                    valid_first_try: report.valid_first_try,
                    valid_after_repair,
                    latency_ms: report.duration_ms,
                    first_group_ms: report.first_group_ms,
                    usd: report.usage.usd,
                    groups: report.grouping.groups.len(),
                    error: None,
                });
            }
            Err(e) => rows.push(EvalRow {
                name: name.clone(),
                valid_first_try: false,
                valid_after_repair: false,
                latency_ms: 0,
                first_group_ms: 0,
                usd: 0.0,
                groups: 0,
                error: Some(e.to_string()),
            }),
        }
    }
    rows
}

/// Every distinct group a streaming run showed before its chunk landed
/// (`Stage::Streamed`), to tell how many the final grouping kept as shown.
#[derive(Default)]
struct Previews(std::sync::Mutex<Vec<(String, Vec<HunkId>)>>);

/// How the previews fared: shown, kept with the same hunks, and the titles
/// of those that changed.
#[derive(Debug, Default, serde::Serialize)]
pub struct PreviewOutcome {
    pub shown: usize,
    pub kept: usize,
    pub changed: Vec<String>,
}

impl Previews {
    fn record(&self, p: Progress<'_>) {
        if !matches!(p.stage, Stage::Streamed { .. }) {
            return;
        }
        let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for g in &p.grouping.groups {
            if g.kind == theodore_domain::GroupKind::Hidden {
                continue;
            }
            let mut hunks = g.hunks.clone();
            hunks.sort();
            if !seen.iter().any(|(_, h)| *h == hunks) {
                seen.push((g.title.clone(), hunks));
            }
        }
    }

    fn against(&self, last: &theodore_domain::Grouping) -> PreviewOutcome {
        let finals: Vec<Vec<HunkId>> = last
            .groups
            .iter()
            .map(|g| {
                let mut h = g.hunks.clone();
                h.sort();
                h
            })
            .collect();
        let seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let changed: Vec<String> = seen
            .iter()
            .filter(|(_, h)| !finals.contains(h))
            .map(|(t, _)| t.clone())
            .collect();
        PreviewOutcome {
            shown: seen.len(),
            kept: seen.len() - changed.len(),
            changed,
        }
    }
}

fn save_report(
    dir: &Path,
    name: &str,
    report: &theodore_grouping::GroupingReport,
    chunks: usize,
    valid_after_repair: bool,
    previews: &PreviewOutcome,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let value = serde_json::json!({
        "fixture": name,
        "chunks": chunks,
        "attempts": report.attempts,
        "valid_first_try": report.valid_first_try,
        "valid_after_repair": valid_after_repair,
        "duration_ms": report.duration_ms,
        "first_group_ms": report.first_group_ms,
        "done_ms": report.duration_ms,
        "stream": {
            "group_ms": report.stream.group_ms,
            "answer_ms": report.stream.answer_ms,
        },
        "previews": previews,
        "usage": report.usage,
        "grouping": report.grouping,
    });
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&value)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// M1 exit targets (docs/plan.md §5.4): at least 90% valid on the first
/// agent call, 100% valid after the one repair call + "Other" fallback.
pub const TARGET_FIRST_TRY_PCT: f64 = 90.0;
pub const TARGET_AFTER_REPAIR_PCT: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvalSummary {
    pub total: usize,
    pub first_try: usize,
    pub after_repair: usize,
}

impl EvalSummary {
    pub fn from_rows(rows: &[EvalRow]) -> Self {
        Self {
            total: rows.len(),
            first_try: rows.iter().filter(|r| r.valid_first_try).count(),
            after_repair: rows.iter().filter(|r| r.valid_after_repair).count(),
        }
    }

    fn pct(n: usize, total: usize) -> f64 {
        if total == 0 {
            100.0
        } else {
            100.0 * n as f64 / total as f64
        }
    }

    pub fn first_try_pct(&self) -> f64 {
        Self::pct(self.first_try, self.total)
    }

    pub fn after_repair_pct(&self) -> f64 {
        Self::pct(self.after_repair, self.total)
    }

    /// Whether this run still clears the §5.4 M1 exit bar.
    pub fn meets_targets(&self) -> bool {
        self.first_try_pct() >= TARGET_FIRST_TRY_PCT
            && self.after_repair_pct() >= TARGET_AFTER_REPAIR_PCT
    }
}

pub fn print_table(rows: &[EvalRow]) -> EvalSummary {
    println!(
        "{:<20} {:>9} {:>10} {:>10} {:>10} {:>8} {:>7}",
        "fixture", "1st-try", "repaired", "first", "latency", "cost", "groups"
    );
    for r in rows {
        if let Some(err) = &r.error {
            println!("{:<20} ERROR: {err}", r.name);
            continue;
        }
        println!(
            "{:<20} {:>9} {:>10} {:>8}ms {:>8}ms {:>8.4} {:>7}",
            r.name,
            if r.valid_first_try { "yes" } else { "no" },
            if r.valid_after_repair { "yes" } else { "no" },
            r.first_group_ms,
            r.latency_ms,
            r.usd,
            r.groups,
        );
    }
    let summary = EvalSummary::from_rows(rows);
    println!(
        "\n{}/{} valid first try ({:.0}%), {}/{} valid after repair ({:.0}%)",
        summary.first_try,
        summary.total,
        summary.first_try_pct(),
        summary.after_repair,
        summary.total,
        summary.after_repair_pct(),
    );
    if !summary.meets_targets() {
        println!(
            "\nBELOW §5.4 M1 TARGETS (first try >= {TARGET_FIRST_TRY_PCT:.0}%, after repair >= {TARGET_AFTER_REPAIR_PCT:.0}%)"
        );
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(valid_first_try: bool, valid_after_repair: bool) -> EvalRow {
        EvalRow {
            name: "x".into(),
            valid_first_try,
            valid_after_repair,
            latency_ms: 0,
            first_group_ms: 0,
            usd: 0.0,
            groups: 1,
            error: None,
        }
    }

    #[test]
    fn meets_targets_at_exactly_the_m1_bar() {
        // 9/10 first try (90%), 10/10 after repair (100%): right on the line.
        let mut rows: Vec<EvalRow> = (0..9).map(|_| row(true, true)).collect();
        rows.push(row(false, true));
        let summary = EvalSummary::from_rows(&rows);
        assert_eq!(summary.first_try_pct(), 90.0);
        assert_eq!(summary.after_repair_pct(), 100.0);
        assert!(summary.meets_targets());
    }

    #[test]
    fn first_try_below_90_percent_fails_the_gate() {
        let mut rows: Vec<EvalRow> = (0..8).map(|_| row(true, true)).collect();
        rows.push(row(false, true));
        rows.push(row(false, true));
        let summary = EvalSummary::from_rows(&rows);
        assert_eq!(summary.first_try_pct(), 80.0);
        assert!(!summary.meets_targets());
    }

    #[test]
    fn any_row_not_valid_after_repair_fails_the_gate() {
        let mut rows: Vec<EvalRow> = (0..9).map(|_| row(true, true)).collect();
        rows.push(row(false, false));
        let summary = EvalSummary::from_rows(&rows);
        assert_eq!(summary.after_repair_pct(), 90.0);
        assert!(!summary.meets_targets());
    }

    #[test]
    fn empty_run_trivially_meets_targets() {
        let summary = EvalSummary::from_rows(&[]);
        assert!(summary.meets_targets());
    }
}
