//! A space's terminals (docs/spaces.md §3 "Terminal"): interactive PTYs in
//! the space's worktree, independent of the agent's turns and the
//! `setup`/`run`/`check`/`archive` commands (`space_commands.rs`) — several
//! per space, surviving navigation away from the space page, in memory
//! only (`Core::term_rt`, same lifetime story as `space_rt`).
//!
//! ```text
//! open ─▶ PtyPort::spawn ─▶ TerminalEntry in term_rt ─▶ forward_events task
//!                                                          │
//!   write / resize ──────────────────────────▶ the pty  ◀─┤
//!   close / archive / app quit ──▶ kill ──▶ Exit ─────────┘
//! ```
//!
//! Output is kept twice: a bounded raw tail (ANSI intact, base64 on the
//! wire) that seeds `xterm.js` when a tab is reattached, and a bounded
//! ANSI-stripped line tail (`space_terminal_tail`) for a later agent tool
//! to read as plain text — not wired up yet.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as SyncMutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use theodore_contract::{Ephemeral, SpaceStatus, SpaceTerminalInfo, TerminalStatus};
use theodore_ports::terminal::{TerminalEvent, TerminalSpec};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// Raw bytes kept per terminal, for replaying into `xterm.js` when a tab is
/// reattached (navigated back to, or a fresh page load while the shell is
/// still running).
const RAW_TAIL_BYTES: usize = 1024 * 1024;
/// ANSI-stripped lines kept per terminal, for `space_terminal_tail`.
const CLEAN_TAIL_LINES: usize = 2_000;
/// How long `close_space_terminal` and the app-quit sweep wait for a killed
/// shell to actually exit before giving up on it.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// One open terminal, keyed by its id in `Core::term_rt`.
pub(crate) struct TerminalEntry {
    space: String,
    title: String,
    shell: String,
    created_at: theodore_domain::Millis,
    input: mpsc::UnboundedSender<Vec<u8>>,
    resize_tx: mpsc::UnboundedSender<(u16, u16)>,
    kill: CancellationToken,
    /// Resolves once the forwarder task has seen the shell's `Exit` — the
    /// only reliable "it's actually dead" signal, since `kill.cancel()`
    /// itself just requests it.
    done: tokio::sync::oneshot::Receiver<()>,
    live: Arc<SyncMutex<LiveState>>,
}

/// Mutated by the forwarder task as output and the exit arrive; read by
/// `list`/`close` without touching the PTY itself.
struct LiveState {
    status: TerminalStatus,
    exit_code: Option<i32>,
    error: Option<String>,
    cols: u16,
    rows: u16,
    seq: u64,
    raw_tail: VecDeque<u8>,
    clean_tail: VecDeque<String>,
    /// The ANSI-stripped current line, not yet terminated by `\n`.
    clean_partial: String,
}

impl Core {
    /// Starts a new terminal in `space`'s worktree: the login shell, the
    /// space's env (same as its commands'), `cols`×`rows`. Refused once the
    /// space is archived (its worktree is gone).
    pub async fn open_space_terminal(
        &self,
        space_id: &str,
        cols: u16,
        rows: u16,
    ) -> AppResult<SpaceTerminalInfo> {
        let space = self.space_row(space_id).await?;
        if space.status == SpaceStatus::Archived {
            return Err(AppError::Invalid(
                "the space is archived; its worktree is gone".into(),
            ));
        }
        let project = self.space_project(&space).await?;
        let cfg = self.space_config_of(&space, &project).await;
        let env = self.space_command_env(&space, &project, &cfg);
        let shell = login_shell();
        let title = {
            let rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            let n = rt.values().filter(|t| t.space == space_id).count() + 1;
            format!("Terminal {n}")
        };

        let handle = self
            .0
            .deps
            .terminals
            .spawn(TerminalSpec {
                shell: shell.clone(),
                args: vec!["-l".into()],
                cwd: PathBuf::from(&space.worktree),
                env,
                cols,
                rows,
            })
            .await;

        let id = uuid::Uuid::new_v4().to_string();
        let created_at = self.0.deps.clock.now();
        let live = Arc::new(SyncMutex::new(LiveState {
            status: TerminalStatus::Running,
            exit_code: None,
            error: None,
            cols,
            rows,
            seq: 0,
            raw_tail: VecDeque::new(),
            clean_tail: VecDeque::new(),
            clean_partial: String::new(),
        }));
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        {
            let mut rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            rt.insert(
                id.clone(),
                TerminalEntry {
                    space: space_id.to_string(),
                    title: title.clone(),
                    shell: shell.clone(),
                    created_at,
                    input: handle.input,
                    resize_tx: handle.resize,
                    kill: handle.kill,
                    done: done_rx,
                    live: live.clone(),
                },
            );
        }
        tokio::spawn(forward_terminal_events(
            self.clone(),
            space_id.to_string(),
            id.clone(),
            handle.events,
            live,
            done_tx,
        ));

        Ok(SpaceTerminalInfo {
            id,
            space: space_id.to_string(),
            title,
            shell,
            status: TerminalStatus::Running,
            exit_code: None,
            error: None,
            cols,
            rows,
            created_at,
            seq: 0,
            scrollback: String::new(),
        })
    }

    /// Bytes typed or pasted into the terminal, forwarded to its shell as
    /// is. A no-op once the shell has exited.
    pub async fn write_space_terminal(&self, terminal: &str, data: &str) -> AppResult<()> {
        let input = {
            let rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            rt.get(terminal)
                .ok_or_else(|| AppError::NotFound(format!("terminal {terminal}")))?
                .input
                .clone()
        };
        let _ = input.send(data.as_bytes().to_vec());
        Ok(())
    }

    /// A new size in cells (`SIGWINCH` under the hood).
    pub async fn resize_space_terminal(
        &self,
        terminal: &str,
        cols: u16,
        rows: u16,
    ) -> AppResult<()> {
        let (resize_tx, live) = {
            let rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            let t = rt
                .get(terminal)
                .ok_or_else(|| AppError::NotFound(format!("terminal {terminal}")))?;
            (t.resize_tx.clone(), t.live.clone())
        };
        {
            let mut st = live.lock().unwrap_or_else(|p| p.into_inner());
            st.cols = cols;
            st.rows = rows;
        }
        let _ = resize_tx.send((cols, rows));
        Ok(())
    }

    /// Kills the shell's process group and waits briefly for it to exit.
    /// Removed from `term_rt` either way: a shell that won't die within
    /// `CLOSE_WAIT` is abandoned, not left to block the caller forever.
    pub async fn close_space_terminal(&self, terminal: &str) -> AppResult<()> {
        let (kill, done) = {
            let mut rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            let Some(t) = rt.remove(terminal) else {
                return Ok(()); // already gone
            };
            (t.kill, t.done)
        };
        kill.cancel();
        let _ = tokio::time::timeout(CLOSE_WAIT, done).await;
        Ok(())
    }

    /// Every open or just-exited terminal of `space`, most recently opened
    /// last, each with its scrollback to seed `xterm.js`.
    pub async fn list_space_terminals(&self, space: &str) -> AppResult<Vec<SpaceTerminalInfo>> {
        let rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<(theodore_domain::Millis, SpaceTerminalInfo)> = rt
            .iter()
            .filter(|(_, t)| t.space == space)
            .map(|(id, t)| (t.created_at, info_of(id, t)))
            .collect();
        out.sort_by_key(|(at, _)| *at);
        Ok(out.into_iter().map(|(_, info)| info).collect())
    }

    /// The last `lines` of `terminal`'s output, ANSI stripped — for a later
    /// agent tool to read as plain text (not wired up in this slice).
    pub async fn space_terminal_tail(
        &self,
        terminal: &str,
        lines: usize,
    ) -> AppResult<Vec<String>> {
        let live = {
            let rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            rt.get(terminal)
                .ok_or_else(|| AppError::NotFound(format!("terminal {terminal}")))?
                .live
                .clone()
        };
        let st = live.lock().unwrap_or_else(|p| p.into_inner());
        let n = st.clean_tail.len().saturating_sub(lines);
        Ok(st.clean_tail.iter().skip(n).cloned().collect())
    }

    /// Kills every terminal of `space`, waiting briefly for each — archive
    /// must not leave a shell writing into a worktree it's about to remove.
    pub(crate) async fn close_space_terminals(&self, space: &str) {
        let entries: Vec<(CancellationToken, tokio::sync::oneshot::Receiver<()>)> = {
            let mut rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            let ids: Vec<String> = rt
                .iter()
                .filter(|(_, t)| t.space == space)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| rt.remove(&id))
                .map(|t| (t.kill, t.done))
                .collect()
        };
        kill_and_wait_all(entries).await;
    }

    /// Kills every open terminal, everywhere — called on a clean app exit
    /// (alongside `stop_all_running_spaces`) so quitting normally never
    /// orphans a shell's process group.
    pub async fn stop_all_space_terminals(&self) {
        let entries: Vec<(CancellationToken, tokio::sync::oneshot::Receiver<()>)> = {
            let mut rt = self.0.term_rt.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut *rt)
                .into_values()
                .map(|t| (t.kill, t.done))
                .collect()
        };
        kill_and_wait_all(entries).await;
    }
}

/// Cancels every `kill` token, then waits for each one's forwarder to see
/// the shell's `Exit` — concurrently, so the wall time is bounded by
/// `CLOSE_WAIT` once, not once per terminal.
async fn kill_and_wait_all(entries: Vec<(CancellationToken, tokio::sync::oneshot::Receiver<()>)>) {
    let waits: Vec<_> = entries
        .into_iter()
        .map(|(kill, done)| {
            kill.cancel();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(CLOSE_WAIT, done).await;
            })
        })
        .collect();
    for w in waits {
        let _ = w.await;
    }
}

fn info_of(id: &str, t: &TerminalEntry) -> SpaceTerminalInfo {
    let st = t.live.lock().unwrap_or_else(|p| p.into_inner());
    SpaceTerminalInfo {
        id: id.to_string(),
        space: t.space.clone(),
        title: t.title.clone(),
        shell: t.shell.clone(),
        status: st.status,
        exit_code: st.exit_code,
        error: st.error.clone(),
        cols: st.cols,
        rows: st.rows,
        created_at: t.created_at,
        seq: st.seq,
        scrollback: STANDARD.encode(st.raw_tail.iter().copied().collect::<Vec<u8>>()),
    }
}

/// `$SHELL -l`; a bare `/bin/sh` if the environment doesn't say (never the
/// case for a real login session, but tests and odd setups shouldn't panic
/// over it).
fn login_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}

/// Appends `bytes` to a capped ring buffer, dropping from the front.
fn push_capped(buf: &mut VecDeque<u8>, bytes: &[u8], cap: usize) {
    buf.extend(bytes.iter().copied());
    while buf.len() > cap {
        buf.pop_front();
    }
}

/// Strips ANSI control sequences (CSI, OSC, and bare ESC-letter) so
/// `space_terminal_tail` reads as plain text.
fn strip_ansi(s: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])")
            .expect("static ANSI regex")
    });
    RE.replace_all(s, "").into_owned()
}

/// Feeds `bytes` into the clean (ANSI-stripped) line tail: carries a
/// partial last line across calls, since PTY output arrives in arbitrary
/// chunks, not line by line.
fn append_clean(tail: &mut VecDeque<String>, partial: &mut String, bytes: &[u8], cap: usize) {
    partial.push_str(&String::from_utf8_lossy(bytes));
    while let Some(pos) = partial.find('\n') {
        let line: String = partial.drain(..=pos).collect();
        let line = strip_ansi(line.trim_end_matches(['\n', '\r']));
        tail.push_back(line);
        while tail.len() > cap {
            tail.pop_front();
        }
    }
}

/// Forwards one terminal's events for its whole life: updates the tails and
/// `seq`, emits `Ephemeral::TerminalOutput` / `TerminalExit`. Ends when the
/// shell exits (always exactly one `Exit`) or the channel closes.
async fn forward_terminal_events(
    core: Core,
    space: String,
    terminal: String,
    mut events: mpsc::Receiver<TerminalEvent>,
    live: Arc<SyncMutex<LiveState>>,
    done: tokio::sync::oneshot::Sender<()>,
) {
    while let Some(ev) = events.recv().await {
        match ev {
            TerminalEvent::Output(bytes) => {
                let (seq, data) = {
                    let mut st = live.lock().unwrap_or_else(|p| p.into_inner());
                    st.seq += 1;
                    push_capped(&mut st.raw_tail, &bytes, RAW_TAIL_BYTES);
                    let st = &mut *st;
                    append_clean(
                        &mut st.clean_tail,
                        &mut st.clean_partial,
                        &bytes,
                        CLEAN_TAIL_LINES,
                    );
                    (st.seq, STANDARD.encode(&bytes))
                };
                core.emit(Ephemeral::TerminalOutput {
                    space: space.clone(),
                    terminal: terminal.clone(),
                    seq,
                    data,
                });
            }
            TerminalEvent::Exit(exit) => {
                {
                    let mut st = live.lock().unwrap_or_else(|p| p.into_inner());
                    st.status = TerminalStatus::Exited;
                    st.exit_code = exit.code;
                    st.error = exit.error.clone();
                }
                core.emit(Ephemeral::TerminalExit {
                    space,
                    terminal,
                    exit_code: exit.code,
                    error: exit.error,
                });
                break;
            }
        }
    }
    // Reached either via `Exit` above, or (never expected, but handled) the
    // channel closing without one — either way the shell is gone and
    // `close`/archive/shutdown waiters can stop blocking on it.
    let _ = done.send(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_common_ansi_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("\x1b]0;title\x07plain"), "plain");
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }

    #[test]
    fn append_clean_splits_on_newlines_and_keeps_a_partial_line() {
        let mut tail = VecDeque::new();
        let mut partial = String::new();
        append_clean(&mut tail, &mut partial, b"line one\nline ", 2000);
        assert_eq!(tail.iter().collect::<Vec<_>>(), vec!["line one"]);
        assert_eq!(partial, "line ");
        append_clean(&mut tail, &mut partial, b"two\n", 2000);
        assert_eq!(
            tail.iter().collect::<Vec<_>>(),
            vec!["line one", "line two"]
        );
        assert_eq!(partial, "");
    }

    #[test]
    fn append_clean_strips_ansi_from_each_line() {
        let mut tail = VecDeque::new();
        let mut partial = String::new();
        append_clean(&mut tail, &mut partial, b"\x1b[32mok\x1b[0m\n", 2000);
        assert_eq!(tail.iter().collect::<Vec<_>>(), vec!["ok"]);
    }

    #[test]
    fn push_capped_drops_from_the_front() {
        let mut buf = VecDeque::new();
        push_capped(&mut buf, b"abc", 5);
        push_capped(&mut buf, b"de", 5);
        assert_eq!(buf.iter().copied().collect::<Vec<u8>>(), b"abcde");
        push_capped(&mut buf, b"fg", 5);
        assert_eq!(buf.iter().copied().collect::<Vec<u8>>(), b"cdefg");
    }
}
