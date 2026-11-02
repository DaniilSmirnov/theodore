//! `TerminalPort` over `portable-pty` (docs/spaces.md "Terminal"): one
//! interactive shell per call, raw bytes both ways, resize, and the same
//! group-kill discipline as `ShellRunner` (the pty's slave makes the child
//! its own session/process-group leader, so its pid doubles as its pgid).
//!
//! Output is coalesced before it reaches the caller — an agent typing or a
//! build spewing logs must not turn into one IPC message per `read()`:
//!
//! ```text
//! blocking reader thread ─▶ raw chunks ─▶ coalesce (16ms or 64KB) ─▶ Output
//! ```

use std::io::{Read, Write};
use std::time::Duration;

use async_trait::async_trait;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use theodore_ports::terminal::{
    TerminalEvent, TerminalExit, TerminalHandle, TerminalPort, TerminalSpec,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::env;
use crate::shell::terminate_group;

/// How long a chunk may wait for more bytes before it's flushed.
const BATCH_DELAY: Duration = Duration::from_millis(16);
/// A chunk is flushed as soon as it reaches this size, delay or not.
const BATCH_BYTES: usize = 64 * 1024;
/// How long `kill` waits after SIGTERM before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// Runs shells under a real PTY.
#[derive(Debug, Clone, Default)]
pub struct PtyRunner;

impl PtyRunner {
    pub fn new() -> Self {
        Self
    }
}

/// What the writer thread (the only thing allowed to touch the master pty
/// handle, which isn't `Send` across every platform's implementation) does.
enum WriterCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
}

#[async_trait]
impl TerminalPort for PtyRunner {
    async fn spawn(&self, spec: TerminalSpec) -> TerminalHandle {
        let (tx, rx) = mpsc::channel(64);
        let (input, input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (resize, resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();
        let cancel = CancellationToken::new();

        let pair = match native_pty_system().openpty(PtySize {
            rows: spec.rows.max(1),
            cols: spec.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(p) => p,
            Err(e) => {
                exit_with(&tx, format!("could not open a pty: {e}")).await;
                return TerminalHandle {
                    events: rx,
                    input,
                    resize,
                    kill: cancel,
                    pid: None,
                };
            }
        };

        let mut cmd = CommandBuilder::new(&spec.shell);
        for a in &spec.args {
            cmd.arg(a);
        }
        cmd.cwd(&spec.cwd);
        cmd.env_clear();
        // A sane default so curses apps and colors work even if nothing
        // upstream set one; the parent env (and then `spec.env`) may still
        // override it.
        cmd.env("TERM", "xterm-256color");
        for (k, v) in env::filtered_env() {
            cmd.env(k, v);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                exit_with(&tx, format!("could not start the shell: {e}")).await;
                return TerminalHandle {
                    events: rx,
                    input,
                    resize,
                    kill: cancel,
                    pid: None,
                };
            }
        };
        // The slave's fd must close in this process once the child has it;
        // held open, the child's pty never sees EOF on its own stdin/out.
        drop(pair.slave);
        let pid = child.process_id();

        let reader = pair.master.try_clone_reader().ok();
        if let Some(reader) = reader {
            let (raw_tx, raw_rx) = mpsc::channel::<Vec<u8>>(256);
            std::thread::spawn(move || pump_reads(reader, raw_tx));
            tokio::spawn(coalesce(raw_rx, tx.clone()));
        }

        // One thread owns both the writer and the master (for resize): the
        // trait objects portable-pty hands back aren't `Send` on every
        // platform, so they never cross an `.await`.
        let (wtx, wrx) = std::sync::mpsc::channel::<WriterCmd>();
        if let Ok(writer) = pair.master.take_writer() {
            let master = pair.master;
            std::thread::spawn(move || writer_loop(master, writer, wrx));
            forward_writer_input(input_rx, wtx.clone());
            forward_writer_resize(resize_rx, wtx);
        }

        tokio::spawn(supervise(child, pid, cancel.clone(), tx));

        TerminalHandle {
            events: rx,
            input,
            resize,
            kill: cancel,
            pid,
        }
    }
}

async fn exit_with(tx: &mpsc::Sender<TerminalEvent>, error: String) {
    let _ = tx
        .send(TerminalEvent::Exit(TerminalExit {
            code: None,
            error: Some(error),
        }))
        .await;
}

/// Forwards keystrokes from the async side to the writer thread; a plain
/// `tokio::spawn`, never blocking the runtime.
fn forward_writer_input(
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    tx: std::sync::mpsc::Sender<WriterCmd>,
) {
    tokio::spawn(async move {
        while let Some(buf) = rx.recv().await {
            if tx.send(WriterCmd::Write(buf)).is_err() {
                return;
            }
        }
    });
}

/// Same, for resizes.
fn forward_writer_resize(
    mut rx: mpsc::UnboundedReceiver<(u16, u16)>,
    tx: std::sync::mpsc::Sender<WriterCmd>,
) {
    tokio::spawn(async move {
        while let Some((cols, rows)) = rx.recv().await {
            if tx.send(WriterCmd::Resize(cols, rows)).is_err() {
                return;
            }
        }
    });
}

/// Owns the master pty and the writer for the shell's whole life: writes
/// typed input, and resizes on request. Ends when both ends of the input
/// channel are dropped (the handle, or the terminal having exited).
fn writer_loop(
    master: Box<dyn portable_pty::MasterPty + Send>,
    mut writer: Box<dyn Write + Send>,
    rx: std::sync::mpsc::Receiver<WriterCmd>,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            WriterCmd::Write(buf) => {
                if writer.write_all(&buf).is_err() {
                    return;
                }
                let _ = writer.flush();
            }
            WriterCmd::Resize(cols, rows) => {
                let _ = master.resize(PtySize {
                    rows: rows.max(1),
                    cols: cols.max(1),
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
    }
}

/// Blocking reads off the pty's master, each one forwarded as soon as it
/// arrives; `coalesce` is what batches them for the caller. Ends on EOF (a
/// clean `Ok(0)`) or any read error (a pty's master commonly errors — EIO on
/// Linux — once every process holding the slave has exited).
fn pump_reads(mut reader: Box<dyn Read + Send>, tx: mpsc::Sender<Vec<u8>>) {
    let mut buf = [0u8; 8 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if tx.blocking_send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// Coalesces raw chunks into `Output` events at most every `BATCH_DELAY`,
/// sooner if `BATCH_BYTES` fills up first — the flow control `docs/spaces.md`
/// asks for, so a chatty shell can't flood the IPC event bus.
async fn coalesce(mut rx: mpsc::Receiver<Vec<u8>>, tx: mpsc::Sender<TerminalEvent>) {
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let first = match rx.recv().await {
            Some(b) => b,
            None => break,
        };
        pending.extend_from_slice(&first);
        let deadline = tokio::time::Instant::now() + BATCH_DELAY;
        while pending.len() < BATCH_BYTES {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(b)) => pending.extend_from_slice(&b),
                Ok(None) => break,
                Err(_) => break, // the delay elapsed; flush what we have
            }
        }
        if tx
            .send(TerminalEvent::Output(std::mem::take(&mut pending)))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Waits for the shell to exit, killing its group on cancel; always ends in
/// exactly one `Exit` event.
async fn supervise(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    pid: Option<u32>,
    cancel: CancellationToken,
    tx: mpsc::Sender<TerminalEvent>,
) {
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let status = child.wait();
        let _ = done_tx.send(status.ok().map(|s| s.exit_code()));
        // The child (and the pty it owned) is dropped here, on the blocking
        // thread, once `wait` returns — never on the async side.
    });
    let code = tokio::select! {
        r = &mut done_rx => r.ok().flatten(),
        _ = cancel.cancelled() => {
            if let Some(pid) = pid {
                terminate_group(pid, KILL_GRACE).await;
            }
            tokio::time::timeout(Duration::from_secs(5), done_rx)
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
        }
    };
    let _ = tx
        .send(TerminalEvent::Exit(TerminalExit {
            code: code.map(|c| c as i32),
            error: None,
        }))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TerminalSpec {
        TerminalSpec {
            shell: "/bin/sh".into(),
            args: vec![],
            cwd: std::env::temp_dir(),
            env: vec![],
            cols: 80,
            rows: 24,
        }
    }

    async fn collect_until_marker(
        events: &mut mpsc::Receiver<TerminalEvent>,
        marker: &str,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
                Ok(Some(TerminalEvent::Output(b))) => {
                    out.extend_from_slice(&b);
                    if String::from_utf8_lossy(&out).contains(marker) {
                        return out;
                    }
                }
                Ok(Some(TerminalEvent::Exit(_))) | Ok(None) => return out,
                Err(_) => return out, // timed out; the assertion below fails loudly
            }
        }
    }

    #[tokio::test]
    async fn runs_a_command_and_streams_its_output() {
        let mut handle = PtyRunner::new().spawn(spec()).await;
        handle
            .input
            .send(b"printf 'hello-pty-%s\\n' done\n".to_vec())
            .unwrap();
        let out = collect_until_marker(&mut handle.events, "hello-pty-done").await;
        assert!(
            String::from_utf8_lossy(&out).contains("hello-pty-done"),
            "{out:?}"
        );
        handle.kill.cancel();
        drop(handle.input);
    }

    #[tokio::test]
    async fn resize_does_not_error_and_reaches_the_shell() {
        let mut handle = PtyRunner::new().spawn(spec()).await;
        handle.resize.send((132, 43)).unwrap();
        // `stty size` prints "rows cols"; proves the resize reached the pty.
        handle.input.send(b"stty size\n".to_vec()).unwrap();
        let out = collect_until_marker(&mut handle.events, "43 132").await;
        assert!(String::from_utf8_lossy(&out).contains("43 132"), "{out:?}");
        handle.kill.cancel();
    }

    #[tokio::test]
    async fn kill_ends_the_session_with_an_exit_event() {
        let mut handle = PtyRunner::new().spawn(spec()).await;
        handle.input.send(b"sleep 30\n".to_vec()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.kill.cancel();
        let exit = loop {
            match tokio::time::timeout(Duration::from_secs(10), handle.events.recv())
                .await
                .expect("the session should end once killed")
            {
                Some(TerminalEvent::Exit(x)) => break x,
                Some(TerminalEvent::Output(_)) => continue,
                None => panic!("no exit event"),
            }
        };
        assert!(exit.error.is_none());
    }

    #[tokio::test]
    async fn a_missing_shell_exits_with_an_error_instead_of_panicking() {
        let mut spec = spec();
        spec.shell = "/nonexistent/theodore/shell".into();
        let mut handle = PtyRunner::new().spawn(spec).await;
        let exit = loop {
            match handle.events.recv().await {
                Some(TerminalEvent::Exit(x)) => break x,
                Some(TerminalEvent::Output(_)) => continue,
                None => panic!("no exit event"),
            }
        };
        assert!(exit.error.is_some());
    }
}
