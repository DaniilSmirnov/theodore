//! `theodore eval-tools`: do the code-intelligence tools help agents?
//! The same tasks with the tools (`on`) and without (`off`), compared on
//! pass rate, tokens, cost, turns, time and tool use
//! (docs/eval/agent-tools/README.md).
//!
//! ```text
//! tasks.yaml ─▶ validate ─▶ plan: task × repeat × condition (interleaved)
//!   per run: spent ≥ budget? ─▶ skip the rest
//!            bare clone (cache) ─▶ fresh worktree at rev
//!            on: open facade ─▶ warm (index, servers ready) ─▶ code-tools MCP
//!            claude -p (the app's flags) ─▶ stream-json ─▶ tokens, cost, turns, tools
//!            check: answer contains / regex, or a command in the worktree
//!            ─▶ runs.jsonl ─▶ worktree removed
//!   summary.md · summary.json
//! ```

pub mod stream;
pub mod summary;
pub mod tasks;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use theodore_agents::{AgentBinaryConfig, claude};
use theodore_domain::AgentId;
use theodore_ports::{AgentMode, AgentRequest, CodeIntelPort, McpEndpoint};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::code_tools_mcp;
use stream::StreamStats;
use summary::{Condition, RunRecord, RunStatus};
use tasks::{Check, Task, TaskKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    /// The tools served from this process over localhost HTTP, as the app
    /// serves `space-tools`: the facade warmed here stays warm (language
    /// servers included) for the agent.
    Http,
    /// `theodore code-tools-mcp` spawned by the agent over stdio: its own
    /// facade; the index is warm (shared cache), language servers start
    /// again inside the agent's time.
    Stdio,
}

#[derive(Debug, Clone, clap::Args)]
pub struct EvalToolsArgs {
    /// The task suite.
    #[arg(long, default_value = "docs/eval/agent-tools/tasks.yaml")]
    pub tasks: PathBuf,
    /// Only these task ids (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
    /// Runs per (task, condition).
    #[arg(long, default_value_t = 2)]
    pub repeat: u32,
    #[arg(long, value_enum, value_delimiter = ',', default_value = "off,on")]
    pub conditions: Vec<Condition>,
    #[arg(long, value_enum, default_value = "claude")]
    pub agent: crate::group::AgentChoice,
    /// Model alias or id passed to the agent CLI (`haiku`, `sonnet`, …).
    #[arg(long)]
    pub model: Option<String>,
    /// `--effort` for Claude Code (low … max).
    #[arg(long)]
    pub effort: Option<String>,
    /// No new run starts once the runs so far cost this much.
    #[arg(long, default_value_t = 30.0)]
    pub budget_usd: f64,
    /// Results directory. Default: `target/eval-tools/<UTC timestamp>`.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Print the plan; no clone, no worktree, no agent.
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long, value_enum, default_value = "http")]
    pub transport: Transport,
    /// How long `on` waits for indexing and language servers before the
    /// agent starts anyway.
    #[arg(long, default_value_t = 600)]
    pub warm_timeout_s: u64,
    /// Download language servers that are missing before warming (the app
    /// asks first; servers already on the machine are used either way).
    #[arg(long)]
    pub install_servers: bool,
    /// Load the code tools up front (`ENABLE_TOOL_SEARCH=false`) instead
    /// of behind Claude Code's ToolSearch, where an agent that already has
    /// Grep tends never to look for them.
    #[arg(long)]
    pub eager_tools: bool,
    /// Leave each run's worktree in place.
    #[arg(long)]
    pub keep_worktrees: bool,
    /// Repos, worktrees, shared cargo targets and the code-intel cache.
    /// Default: `<data dir>/eval-tools`.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
}

/// One planned run.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub task: usize,
    pub condition: Condition,
    pub repeat: u32,
    /// `<task>.<condition>.<repeat>`.
    pub id: String,
}

/// task × repeat × condition, the conditions interleaved so drift (API
/// load, caches) hits both alike.
pub fn plan(tasks: &[Task], conditions: &[Condition], repeat: u32) -> Vec<Planned> {
    let mut conds: Vec<Condition> = Vec::new();
    for c in conditions {
        if !conds.contains(c) {
            conds.push(*c);
        }
    }
    let mut out = Vec::new();
    for (i, t) in tasks.iter().enumerate() {
        for r in 0..repeat {
            for c in &conds {
                out.push(Planned {
                    task: i,
                    condition: *c,
                    repeat: r,
                    id: format!("{}.{}.{r}", t.id, c.as_str()),
                });
            }
        }
    }
    out
}

/// What the agent gets for a task under a condition: everything but the
/// code tools (and their system-prompt line) is the same in both.
pub fn agent_request(
    task: &Task,
    condition: Condition,
    cwd: &Path,
    mcp: Option<McpEndpoint>,
    args: &EvalToolsArgs,
    env: Vec<(String, String)>,
) -> AgentRequest {
    let code = match (condition, task.kind) {
        (Condition::Off, _) => theodore_app::CodeTools::Off,
        (Condition::On, TaskKind::Answer) => theodore_app::CodeTools::Read,
        (Condition::On, TaskKind::Edit) => theodore_app::CodeTools::Edit,
    };
    AgentRequest {
        agent: AgentId::Claude,
        prompt: task.prompt.clone(),
        system: theodore_app::code_prompt(code).map(str::to_string),
        cwd: cwd.to_path_buf(),
        // A review run's read-only tools, or a space's Edit mode.
        mode: match task.kind {
            TaskKind::Answer => AgentMode::ReadOnly,
            TaskKind::Edit => AgentMode::Edit,
        },
        mcp: if condition == Condition::On {
            mcp
        } else {
            None
        },
        model: args.model.clone(),
        effort: args.effort.clone(),
        stall_after_ms: task.timeout_s * 1000,
        env: {
            let mut env = env;
            if condition == Condition::On && args.eager_tools {
                env.push(("ENABLE_TOOL_SEARCH".to_string(), "false".to_string()));
            }
            env
        },
        user_mcp: false,
        attachments: vec![],
        resume: None,
        stream_text: false,
    }
}

/// The stdio `--mcp-config`: this binary's `code-tools-mcp` on `root`.
pub fn stdio_mcp_config(exe: &Path, root: &Path, edit: bool, data: &Path) -> serde_json::Value {
    let mut args = vec![
        "code-tools-mcp".to_string(),
        "--root".into(),
        root.display().to_string(),
        "--trust".into(),
        "trusted".into(),
        "--data-dir".into(),
        data.display().to_string(),
    ];
    if edit {
        args.push("--edit".into());
    }
    serde_json::json!({
        "mcpServers": {
            theodore_ports::CODE_TOOLS: {
                "type": "stdio",
                "command": exe.display().to_string(),
                "args": args,
            }
        }
    })
}

/// `YYYYMMDD-HHMMSS`, UTC.
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// A path-safe name for a repo source.
fn slug(source: &str) -> String {
    let s: String = source
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    let keep = s.len().saturating_sub(100);
    s[keep..].to_string()
}

/// The repo the tasks file lives in: local task repos and check scripts
/// are relative to it.
fn eval_root(tasks_file: &Path) -> PathBuf {
    let dir = tasks_file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| dir.to_path_buf())
}

/// The tasks file's directory relative to `root` (empty if outside it).
fn suite_dir(tasks_file: &Path, root: &Path) -> PathBuf {
    let dir = tasks_file
        .canonicalize()
        .ok()
        .and_then(|f| f.parent().map(Path::to_path_buf));
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    dir.and_then(|d| d.strip_prefix(&root).ok().map(Path::to_path_buf))
        .unwrap_or_default()
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {} in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A bare clone of `source` under `repos/` that has `rev`.
async fn ensure_repo(cache: &Path, source: &str, rev: &str) -> Result<PathBuf> {
    let dir = cache.join("repos").join(slug(source));
    if !dir.join("HEAD").exists() {
        tokio::fs::create_dir_all(cache.join("repos")).await?;
        eprintln!("  cloning {source}");
        let out = tokio::process::Command::new("git")
            .args(["clone", "--bare", "--quiet", source])
            .arg(&dir)
            .stdin(Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            bail!(
                "git clone {source}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let has = |dir: PathBuf| async move {
        git(&dir, &["cat-file", "-e", &format!("{rev}^{{commit}}")])
            .await
            .is_ok()
    };
    if !has(dir.clone()).await {
        // A sha the default clone didn't bring (another branch, a PR).
        let by_sha = git(&dir, &["fetch", "--quiet", "origin", rev]).await;
        if by_sha.is_err() || !has(dir.clone()).await {
            git(
                &dir,
                &["fetch", "--quiet", "origin", "+refs/heads/*:refs/heads/*"],
            )
            .await?;
        }
        if !has(dir.clone()).await {
            bail!("{source} has no commit {rev}");
        }
    }
    Ok(dir)
}

/// The last `max` chars of `s`.
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        format!("…{}", s.chars().skip(n - max).collect::<String>())
    }
}

/// Kills `pid`'s process group (the CLI and what it started).
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status();
    }
}

struct AgentOutcome {
    stats: StreamStats,
    timed_out: bool,
    wall_s: f64,
    stderr: String,
    exit_ok: bool,
}

/// `claude <args>` in `cwd` until it exits or `timeout`; every stdout line
/// goes to `transcript` too.
async fn run_agent(
    program: &Path,
    args: &[String],
    req: &AgentRequest,
    timeout: Duration,
    transcript: &Path,
) -> Result<AgentOutcome> {
    let mut env = theodore_agents::env::filtered_env();
    env.extend(req.env.iter().cloned());
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(&req.cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let started = Instant::now();
    let mut child = cmd.spawn().context("starting claude")?;
    let pid = child.id();
    let stdout = child.stdout.take().context("no stdout")?;
    let mut stderr = child.stderr.take().context("no stderr")?;
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let mut file = tokio::fs::File::create(transcript).await?;
    let mut stats = StreamStats::default();
    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            file.write_all(line.as_bytes()).await?;
            file.write_all(b"\n").await?;
            stats.on_line(&line);
        }
        anyhow::Ok(())
    };
    let timed_out = tokio::time::timeout(timeout, read).await.is_err();
    if timed_out {
        kill_group(pid);
        let _ = child.start_kill();
    }
    let status = child.wait().await.ok();
    let _ = file.flush().await;
    let stderr = tokio::time::timeout(Duration::from_secs(5), err_task)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Ok(AgentOutcome {
        stats,
        timed_out,
        wall_s: started.elapsed().as_secs_f64(),
        stderr,
        exit_ok: status.is_some_and(|s| s.success()),
    })
}

/// `bash -c <run>` in the worktree; (passed, output tail).
#[allow(clippy::too_many_arguments)]
async fn run_check(
    run: &str,
    worktree: &Path,
    root: &Path,
    suite_dir: &Path,
    answer_file: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> (bool, String) {
    let command = tasks::resolve_command(run, worktree, root, suite_dir);
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(&command)
        .current_dir(worktree)
        .envs(env.iter().cloned())
        .env("THEODORE_EVAL_WORKTREE", worktree)
        .env("THEODORE_EVAL_ROOT", root)
        .env("THEODORE_EVAL_ANSWER_FILE", answer_file)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => (
            false,
            format!("check timed out after {}s", timeout.as_secs()),
        ),
        Ok(Err(e)) => (false, format!("check didn't start: {e}")),
        Ok(Ok(out)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (out.status.success(), tail(&text, 4000))
        }
    }
}

/// Shared by every run of one invocation.
struct Ctx<'a> {
    args: &'a EvalToolsArgs,
    cache: PathBuf,
    root: PathBuf,
    /// The tasks file's directory, relative to `root`.
    suite_dir: PathBuf,
    out: PathBuf,
    program: PathBuf,
    exe: PathBuf,
    code_dir: PathBuf,
    ci: Option<theodore_code_intel::CodeIntel>,
    stamp: String,
}

fn empty_record(task: &Task, p: &Planned, model: Option<String>) -> RunRecord {
    RunRecord {
        run: p.id.clone(),
        task: task.id.clone(),
        kind: task.kind,
        condition: p.condition,
        repeat: p.repeat,
        status: RunStatus::SetupError,
        score: 0,
        model,
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        cost_usd: None,
        num_turns: None,
        duration_s: 0.0,
        agent_duration_ms: None,
        tool_calls: Default::default(),
        warm_s: None,
        warm_ready: None,
        final_text: String::new(),
        check_output: None,
        error: None,
        diff_stat: None,
    }
}

/// One run, start to end; setup failures become the record's error.
async fn one_run(ctx: &Ctx<'_>, task: &Task, p: &Planned) -> RunRecord {
    let mut rec = empty_record(task, p, ctx.args.model.clone());
    let repo = match ensure_repo(&ctx.cache, &task.repo_source(&ctx.root), &task.rev).await {
        Ok(r) => r,
        Err(e) => {
            rec.error = Some(format!("{e:#}"));
            return rec;
        }
    };
    let wt = ctx.cache.join("worktrees").join(&ctx.stamp).join(&p.id);
    let wt_s = wt.display().to_string();
    if let Err(e) = git(
        &repo,
        &["worktree", "add", "--detach", "--force", &wt_s, &task.rev],
    )
    .await
    {
        rec.error = Some(format!("{e:#}"));
        return rec;
    }
    let body = run_in_worktree(ctx, task, p, &wt, &mut rec).await;
    if let Err(e) = body {
        rec.status = RunStatus::SetupError;
        rec.error = Some(format!("{e:#}"));
    }
    if !ctx.args.keep_worktrees {
        let _ = git(&repo, &["worktree", "remove", "--force", &wt_s]).await;
        let _ = git(&repo, &["worktree", "prune"]).await;
    }
    rec
}

async fn run_in_worktree(
    ctx: &Ctx<'_>,
    task: &Task,
    p: &Planned,
    wt: &Path,
    rec: &mut RunRecord,
) -> Result<()> {
    let wt = wt.canonicalize()?;
    let target = ctx.cache.join("target").join(&task.id);
    let env = vec![("CARGO_TARGET_DIR".to_string(), target.display().to_string())];
    let edit = task.kind == TaskKind::Edit;

    // `on`: the facade warmed before the clock starts.
    let mut http: Option<theodore_mcp::McpHandle> = None;
    let mut mcp: Option<McpEndpoint> = None;
    let mut stdio_cfg: Option<tempfile::NamedTempFile> = None;
    let mut opened: Option<PathBuf> = None;
    if p.condition == Condition::On {
        let ci = ctx.ci.as_ref().context("no code intelligence")?;
        let root = code_tools_mcp::open_root(ci, &wt, theodore_ports::TrustMode::Trusted).await?;
        opened = Some(root.clone());
        let warm = code_tools_mcp::warm(
            ci,
            &root,
            Duration::from_secs(ctx.args.warm_timeout_s),
            ctx.args.install_servers,
        )
        .await?;
        eprintln!(
            "  warm {:.1}s{}: {}",
            warm.secs,
            if warm.ready { "" } else { " (timed out)" },
            warm.languages
                .iter()
                .filter(|l| l.files > 0)
                .map(|l| format!(
                    "{} {}%{}",
                    l.language,
                    l.index_percent,
                    l.server_state
                        .as_ref()
                        .map(|s| format!(" {}:{s}", l.server.clone().unwrap_or_default()))
                        .unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        rec.warm_s = Some(warm.secs);
        rec.warm_ready = Some(warm.ready);
        match ctx.args.transport {
            Transport::Http => {
                let handle = theodore_mcp::McpServer::start_with_code(
                    std::sync::Arc::new(theodore_mcp::NoReviewTools),
                    std::sync::Arc::new(theodore_mcp::NoSpaceTools),
                    code_tools_mcp::backend(ci, root.clone()),
                )
                .await?;
                mcp = Some(handle.register_run(theodore_mcp::code_run(AgentId::Claude, edit)));
                http = Some(handle);
            }
            Transport::Stdio => {
                // The server opens its own facade on the warm cache.
                ci.close(&root).await;
                opened = None;
                let mut f = tempfile::Builder::new()
                    .prefix("theodore-eval-mcp-")
                    .suffix(".json")
                    .tempfile()?;
                let cfg = stdio_mcp_config(&ctx.exe, &wt, edit, &ctx.code_dir);
                std::io::Write::write_all(&mut f, cfg.to_string().as_bytes())?;
                stdio_cfg = Some(f);
                mcp = Some(McpEndpoint {
                    url: String::new(),
                    token: String::new(),
                    name: theodore_ports::CODE_TOOLS.into(),
                });
            }
        }
    }

    let req = agent_request(task, p.condition, &wt, mcp, ctx.args, env.clone());
    // HTTP: the app's own config writer (bearer token in a 0600 file).
    let http_cfg = if http.is_some() {
        claude::write_mcp_config(&req)?
    } else {
        None
    };
    let cfg_path = http_cfg
        .as_ref()
        .or(stdio_cfg.as_ref())
        .map(|f| f.path().to_path_buf());
    let argv = claude::build_args(&req, &AgentBinaryConfig::default(), cfg_path.as_deref());
    let transcript = ctx.out.join("transcripts").join(format!("{}.jsonl", p.id));
    let outcome = run_agent(
        &ctx.program,
        &argv,
        &req,
        Duration::from_secs(task.timeout_s),
        &transcript,
    )
    .await;

    if let Some(h) = http.take() {
        h.shutdown().await;
    }
    if let (Some(ci), Some(root)) = (ctx.ci.as_ref(), opened.take()) {
        ci.close(&root).await;
    }
    let o = outcome?;

    let s = &o.stats;
    rec.input_tokens = s.input_tokens;
    rec.output_tokens = s.output_tokens;
    rec.cache_creation_tokens = s.cache_creation_tokens;
    rec.cache_read_tokens = s.cache_read_tokens;
    if !s.finished {
        // Killed or crashed: what its messages showed, uncached split unknown.
        rec.input_tokens = s.partial_tokens;
    }
    rec.cost_usd = s.cost_usd;
    rec.num_turns = s.num_turns;
    rec.duration_s = o.wall_s;
    rec.agent_duration_ms = s.duration_ms;
    rec.tool_calls = s.tool_calls.clone();
    rec.final_text = s.final_text.clone();
    rec.status = if o.timed_out {
        RunStatus::Timeout
    } else if !s.finished || s.is_error {
        RunStatus::AgentError
    } else {
        RunStatus::Ok
    };
    if rec.status != RunStatus::Ok {
        let why = if o.timed_out {
            format!("timed out after {}s", task.timeout_s)
        } else if s.is_error {
            s.final_text.clone()
        } else {
            format!("claude exited (ok: {}) without a result", o.exit_ok)
        };
        rec.error = Some(
            format!("{why}\n{}", tail(&o.stderr, 2000))
                .trim()
                .to_string(),
        );
    }

    if edit {
        let stat = git(&wt, &["diff", "--stat", "HEAD"])
            .await
            .unwrap_or_default();
        let untracked = git(&wt, &["ls-files", "--others", "--exclude-standard"])
            .await
            .unwrap_or_default();
        let mut d = stat.trim_end().to_string();
        for f in untracked.lines() {
            d.push_str(&format!("\n new: {f}"));
        }
        rec.diff_stat = Some(d.trim().to_string());
    }

    if rec.status == RunStatus::Ok {
        rec.score = match &task.check {
            Check::Command { run } => {
                let answer = ctx.out.join("answers").join(format!("{}.txt", p.id));
                tokio::fs::write(&answer, &rec.final_text).await?;
                let (ok, out) = run_check(
                    run,
                    &wt,
                    &ctx.root,
                    &ctx.suite_dir,
                    &answer,
                    &env,
                    Duration::from_secs(task.timeout_s),
                )
                .await;
                rec.check_output = Some(out);
                u8::from(ok)
            }
            check => u8::from(tasks::score_answer(check, &rec.final_text).unwrap_or(false)),
        };
    }
    Ok(())
}

/// Prints the plan (`--dry-run`) or runs it.
pub async fn run(args: &EvalToolsArgs) -> Result<()> {
    if AgentId::from(args.agent) != AgentId::Claude {
        bail!("eval-tools runs Claude Code only for now (its stream-json carries the usage)");
    }
    if args.repeat == 0 {
        bail!("--repeat must be at least 1");
    }
    let all = tasks::load(&args.tasks)?;
    let tasks = tasks::select(all, &args.only)?;
    let planned = plan(&tasks, &args.conditions, args.repeat);
    let root = eval_root(&args.tasks);
    let cache = args
        .cache_dir
        .clone()
        .unwrap_or_else(code_tools_mcp::eval_tools_dir);
    let stamp = timestamp();
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| root.join("target").join("eval-tools").join(&stamp));

    if args.dry_run {
        print!("{}", dry_run(args, &tasks, &planned, &root, &cache, &out));
        return Ok(());
    }

    let program = theodore_agents::detect::locate_binary(AgentId::Claude, &Default::default())
        .context("claude is not installed")?;
    let version = theodore_agents::detect::read_version(&program).await;
    for d in ["transcripts", "answers"] {
        tokio::fs::create_dir_all(out.join(d)).await?;
    }
    let code_dir = cache.join("code-intel");
    let ci = if planned.iter().any(|p| p.condition == Condition::On) {
        Some(code_tools_mcp::facade(&code_dir)?)
    } else {
        None
    };
    let ctx = Ctx {
        args,
        cache: cache.clone(),
        root: root.clone(),
        suite_dir: suite_dir(&args.tasks, &root),
        out: out.clone(),
        program,
        exe: std::env::current_exe()?,
        code_dir,
        ci,
        stamp,
    };

    eprintln!(
        "eval-tools: {} runs ({} tasks × {} × {:?}), budget ${:.2}, out {}",
        planned.len(),
        tasks.len(),
        args.repeat,
        args.conditions,
        args.budget_usd,
        out.display()
    );
    let mut jsonl = tokio::fs::File::create(out.join("runs.jsonl")).await?;
    let mut records: Vec<RunRecord> = Vec::new();
    let mut spent = 0.0;
    let mut skipped = 0;
    for (i, p) in planned.iter().enumerate() {
        if spent >= args.budget_usd {
            skipped = planned.len() - i;
            eprintln!(
                "budget reached (${spent:.2} of ${:.2}): {skipped} runs not started",
                args.budget_usd
            );
            break;
        }
        let task = &tasks[p.task];
        eprintln!("[{}/{}] {}", i + 1, planned.len(), p.id);
        let rec = one_run(&ctx, task, p).await;
        spent += rec.cost_usd.unwrap_or(0.0);
        eprintln!(
            "  {:?} score {} · {:.1}k tokens · ${:.4} · {} turns · {:.1}s{}",
            rec.status,
            rec.score,
            rec.total_tokens() as f64 / 1000.0,
            rec.cost_usd.unwrap_or(0.0),
            rec.num_turns.unwrap_or(0),
            rec.duration_s,
            rec.error
                .as_ref()
                .map(|e| format!(" · {}", e.lines().next().unwrap_or_default()))
                .unwrap_or_default()
        );
        jsonl
            .write_all(format!("{}\n", serde_json::to_string(&rec)?).as_bytes())
            .await?;
        jsonl.flush().await?;
        records.push(rec);
    }
    if let Some(ci) = &ctx.ci {
        ci.shutdown().await;
    }
    // Only if every worktree in it is gone.
    let _ = std::fs::remove_dir(ctx.cache.join("worktrees").join(&ctx.stamp));

    let summary = summary::summarize(&records, skipped);
    let header = vec![
        format!(
            "agent: claude {}",
            version.unwrap_or_else(|| "(version unknown)".into())
        ),
        format!(
            "model: {}, effort: {}",
            args.model.as_deref().unwrap_or("(CLI default)"),
            args.effort.as_deref().unwrap_or("(CLI default)")
        ),
        format!(
            "tasks: {} ({}), repeat {}, transport {:?}",
            tasks.len(),
            args.tasks.display(),
            args.repeat,
            args.transport
        ),
    ];
    let md = summary::markdown(&summary, &header);
    tokio::fs::write(out.join("summary.md"), &md).await?;
    tokio::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )
    .await?;
    print!("{md}");
    eprintln!("results in {}", out.display());
    Ok(())
}

/// The plan, the paths and the agent's flags per condition, without
/// touching git, the network or an agent.
pub fn dry_run(
    args: &EvalToolsArgs,
    tasks: &[Task],
    planned: &[Planned],
    root: &Path,
    cache: &Path,
    out: &Path,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "dry run: {} runs, {} tasks × {} repeat(s) × {:?}\n",
        planned.len(),
        tasks.len(),
        args.repeat,
        args.conditions
    ));
    s.push_str(&format!(
        "model: {}, effort: {}, budget ${:.2}, transport {:?}\n",
        args.model.as_deref().unwrap_or("(CLI default)"),
        args.effort.as_deref().unwrap_or("(CLI default)"),
        args.budget_usd,
        args.transport
    ));
    s.push_str(&format!(
        "suite root: {}\ncache: {}\nout: {}\n\ntasks:\n",
        root.display(),
        cache.display(),
        out.display()
    ));
    for t in tasks {
        s.push_str(&format!(
            "  {} [{}] {} @ {} · timeout {}s · check {}\n",
            t.id,
            match t.kind {
                TaskKind::Answer => "answer",
                TaskKind::Edit => "edit",
            },
            t.repo_source(root),
            &t.rev[..12],
            t.timeout_s,
            match &t.check {
                Check::Contains { value } => format!("contains {value:?}"),
                Check::Regex { value } => format!("regex {value:?}"),
                Check::Command { run } => format!("command {run:?}"),
            }
        ));
    }
    s.push_str("\nruns, in order:\n");
    for p in planned {
        s.push_str(&format!("  {}\n", p.id));
    }
    if let Some(t) = tasks.first() {
        s.push_str(&format!("\nclaude flags for {}:\n", t.id));
        let wt = cache.join("worktrees").join("<stamp>").join(&t.id);
        for c in &args.conditions {
            let mcp = (*c == Condition::On).then(|| McpEndpoint {
                url: "http://127.0.0.1:<port>/mcp".into(),
                token: "<token>".into(),
                name: theodore_ports::CODE_TOOLS.into(),
            });
            let req = agent_request(t, *c, &wt, mcp, args, vec![]);
            let cfg = (*c == Condition::On).then(|| PathBuf::from("<mcp-config.json>"));
            let mut argv = claude::build_args(&req, &AgentBinaryConfig::default(), cfg.as_deref());
            // The prompt is the task's, shown above.
            if argv.len() > 1 {
                argv[1] = "<prompt>".into();
            }
            s.push_str(&format!("  {}: claude {}\n", c.as_str(), argv.join(" ")));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, kind: TaskKind) -> Task {
        Task {
            id: id.into(),
            repo: ".".into(),
            rev: "0123456789abcdef0123456789abcdef01234567".into(),
            kind,
            prompt: "where is save defined?".into(),
            check: match kind {
                TaskKind::Answer => Check::Contains {
                    value: "store.rs".into(),
                },
                TaskKind::Edit => Check::Command { run: "true".into() },
            },
            timeout_s: 60,
        }
    }

    fn args() -> EvalToolsArgs {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            a: EvalToolsArgs,
        }
        Cli::parse_from(["x", "--model", "haiku"]).a
    }

    #[test]
    fn plans_interleaved_conditions_per_repeat() {
        let tasks = [task("a", TaskKind::Answer), task("b", TaskKind::Edit)];
        let ids: Vec<String> = plan(&tasks, &[Condition::Off, Condition::On, Condition::Off], 2)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(
            ids,
            [
                "a.off.0", "a.on.0", "a.off.1", "a.on.1", "b.off.0", "b.on.0", "b.off.1", "b.on.1"
            ]
        );
    }

    #[test]
    fn defaults_match_the_documented_command() {
        let a = args();
        assert_eq!(a.tasks, PathBuf::from("docs/eval/agent-tools/tasks.yaml"));
        assert_eq!(a.repeat, 2);
        assert_eq!(a.conditions, vec![Condition::Off, Condition::On]);
        assert_eq!(a.budget_usd, 30.0);
        assert_eq!(a.transport, Transport::Http);
    }

    #[test]
    fn conditions_differ_only_in_the_code_tools() {
        let a = args();
        let wt = Path::new("/wt");
        let ep = McpEndpoint {
            url: "http://127.0.0.1:1/mcp".into(),
            token: "t".into(),
            name: theodore_ports::CODE_TOOLS.into(),
        };
        for kind in [TaskKind::Answer, TaskKind::Edit] {
            let t = task("a", kind);
            let off = agent_request(&t, Condition::Off, wt, Some(ep.clone()), &a, vec![]);
            let on = agent_request(&t, Condition::On, wt, Some(ep.clone()), &a, vec![]);
            assert!(off.mcp.is_none() && off.system.is_none());
            assert_eq!(on.mcp.as_ref().unwrap().name, "code-tools");
            let line = on.system.clone().unwrap();
            assert!(line.contains("find_symbol"), "{line}");
            assert_eq!(line.contains("replace_symbol_body"), kind == TaskKind::Edit);
            assert_eq!((off.mode, off.model.clone()), (on.mode, on.model.clone()));
            assert_eq!(off.prompt, on.prompt);
            assert_eq!(
                on.mode,
                if kind == TaskKind::Edit {
                    AgentMode::Edit
                } else {
                    AgentMode::ReadOnly
                }
            );
            // Both keep Grep and Read; `on` pre-approves its server.
            let flags = |r: &AgentRequest| {
                let argv = claude::build_args(r, &AgentBinaryConfig::default(), None);
                let i = argv.iter().position(|x| x == "--allowedTools").unwrap();
                argv[i + 1].clone()
            };
            assert!(flags(&off).starts_with("Read,Grep,"));
            assert!(
                flags(&on).ends_with(",mcp__code-tools__*"),
                "{}",
                flags(&on)
            );
        }
    }

    #[test]
    fn dry_run_prints_the_plan_and_the_flags() {
        let a = args();
        let tasks = [task("a", TaskKind::Answer)];
        let planned = plan(&tasks, &a.conditions, a.repeat);
        let s = dry_run(
            &a,
            &tasks,
            &planned,
            Path::new("/suite"),
            Path::new("/cache"),
            Path::new("/out"),
        );
        assert!(s.contains("dry run: 4 runs, 1 tasks × 2 repeat(s)"), "{s}");
        assert!(s.contains("a [answer] /suite @ 0123456789ab"), "{s}");
        assert!(s.contains("  a.on.1\n"), "{s}");
        assert!(s.contains("off: claude -p <prompt>"), "{s}");
        assert!(s.contains("--mcp-config <mcp-config.json>"), "{s}");
        assert!(s.contains("--model haiku"), "{s}");
    }

    #[test]
    fn stdio_config_runs_this_binary_on_the_worktree() {
        let cfg = stdio_mcp_config(
            Path::new("/bin/theodore"),
            Path::new("/wt"),
            true,
            Path::new("/ci"),
        );
        let s = &cfg["mcpServers"]["code-tools"];
        assert_eq!(s["command"], "/bin/theodore");
        let args: Vec<&str> = s["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "code-tools-mcp",
                "--root",
                "/wt",
                "--trust",
                "trusted",
                "--data-dir",
                "/ci",
                "--edit"
            ]
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(
            slug("https://github.com/JeanIsahakyan/theodore.git"),
            "https---github-com-JeanIsahakyan-theodore"
        );
        let ts = timestamp();
        assert_eq!(ts.len(), 15, "{ts}");
        assert!(ts.starts_with("20"), "{ts}");
        assert_eq!(tail("abcdef", 3), "…def");
        assert_eq!(tail("ab", 3), "ab");
    }
}
