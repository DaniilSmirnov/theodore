//! `Command::output` that finishes when the child exits, not when its
//! pipes close.
//!
//! On macOS a pipe is made with `pipe` and only then marked close-on-exec,
//! so a child another thread forks in between inherits its write end. While
//! that unrelated child lives (an agent run, a dev server), the pipe never
//! reaches EOF and `output()` waits for it — a `--version` probe then times
//! out. Here the pipes are read until EOF or, once the child has exited, for
//! at most [`DRAIN`] more.

use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// How long the pipes may still be read after the child exited.
const DRAIN: Duration = Duration::from_millis(500);

/// Runs `cmd` with stdout and stderr piped and returns its output once it
/// has exited (see the module docs).
pub async fn output_after_exit(mut cmd: Command) -> std::io::Result<Output> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout = collect(child.stdout.take());
    let stderr = collect(child.stderr.take());
    let status = child.wait().await?;
    Ok(Output {
        status,
        stdout: stdout.finish().await,
        stderr: stderr.finish().await,
    })
}

/// A pipe being read into a buffer in the background.
struct Collected {
    buf: Arc<Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}

fn collect(pipe: Option<impl AsyncRead + Unpin + Send + 'static>) -> Collected {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = buf.clone();
    let task = tokio::spawn(async move {
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    Collected { buf, task }
}

impl Collected {
    /// What was read: everything up to EOF, or what arrived within
    /// [`DRAIN`] when a leaked write end keeps EOF away.
    async fn finish(mut self) -> Vec<u8> {
        if tokio::time::timeout(DRAIN, &mut self.task).await.is_err() {
            self.task.abort();
        }
        std::mem::take(&mut *self.buf.lock().unwrap())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_output_and_status() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
        let out = output_after_exit(cmd).await.unwrap();
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
    }

    /// A grandchild that keeps stdout open (as a leaked write end would)
    /// doesn't hold the result back past the drain.
    #[tokio::test]
    async fn does_not_wait_for_a_pipe_held_open_elsewhere() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo v1; sleep 30 &"]);
        let started = std::time::Instant::now();
        let out = output_after_exit(cmd).await.unwrap();
        assert_eq!(out.stdout, b"v1\n");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
