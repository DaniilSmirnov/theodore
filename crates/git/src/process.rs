//! Runs `git` as a child process with no terminal prompting and no secret
//! ever written to disk, argv, or `.git/config`.

use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;

use theodore_ports::{GitError, GitResult};

/// When set, every `git` child ignores the user's global config
/// (`~/.gitconfig`: hooks paths, signing, aliases). The sandbox sets it: its
/// repos are synthetic, so a personal commit hook or signing key must not
/// apply there. Process-wide, because a process is either the sandbox or not.
static ISOLATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ignore the user's global git config from now on (sandbox mode).
pub fn isolate_from_user_config() {
    ISOLATED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// One invocation of `git`. `token`, if present, is set only on this child
/// process's env and read back by the askpass helper (`auth.rs`); it is
/// never part of `args`.
pub(crate) struct GitProc<'a> {
    pub cwd: &'a Path,
    pub askpass: &'a Path,
    pub args: &'a [&'a str],
    pub token: Option<&'a str>,
    /// Bytes to feed the child's stdin (e.g. shas for `cat-file --batch-check`).
    pub stdin: Option<&'a [u8]>,
}

impl GitProc<'_> {
    /// Builds the child's `Command` without spawning it, so a test can
    /// inspect/drop a real `Child` directly (`run`'s own `Child` never
    /// escapes it). Production code always goes through `run`/`run_text`.
    fn command(&self) -> tokio::process::Command {
        self.command_below(None)
    }

    /// `command`; with `ceiling`, git never looks above it for a repository
    /// (`GIT_CEILING_DIRECTORIES`), so a space worktree whose `.git` went
    /// missing fails instead of resolving a parent repo. An inherited
    /// `GIT_DIR` / `GIT_WORK_TREE` would skip discovery altogether, so
    /// those go too.
    fn command_below(&self, ceiling: Option<&Path>) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("git");
        if let Some(ceiling) = ceiling {
            cmd.env("GIT_CEILING_DIRECTORIES", ceiling);
            for var in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_COMMON_DIR",
                "GIT_INDEX_FILE",
            ] {
                cmd.env_remove(var);
            }
        }
        cmd.current_dir(self.cwd)
            .args(self.args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", self.askpass)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(
                ISOLATED
                    .load(std::sync::atomic::Ordering::SeqCst)
                    .then_some(("GIT_CONFIG_GLOBAL", "/dev/null")),
            )
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A cancelled operation (the caller's future dropped) must not
            // leave `git` running: without this, a long `fetch`/`clone` keeps
            // going in the background after Theodore gives up on it.
            .kill_on_drop(true);
        match self.token {
            Some(t) => {
                cmd.env("THEODORE_GIT_TOKEN", t);
                cmd.env("THEODORE_GIT_USER", "x-access-token");
            }
            None => {
                cmd.env_remove("THEODORE_GIT_TOKEN");
            }
        }
        cmd
    }

    pub(crate) async fn run(&self) -> GitResult<Vec<u8>> {
        self.run_cmd(self.command()).await
    }

    /// `run` with discovery stopped at `ceiling` (`command_below`).
    pub(crate) async fn run_below(&self, ceiling: &Path) -> GitResult<Vec<u8>> {
        self.run_cmd(self.command_below(Some(ceiling))).await
    }

    /// `run` below `ceiling`, with extra variables on the child
    /// (`GIT_INDEX_FILE` for a snapshot through a temporary index).
    pub(crate) async fn run_env_below(
        &self,
        ceiling: &Path,
        env: &[(&str, &Path)],
    ) -> GitResult<Vec<u8>> {
        let mut cmd = self.command_below(Some(ceiling));
        cmd.envs(env.iter().copied());
        self.run_cmd(cmd).await
    }

    async fn run_cmd(&self, mut cmd: tokio::process::Command) -> GitResult<Vec<u8>> {
        let mut child = cmd.spawn().map_err(io_err)?;
        if let (Some(data), Some(mut pipe)) = (self.stdin, child.stdin.take()) {
            pipe.write_all(data).await.map_err(io_err)?;
            // Dropping `pipe` closes the write end so the child sees EOF.
        }
        let output = child.wait_with_output().await.map_err(io_err)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let code = output.status.code().unwrap_or(-1);
            if looks_like_auth_failure(&stderr) {
                return Err(GitError::Auth(stderr));
            }
            if looks_like_network_failure(&stderr) {
                return Err(GitError::Network(stderr));
            }
            return Err(GitError::Command {
                args: self.args.join(" "),
                code,
                stderr,
            });
        }
        Ok(output.stdout)
    }

    pub(crate) async fn run_text(&self) -> GitResult<String> {
        Ok(String::from_utf8_lossy(&self.run().await?).into_owned())
    }

    /// Like `run`, but never turns a non-zero exit into `Err`: some
    /// subcommands (`merge-tree`) use the exit code itself to mean
    /// something (0 clean, 1 conflicts, >1 a real error) and still write
    /// useful stdout on exit 1, which `run` would otherwise discard.
    pub(crate) async fn run_raw(&self) -> GitResult<RawOutput> {
        self.run_raw_cmd(self.command()).await
    }

    /// `run_raw` with discovery stopped at `ceiling` (`command_below`).
    pub(crate) async fn run_raw_below(&self, ceiling: &Path) -> GitResult<RawOutput> {
        self.run_raw_cmd(self.command_below(Some(ceiling))).await
    }

    async fn run_raw_cmd(&self, mut cmd: tokio::process::Command) -> GitResult<RawOutput> {
        let mut child = cmd.spawn().map_err(io_err)?;
        if let (Some(data), Some(mut pipe)) = (self.stdin, child.stdin.take()) {
            pipe.write_all(data).await.map_err(io_err)?;
        }
        let output = child.wait_with_output().await.map_err(io_err)?;
        Ok(RawOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

pub(crate) struct RawOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn io_err(e: std::io::Error) -> GitError {
    if e.kind() == std::io::ErrorKind::NotFound {
        GitError::NotInstalled
    } else {
        GitError::Io(e)
    }
}

fn looks_like_auth_failure(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("authentication failed")
        || s.contains("could not read username")
        || s.contains("could not read password")
        || s.contains("terminal prompts disabled")
        || s.contains("http basic: access denied")
}

/// The remote couldn't be reached at all: no network, DNS, a timeout.
/// `unable to access '<url>': …` is also what git prints for an HTTP
/// error status (403, 404), which says the network works — so it only
/// counts without one.
pub(crate) fn looks_like_network_failure(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    const MARKERS: [&str; 10] = [
        "could not resolve host",
        "could not resolve hostname",
        "temporary failure in name resolution",
        "connection timed out",
        "operation timed out",
        "network is unreachable",
        "no route to host",
        "failed to connect",
        "connection refused",
        "connection reset",
    ];
    MARKERS.iter().any(|m| s.contains(m))
        || (s.contains("unable to access") && !s.contains("returned error"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pid_is_alive(pid: u32) -> bool {
        // `kill -0` sends no signal, just checks the pid exists and is ours
        // to signal — the standard no-op existence probe.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Regression guard for `kill_on_drop`: a `git clone` against a
    /// listener that accepts the TCP connection but never speaks the
    /// protocol hangs forever unless killed. Dropping the `Child` (what
    /// happens when the owning future is cancelled/dropped) must end it.
    #[tokio::test]
    async fn dropping_the_child_kills_a_hanging_clone() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local_addr").port();
        std::thread::spawn(move || {
            // Accept and then go silent: `git://` expects the server to
            // speak first, so this stalls the client indefinitely.
            let _ = listener.accept();
            std::thread::sleep(Duration::from_secs(30));
        });

        let cwd = std::env::temp_dir();
        let askpass = cwd.join("theodore-test-unused-askpass.sh");
        let dest = cwd.join(format!("theodore-kill-on-drop-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let url = format!("git://127.0.0.1:{port}/theodore-test.git");
        let dest_str = dest.to_str().expect("utf8 tmp path").to_string();

        let proc = GitProc {
            cwd: &cwd,
            askpass: &askpass,
            args: &["clone", &url, &dest_str],
            token: None,
            stdin: None,
        };

        let mut cmd = proc.command();
        let child = cmd.spawn().expect("spawn git clone");
        let pid = child.id().expect("pid");
        assert!(pid_is_alive(pid), "git should be running right after spawn");

        drop(child);
        // Give the OS a moment to act on the kill `kill_on_drop` issues.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !pid_is_alive(pid),
            "git clone must not survive the Child being dropped"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }
}

#[cfg(test)]
mod network_tests {
    use super::looks_like_network_failure;

    #[test]
    fn unreachable_remotes_read_as_offline() {
        for stderr in [
            "fatal: unable to access 'https://github.com/a/b.git/': Could not resolve host: github.com",
            "ssh: Could not resolve hostname github.com: nodename nor servname provided",
            "fatal: unable to access 'https://github.com/a/b.git/': Failed to connect to github.com port 443 after 3 ms: Couldn't connect to server",
            "fatal: unable to access 'https://x/': Connection timed out after 30000 milliseconds",
            "ssh: connect to host github.com port 22: Operation timed out",
            "ssh: connect to host github.com port 22: Network is unreachable",
        ] {
            assert!(looks_like_network_failure(stderr), "{stderr}");
        }
    }

    #[test]
    fn a_reachable_remote_that_says_no_is_not_offline() {
        for stderr in [
            "fatal: unable to access 'https://github.com/a/b.git/': The requested URL returned error: 403",
            "fatal: couldn't find remote ref refs/pull/9/head",
            "error: cannot lock ref 'refs/heads/main': is at abc but expected def",
            "fatal: 'origin' does not appear to be a git repository",
        ] {
            assert!(!looks_like_network_failure(stderr), "{stderr}");
        }
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;

    #[test]
    fn isolation_points_git_away_from_the_global_config() {
        let askpass = std::path::PathBuf::from("/bin/true");
        let cwd = std::env::temp_dir();
        let p = GitProc {
            cwd: &cwd,
            askpass: &askpass,
            args: &["--version"],
            token: None,
            stdin: None,
        };
        isolate_from_user_config();
        let cmd = p.command();
        let set: Vec<_> = cmd
            .as_std()
            .get_envs()
            .filter(|(k, _)| *k == "GIT_CONFIG_GLOBAL")
            .map(|(_, v)| v.map(|v| v.to_owned()))
            .collect();
        assert_eq!(set, vec![Some(std::ffi::OsString::from("/dev/null"))]);
    }
}
