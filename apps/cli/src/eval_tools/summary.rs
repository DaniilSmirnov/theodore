//! One run's record (a line of `runs.jsonl`) and the aggregation into the
//! summary: per condition, per task, and the on − off deltas.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::tasks::TaskKind;

/// With or without the code tools.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Condition {
    /// No code tools: Grep, Read and the rest of the agent's own.
    Off,
    /// The code tools attached, and the app's system-prompt line about them.
    On,
}

impl Condition {
    pub fn as_str(self) -> &'static str {
        match self {
            Condition::Off => "off",
            Condition::On => "on",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The agent finished; the score is the check's.
    Ok,
    /// The agent reported an error or exited without a result.
    AgentError,
    /// Killed at the task's `timeout_s`.
    Timeout,
    /// Setting the run up failed (worktree, spawn).
    SetupError,
}

/// One (task, condition, repeat).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    /// `<task>.<condition>.<repeat>`.
    pub run: String,
    pub task: String,
    pub kind: TaskKind,
    pub condition: Condition,
    pub repeat: u32,
    pub status: RunStatus,
    /// 1 = the check passed.
    pub score: u8,
    pub model: Option<String>,
    /// Uncached input.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// The CLI's `total_cost_usd`; `None` when it never reported (killed).
    pub cost_usd: Option<f64>,
    pub num_turns: Option<u32>,
    /// Wall time of the agent process, warm-up excluded.
    pub duration_s: f64,
    /// The CLI's own `duration_ms`.
    pub agent_duration_ms: Option<u64>,
    /// Calls by tool name (`Grep`, `Read`, `mcp__code-tools__find_symbol`).
    pub tool_calls: BTreeMap<String, u32>,
    /// `on` only: seconds the worktree took to index (and its language
    /// servers to be ready) before the agent started.
    pub warm_s: Option<f64>,
    /// `on` only: everything settled before the warm-up timeout.
    pub warm_ready: Option<bool>,
    pub final_text: String,
    /// The check command's output, tail.
    pub check_output: Option<String>,
    pub error: Option<String>,
    /// `git diff --stat` of the worktree after the run (edit tasks).
    pub diff_stat: Option<String>,
}

impl RunRecord {
    /// Every token the run sent or got, cache included.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.cache_creation_tokens + self.cache_read_tokens + self.output_tokens
    }
}

/// Median and mean of a sample; `None` when it is empty.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Stat {
    pub median: f64,
    pub mean: f64,
    pub n: usize,
}

pub fn stat(values: impl IntoIterator<Item = f64>) -> Option<Stat> {
    let mut v: Vec<f64> = values.into_iter().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    };
    Some(Stat {
        median,
        mean: v.iter().sum::<f64>() / n as f64,
        n,
    })
}

/// One condition over a set of runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Group {
    pub runs: usize,
    pub passed: usize,
    /// Runs that didn't finish cleanly (error, timeout, setup).
    pub failed: usize,
    pub tokens: Option<Stat>,
    pub cost_usd: Option<Stat>,
    pub turns: Option<Stat>,
    pub duration_s: Option<Stat>,
    pub warm_s: Option<Stat>,
    /// Mean calls per run, by tool.
    pub tool_calls: BTreeMap<String, f64>,
}

impl Group {
    pub fn pass_rate(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            self.passed as f64 / self.runs as f64
        }
    }
}

pub fn group<'a>(runs: impl IntoIterator<Item = &'a RunRecord>) -> Group {
    let runs: Vec<&RunRecord> = runs.into_iter().collect();
    let mut calls: BTreeMap<String, u32> = BTreeMap::new();
    for r in &runs {
        for (tool, n) in &r.tool_calls {
            *calls.entry(tool.clone()).or_default() += n;
        }
    }
    let n = runs.len().max(1) as f64;
    Group {
        runs: runs.len(),
        passed: runs.iter().filter(|r| r.score > 0).count(),
        failed: runs.iter().filter(|r| r.status != RunStatus::Ok).count(),
        tokens: stat(runs.iter().map(|r| r.total_tokens() as f64)),
        cost_usd: stat(runs.iter().filter_map(|r| r.cost_usd)),
        turns: stat(runs.iter().filter_map(|r| r.num_turns.map(f64::from))),
        duration_s: stat(runs.iter().map(|r| r.duration_s)),
        warm_s: stat(runs.iter().filter_map(|r| r.warm_s)),
        tool_calls: calls
            .into_iter()
            .map(|(k, v)| (k, f64::from(v) / n))
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskSummary {
    pub task: String,
    pub by_condition: BTreeMap<Condition, Group>,
}

impl TaskSummary {
    /// on − off of a group's median, when both ran.
    pub fn delta(&self, f: impl Fn(&Group) -> Option<f64>) -> Option<f64> {
        let on = f(self.by_condition.get(&Condition::On)?)?;
        let off = f(self.by_condition.get(&Condition::Off)?)?;
        Some(on - off)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub runs: usize,
    /// Runs not started: the budget ran out.
    pub skipped: usize,
    pub cost_usd: f64,
    pub by_condition: BTreeMap<Condition, Group>,
    pub by_task: Vec<TaskSummary>,
}

/// Aggregates `records` (in run order); `skipped` runs never started.
pub fn summarize(records: &[RunRecord], skipped: usize) -> Summary {
    let mut conditions: Vec<Condition> = records.iter().map(|r| r.condition).collect();
    conditions.sort();
    conditions.dedup();
    let by_condition = conditions
        .iter()
        .map(|c| (*c, group(records.iter().filter(|r| r.condition == *c))))
        .collect();
    let mut tasks: Vec<&str> = Vec::new();
    for r in records {
        if !tasks.contains(&r.task.as_str()) {
            tasks.push(&r.task);
        }
    }
    let by_task = tasks
        .into_iter()
        .map(|t| TaskSummary {
            task: t.to_string(),
            by_condition: conditions
                .iter()
                .filter(|c| records.iter().any(|r| r.task == t && r.condition == **c))
                .map(|c| {
                    let runs = records.iter().filter(|r| r.task == t && r.condition == *c);
                    (*c, group(runs))
                })
                .collect(),
        })
        .collect();
    Summary {
        runs: records.len(),
        skipped,
        cost_usd: records.iter().filter_map(|r| r.cost_usd).sum(),
        by_condition,
        by_task,
    }
}

// ── markdown ────────────────────────────────────────────────────────────────

fn med(s: Option<Stat>) -> Option<f64> {
    s.map(|s| s.median)
}

fn num(v: Option<f64>, decimals: usize) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{v:.decimals$}"))
}

fn signed(v: Option<f64>, decimals: usize) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{v:+.decimals$}"))
}

fn tokens(v: Option<f64>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{:.1}k", v / 1000.0))
}

fn signed_tokens(v: Option<f64>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{:+.1}k", v / 1000.0))
}

/// `mcp__<server>__find_symbol` → `find_symbol`.
pub fn short_tool(name: &str) -> &str {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.split_once("__").map_or(rest, |(_, tool)| tool),
        None => name,
    }
}

/// The summary as markdown; `header`: lines above the tables.
pub fn markdown(s: &Summary, header: &[String]) -> String {
    let mut out = String::from("# Agent tools eval\n\n");
    for h in header {
        let _ = writeln!(out, "- {h}");
    }
    let _ = writeln!(
        out,
        "- runs: {}{}, cost ${:.2}\n",
        s.runs,
        if s.skipped > 0 {
            format!(" ({} skipped: budget)", s.skipped)
        } else {
            String::new()
        },
        s.cost_usd
    );

    out.push_str("## By condition\n\n");
    out.push_str(
        "| condition | runs | pass | failed | tokens med | tokens mean | cost med | cost mean | turns med | turns mean | time med (s) | time mean (s) | warm-up mean (s) |\n",
    );
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for (c, g) in &s.by_condition {
        let _ = writeln!(
            out,
            "| {} | {} | {}/{} ({:.0}%) | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.as_str(),
            g.runs,
            g.passed,
            g.runs,
            g.pass_rate() * 100.0,
            g.failed,
            tokens(med(g.tokens)),
            tokens(g.tokens.map(|t| t.mean)),
            num(med(g.cost_usd), 3),
            num(g.cost_usd.map(|t| t.mean), 3),
            num(med(g.turns), 1),
            num(g.turns.map(|t| t.mean), 1),
            num(med(g.duration_s), 1),
            num(g.duration_s.map(|t| t.mean), 1),
            num(g.warm_s.map(|t| t.mean), 1),
        );
    }

    let mut tools: Vec<&str> = s
        .by_condition
        .values()
        .flat_map(|g| g.tool_calls.keys().map(String::as_str))
        .collect();
    tools.sort();
    tools.dedup();
    if !tools.is_empty() {
        out.push_str("\n### Tool calls per run (mean)\n\n| tool |");
        for c in s.by_condition.keys() {
            let _ = write!(out, " {} |", c.as_str());
        }
        out.push_str("\n| --- |");
        for _ in s.by_condition.keys() {
            out.push_str(" ---: |");
        }
        out.push('\n');
        for t in tools {
            let _ = write!(out, "| {} |", short_tool(t));
            for g in s.by_condition.values() {
                let _ = write!(out, " {} |", num(g.tool_calls.get(t).copied(), 1));
            }
            out.push('\n');
        }
    }

    out.push_str("\n## By task (medians; Δ = on − off)\n\n");
    out.push_str(
        "| task | pass off | pass on | tokens off | tokens on | Δ tokens | Δ cost | Δ turns | Δ time (s) |\n",
    );
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for t in &s.by_task {
        let pass = |c: Condition| {
            t.by_condition
                .get(&c)
                .map_or_else(|| "–".into(), |g| format!("{}/{}", g.passed, g.runs))
        };
        let tok = |c: Condition| tokens(t.by_condition.get(&c).and_then(|g| med(g.tokens)));
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            t.task,
            pass(Condition::Off),
            pass(Condition::On),
            tok(Condition::Off),
            tok(Condition::On),
            signed_tokens(t.delta(|g| med(g.tokens))),
            signed(t.delta(|g| med(g.cost_usd)), 3),
            signed(t.delta(|g| med(g.turns)), 1),
            signed(t.delta(|g| med(g.duration_s)), 1),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(task: &str, c: Condition, score: u8, tokens: u64, cost: f64, turns: u32) -> RunRecord {
        RunRecord {
            run: format!("{task}.{}.0", c.as_str()),
            task: task.into(),
            kind: TaskKind::Answer,
            condition: c,
            repeat: 0,
            status: RunStatus::Ok,
            score,
            model: None,
            input_tokens: tokens / 2,
            output_tokens: tokens / 4,
            cache_creation_tokens: 0,
            cache_read_tokens: tokens / 4,
            cost_usd: Some(cost),
            num_turns: Some(turns),
            duration_s: f64::from(turns) * 10.0,
            agent_duration_ms: None,
            tool_calls: BTreeMap::new(),
            warm_s: (c == Condition::On).then_some(2.0),
            warm_ready: None,
            final_text: String::new(),
            check_output: None,
            error: None,
            diff_stat: None,
        }
    }

    #[test]
    fn stats_are_median_and_mean() {
        let s = stat([3.0, 1.0, 2.0]).unwrap();
        assert_eq!((s.median, s.mean, s.n), (2.0, 2.0, 3));
        let s = stat([1.0, 2.0, 3.0, 10.0]).unwrap();
        assert_eq!((s.median, s.mean), (2.5, 4.0));
        assert!(stat([]).is_none());
        assert!(stat([f64::NAN]).is_none());
    }

    #[test]
    fn summarizes_by_condition_and_task_with_deltas() {
        let mut off_a = rec("a", Condition::Off, 0, 1000, 0.10, 8);
        off_a.tool_calls = [("Grep".to_string(), 4), ("Read".to_string(), 6)].into();
        let mut on_a = rec("a", Condition::On, 1, 600, 0.06, 5);
        on_a.tool_calls = [
            ("Grep".to_string(), 1),
            ("mcp__code-tools__find_symbol".to_string(), 3),
        ]
        .into();
        let mut on_b = rec("b", Condition::On, 1, 400, 0.04, 3);
        on_b.status = RunStatus::Timeout;
        on_b.cost_usd = None;
        let off_b = rec("b", Condition::Off, 1, 800, 0.08, 6);
        let records = vec![off_a, on_a, off_b, on_b];

        let s = summarize(&records, 2);
        assert_eq!((s.runs, s.skipped), (4, 2));
        assert!((s.cost_usd - 0.24).abs() < 1e-9);

        let off = &s.by_condition[&Condition::Off];
        assert_eq!((off.runs, off.passed, off.failed), (2, 1, 0));
        assert_eq!(off.tokens.unwrap().median, 900.0);
        assert_eq!(off.tool_calls["Grep"], 2.0);
        assert!(off.warm_s.is_none());

        let on = &s.by_condition[&Condition::On];
        assert_eq!((on.runs, on.passed, on.failed), (2, 2, 1));
        assert_eq!(on.pass_rate(), 1.0);
        // The timed-out run reported no cost: left out, not counted as 0.
        assert_eq!(on.cost_usd.unwrap().n, 1);
        assert_eq!(on.tool_calls["mcp__code-tools__find_symbol"], 1.5);
        assert_eq!(on.warm_s.unwrap().mean, 2.0);

        assert_eq!(
            s.by_task
                .iter()
                .map(|t| t.task.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let a = &s.by_task[0];
        assert_eq!(a.delta(|g| med(g.tokens)), Some(-400.0));
        assert_eq!(a.delta(|g| med(g.turns)), Some(-3.0));

        let md = markdown(&s, &["model: haiku".into()]);
        assert!(md.contains("- model: haiku"), "{md}");
        assert!(md.contains("(2 skipped: budget)"), "{md}");
        assert!(md.contains("| on | 2 | 2/2 (100%) | 1 |"), "{md}");
        assert!(md.contains("| find_symbol |"), "{md}");
        assert!(
            md.contains("| a | 0/1 | 1/1 | 1.0k | 0.6k | -0.4k |"),
            "{md}"
        );
    }

    #[test]
    fn one_condition_has_no_deltas() {
        let s = summarize(&[rec("a", Condition::On, 1, 100, 0.01, 1)], 0);
        assert_eq!(s.by_task[0].delta(|g| med(g.tokens)), None);
        assert!(markdown(&s, &[]).contains("| a | – | 1/1 |"));
    }

    #[test]
    fn records_round_trip_as_json_lines() {
        let r = rec("a", Condition::On, 1, 100, 0.01, 1);
        let line = serde_json::to_string(&r).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<RunRecord>(&line).unwrap(), r);
        assert!(line.contains(r#""condition":"on""#));
    }

    #[test]
    fn tool_names_shorten_past_the_server() {
        assert_eq!(short_tool("mcp__code-tools__find_symbol"), "find_symbol");
        assert_eq!(short_tool("Grep"), "Grep");
    }
}
