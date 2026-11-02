//! Keeps an agent's file-edit tools inside its working directories
//! (docs/spaces.md §5 Safety): the space worktree plus the turn's
//! attachment dirs. Shell commands are not covered; this is no OS sandbox.
//!
//! Claude Code runs a `PreToolUse` hook before every Edit / Write /
//! NotebookEdit, in every permission mode (`bypassPermissions` included).
//! The hook is Theodore's own binary in guard mode:
//!
//! ```text
//! claude ─▶ PreToolUse ─▶ <theodore exe> --theodore-edit-guard <root>…
//!                            stdin: {"tool_input":{"file_path":…},"cwd":…}
//!                            inside a root ─▶ exit 0   (the edit runs)
//!                            outside      ─▶ exit 2   (denied, reason → agent)
//! ```
//!
//! A binary only offers itself as the hook after its `main` called
//! [`run_if_requested`], so a host that never handles the flag (a test
//! binary) is never launched as one.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;

/// argv[1] of the guard mode.
pub const FLAG: &str = "--theodore-edit-guard";

static EXE: OnceLock<PathBuf> = OnceLock::new();

/// Call first in `main`. In guard mode (`<exe> --theodore-edit-guard
/// <root>…`) it judges one hook input from stdin and exits the process:
/// 0 allows the edit, 2 denies it with the reason on stderr. Otherwise it
/// returns, and this binary becomes the hook later runs use.
pub fn run_if_requested() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(OsStr::new(FLAG)) {
        let roots: Vec<PathBuf> = args.map(PathBuf::from).collect();
        let mut input = String::new();
        let verdict = std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| format!("unreadable hook input: {e}"))
            .and_then(|_| check(&input, &roots));
        match verdict {
            Ok(()) => std::process::exit(0),
            Err(why) => {
                eprintln!("{why}");
                std::process::exit(2);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = EXE.set(exe);
    }
}

/// The binary to run as the hook, once `run_if_requested` has set it.
pub fn installed_exe() -> Option<&'static Path> {
    EXE.get().map(PathBuf::as_path)
}

/// The hook's shell command (Claude Code runs it with `sh -c`).
pub fn hook_command(exe: &Path, roots: &[PathBuf]) -> String {
    let mut cmd = shell_quote(&exe.display().to_string());
    cmd.push(' ');
    cmd.push_str(FLAG);
    for root in roots {
        cmd.push(' ');
        cmd.push_str(&shell_quote(&root.display().to_string()));
    }
    cmd
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `Ok` when the edit in a `PreToolUse` hook input (Claude Code's JSON)
/// lands inside one of `roots`. Fails closed: no path, a `..` in it, or
/// unreadable input is a denial. Symlinks are resolved on both sides, so a
/// link inside the worktree pointing out of it doesn't count as inside.
pub fn check(input: &str, roots: &[PathBuf]) -> Result<(), String> {
    let v: Value =
        serde_json::from_str(input).map_err(|e| format!("unreadable hook input: {e}"))?;
    let tool = &v["tool_input"];
    let raw = ["file_path", "notebook_path", "path"]
        .iter()
        .find_map(|k| tool[*k].as_str())
        .ok_or("no file path in this edit")?;
    let mut path = PathBuf::from(raw);
    if path.is_relative() {
        let cwd = v["cwd"].as_str().ok_or("a relative path and no cwd")?;
        path = Path::new(cwd).join(path);
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(format!("{raw}: use a path without `..`"));
    }
    let real = physical(&path);
    if roots.iter().any(|r| real.starts_with(physical(r))) {
        return Ok(());
    }
    let inside = roots
        .first()
        .map_or_else(String::new, |r| format!(" inside {}", r.display()));
    Err(format!(
        "{raw} is outside this space; edit files only{inside}"
    ))
}

/// `p` with symlinks resolved on its deepest existing ancestor (the file
/// itself may not exist yet).
fn physical(p: &Path) -> PathBuf {
    let mut base = p;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            return rest.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(path: &str) -> String {
        serde_json::json!({
            "cwd": "/nowhere",
            "tool_name": "Write",
            "tool_input": { "file_path": path, "content": "x" },
        })
        .to_string()
    }

    #[test]
    fn edits_inside_a_root_pass_and_outside_are_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let att = tmp.path().join("att");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&att).unwrap();
        let roots = vec![ws.clone(), att.clone()];
        let at = |p: &Path| input(&p.display().to_string());

        assert!(check(&at(&ws.join("new/dir/file.rs")), &roots).is_ok());
        assert!(check(&at(&att.join("shot.png")), &roots).is_ok());
        let err = check(&at(&tmp.path().join("other/x.txt")), &roots).unwrap_err();
        assert!(err.contains("outside this space"), "{err}");
        assert!(check(&at(&ws.join("../other/x.txt")), &roots).is_err());
        // A sibling whose name only starts with the root's.
        assert!(check(&at(&tmp.path().join("ws-2/x")), &roots).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_worktree_is_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::os::unix::fs::symlink(&out, ws.join("link")).unwrap();
        let roots = vec![ws.clone()];
        let p = ws.join("link/x.txt").display().to_string();
        assert!(check(&input(&p), &roots).is_err());
    }

    #[test]
    fn relative_paths_resolve_against_cwd_and_bad_input_is_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = vec![tmp.path().to_path_buf()];
        let rel = serde_json::json!({
            "cwd": tmp.path(),
            "tool_input": { "notebook_path": "nb.ipynb" },
        })
        .to_string();
        assert!(check(&rel, &roots).is_ok());
        assert!(check("not json", &roots).is_err());
        assert!(check(r#"{"tool_input":{}}"#, &roots).is_err());
    }

    #[test]
    fn the_hook_command_quotes_every_path() {
        let cmd = hook_command(
            Path::new("/Applications/Theodore Staging.app/Contents/MacOS/theodore"),
            &[PathBuf::from("/Users/o'neil/.theodore/spaces/a1")],
        );
        assert_eq!(
            cmd,
            "'/Applications/Theodore Staging.app/Contents/MacOS/theodore' \
             --theodore-edit-guard '/Users/o'\\''neil/.theodore/spaces/a1'"
        );
    }
}
