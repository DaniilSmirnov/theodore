//! `CommandPort` over `sh -c` (docs/config.md): a space's setup, run,
//! check and archive commands. Same process discipline as the agent CLIs —
//! own process group, SIGTERM → grace → SIGKILL to the whole group on
//! cancel or timeout, Theodore's own `THEODORE_*` secrets stripped from the
//! inherited environment — plus a cap on each output line.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use theodore_ports::command::{CommandEvent, CommandExit, CommandPort, CommandRun, CommandSpec};
use tokio::io::{AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::env;

/// A line longer than this is cut (the rest of it is dropped, never
/// buffered): a command printing megabytes without a newline must not grow
/// this process's memory.
pub const MAX_LINE_BYTES: usize = 8 * 1024;
const CHANNEL_CAPACITY: usize = 1024;

/// Runs commands with `sh -c`.
#[derive(Debug, Clone)]
pub struct ShellRunner {
    shell: String,
    grace: Duration,
}

impl Default for ShellRunner {
    fn default() -> Self {
        Self {
            shell: "/bin/sh".into(),
            grace: Duration::from_secs(5),
        }
    }
}

impl ShellRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long a cancelled or timed-out command gets after SIGTERM before
    /// SIGKILL (tests shorten it).
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }
}

#[async_trait]
impl CommandPort for ShellRunner {
    async fn spawn(&self, spec: CommandSpec) -> CommandRun {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let cancel = CancellationToken::new();
        let mut cmd = Command::new(&self.shell);
        cmd.arg("-c")
            .arg(&spec.script)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(env::filtered_env())
            .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let pid = match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                tokio::spawn(supervise(
                    child,
                    tx,
                    cancel.clone(),
                    spec.timeout,
                    self.grace,
                ));
                pid
            }
            Err(e) => {
                let _ = tx.try_send(CommandEvent::Exit(CommandExit {
                    error: Some(format!("could not start the command: {e}")),
                    ..CommandExit::default()
                }));
                None
            }
        };
        CommandRun {
            events: rx,
            cancel,
            pid,
        }
    }

    async fn group_matches(&self, pgid: u32, script: &str) -> bool {
        let out = Command::new("ps")
            .args(["-ww", "-o", "command=", "-p", &pgid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).contains(script),
            _ => false,
        }
    }

    async fn reap_group(&self, pgid: u32, grace: Duration) {
        terminate_group(pgid, grace).await;
    }
}

/// SIGTERM a process group, `grace` to exit, then SIGKILL — the discipline
/// every command and the space terminal's PTY (`pty.rs`) kill a group with.
pub(crate) async fn terminate_group(pgid: u32, grace: Duration) {
    signal_group(pgid, "-TERM").await;
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !group_alive(pgid).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    signal_group(pgid, "-KILL").await;
}

/// Whether a process (any process, not necessarily ours) exists at `pid`.
pub(crate) async fn group_alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn supervise(
    mut child: Child,
    tx: mpsc::Sender<CommandEvent>,
    cancel: CancellationToken,
    timeout: Option<Duration>,
    grace: Duration,
) {
    let out = child.stdout.take().map(|s| pump(s, false, tx.clone()));
    let err = child.stderr.take().map(|s| pump(s, true, tx.clone()));
    let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
    let mut exit = CommandExit::default();
    tokio::select! {
        status = child.wait() => {
            exit.code = status.ok().and_then(|s| s.code());
        }
        _ = cancel.cancelled() => {
            exit.cancelled = true;
            terminate(&mut child, grace).await;
        }
        _ = sleep_until(deadline) => {
            exit.timed_out = true;
            terminate(&mut child, grace).await;
        }
    }
    // The pipes close once every process holding them is gone; a
    // grandchild that escaped the group signal mustn't hold the exit back.
    for task in [out, err].into_iter().flatten() {
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }
    let _ = tx.send(CommandEvent::Exit(exit)).await;
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// SIGTERM to the group, `grace` to exit, then SIGKILL to the group.
async fn terminate(child: &mut Child, grace: Duration) {
    if let Some(pid) = child.id() {
        signal_group(pid, "-TERM").await;
        if tokio::time::timeout(grace, child.wait()).await.is_ok() {
            // The leader is gone; anything it left in the group goes too.
            signal_group(pid, "-KILL").await;
            return;
        }
        signal_group(pid, "-KILL").await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

pub(crate) async fn signal_group(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg(signal)
        .arg(format!("-{pid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    stderr: bool,
    tx: mpsc::Sender<CommandEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        while let Ok(Some((text, _))) = read_line_capped(&mut reader, MAX_LINE_BYTES).await {
            if tx.send(CommandEvent::Line { stderr, text }).await.is_err() {
                return;
            }
        }
    })
}

/// One `\n`-terminated line, at most `max` bytes of it kept (the rest is
/// read and dropped). `None` at a clean end of input.
async fn read_line_capped<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<Option<(String, bool)>> {
    use tokio::io::AsyncBufReadExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut any = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            break;
        }
        any = true;
        let newline = available.iter().position(|&b| b == b'\n');
        let take = newline.map_or(available.len(), |p| p + 1);
        let content = &available[..newline.unwrap_or(available.len())];
        let room = max.saturating_sub(buf.len());
        let n = room.min(content.len());
        buf.extend_from_slice(&content[..n]);
        truncated |= content.len() > n;
        reader.consume(take);
        if newline.is_some() {
            break;
        }
    }
    if !any {
        return Ok(None);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        text.push('…');
    }
    Ok(Some((text, truncated)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(script: &str) -> CommandSpec {
        CommandSpec {
            script: script.into(),
            cwd: std::env::temp_dir(),
            env: vec![],
            timeout: Some(Duration::from_secs(20)),
        }
    }

    async fn collect(run: CommandRun) -> (Vec<(bool, String)>, CommandExit) {
        let mut events = run.events;
        let mut lines = Vec::new();
        while let Some(e) = events.recv().await {
            match e {
                CommandEvent::Line { stderr, text } => lines.push((stderr, text)),
                CommandEvent::Exit(x) => return (lines, x),
            }
        }
        panic!("no exit event");
    }

    #[tokio::test]
    async fn streams_both_outputs_and_the_exit_code() {
        let run = ShellRunner::new()
            .spawn(spec("echo out; echo err >&2; exit 3"))
            .await;
        let (lines, exit) = collect(run).await;
        assert!(lines.contains(&(false, "out".into())), "{lines:?}");
        assert!(lines.contains(&(true, "err".into())), "{lines:?}");
        assert_eq!(exit.code, Some(3));
        assert!(!exit.success());
    }

    #[tokio::test]
    async fn runs_in_cwd_with_the_extra_env_and_without_theodore_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = spec("pwd; echo \"$GREETING\"; echo \"[${THEODORE_HOST_TOKEN_X-unset}]\"");
        s.cwd = dir.path().to_path_buf();
        s.env = vec![("GREETING".into(), "hello space".into())];
        // SAFETY of the test: a variable only this test reads.
        let run = ShellRunner::new().spawn(s).await;
        let (lines, exit) = collect(run).await;
        assert!(exit.success(), "{exit:?}");
        let texts: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
        let canon = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            std::fs::canonicalize(texts[0]).unwrap(),
            canon,
            "cwd is the given directory"
        );
        assert_eq!(texts[1], "hello space");
        assert_eq!(texts[2], "[unset]");
    }

    #[tokio::test]
    async fn a_timeout_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let mut s = spec(&format!("(sleep 2; touch {}) & sleep 30", marker.display()));
        s.timeout = Some(Duration::from_millis(200));
        let started = std::time::Instant::now();
        let run = ShellRunner::new()
            .with_grace(Duration::from_millis(200))
            .spawn(s)
            .await;
        let (_, exit) = collect(run).await;
        assert!(exit.timed_out);
        assert!(!exit.success());
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "the background child was killed too");
    }

    #[tokio::test]
    async fn cancel_stops_a_long_running_command() {
        let mut s = spec("echo started; sleep 30");
        s.timeout = None;
        let run = ShellRunner::new()
            .with_grace(Duration::from_millis(200))
            .spawn(s)
            .await;
        let cancel = run.cancel.clone();
        let mut events = run.events;
        match events.recv().await {
            Some(CommandEvent::Line { text, .. }) => assert_eq!(text, "started"),
            other => panic!("{other:?}"),
        }
        cancel.cancel();
        let exit = loop {
            match events.recv().await {
                Some(CommandEvent::Exit(x)) => break x,
                Some(_) => {}
                None => panic!("no exit"),
            }
        };
        assert!(exit.cancelled);
    }

    #[tokio::test]
    async fn long_lines_are_capped() {
        let run = ShellRunner::new()
            .spawn(spec(&format!(
                "head -c {} /dev/zero | tr '\\0' a; echo; echo next",
                MAX_LINE_BYTES * 4
            )))
            .await;
        let (lines, exit) = collect(run).await;
        assert!(exit.success());
        assert_eq!(lines.len(), 2, "the overflow is dropped, not split");
        assert!(lines[0].1.len() <= MAX_LINE_BYTES + '…'.len_utf8());
        assert!(lines[0].1.ends_with('…'));
        assert_eq!(lines[1].1, "next");
    }

    #[tokio::test]
    async fn a_missing_cwd_is_an_exit_with_an_error() {
        let mut s = spec("true");
        s.cwd = "/nonexistent/theodore/space".into();
        let (_, exit) = collect(ShellRunner::new().spawn(s).await).await;
        assert!(exit.error.is_some());
        assert!(!exit.success());
    }

    // ── startup reaping (docs/spaces.md "Crash recovery") ──────────────

    #[tokio::test]
    async fn a_live_group_whose_command_matches_is_reaped() {
        let runner = ShellRunner::new();
        let run = runner.spawn(spec("sleep 30")).await;
        let pgid = run.pid.expect("a real pid");
        assert!(group_alive(pgid).await, "just started");
        assert!(runner.group_matches(pgid, "sleep 30").await);

        runner.reap_group(pgid, Duration::from_millis(300)).await;
        assert!(!group_alive(pgid).await, "the group was killed");
    }

    #[tokio::test]
    async fn a_command_that_no_longer_matches_is_never_touched() {
        let runner = ShellRunner::new();
        let run = runner.spawn(spec("sleep 30")).await;
        let pgid = run.pid.expect("a real pid");

        // A pid reuse would show a different command line; never a match.
        assert!(!runner.group_matches(pgid, "totally different script").await);
        assert!(
            group_alive(pgid).await,
            "group_matches must not have touched it"
        );
        signal_group(pgid, "-KILL").await;
    }

    #[tokio::test]
    async fn a_pid_with_nothing_running_at_it_never_matches() {
        let runner = ShellRunner::new();
        let run = runner.spawn(spec("true")).await;
        let pgid = run.pid.expect("a real pid");
        wait_for_death(pgid).await;
        assert!(!runner.group_matches(pgid, "true").await);
    }

    async fn wait_for_death(pid: u32) {
        for _ in 0..100 {
            if !group_alive(pid).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("pid {pid} never exited");
    }
}
