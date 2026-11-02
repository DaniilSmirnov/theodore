//! The prompts behind "Set up with agent" and "Fix with agent"
//! (docs/config.md §2 `spaces:`): a space with nothing configured, or
//! whose setup just failed, asks its own agent to work it out instead of
//! leaving the user to write the YAML by hand.
//!
//! The agent edits the project's config file with its normal tools, in
//! its own worktree, the same as any other change it makes in the space:
//! the commands it proposes reach the project once the space ships
//! (§8), and wait for the usual "Run commands from this repo?" from
//! there — nothing here bypasses that trust prompt.
//!
//! Pure text, kept out of `space_hooks.rs` so it's testable without a
//! running `Core`; `Core::space_setup_assist` / `Core::space_fix_setup`
//! send it as a normal turn.

use crate::config::REPO_FILE;

/// "Set up with agent": sent once, when the project has no `spaces:`
/// commands yet.
pub(crate) fn setup_assist_prompt() -> String {
    format!(
        "This project has no `spaces:` commands set up yet, so every space \
         starts with nothing to run and nothing to check. Look at this repo \
         and work out its own development loop: its package manager and \
         lockfile, its README, any Makefile, justfile or docker-compose \
         file, and whatever scripts it already defines — don't invent a \
         command it doesn't support.\n\
         \n\
         Then add a `spaces:` section to {REPO_FILE} (create the file if \
         it's missing) with whichever of these the repo actually supports:\n\
         - `setup`: installs dependencies and starts anything the project \
         needs once, before the first turn.\n\
         - `run`: its dev server, using $THEODORE_PORT for the port when the \
         project lets you pick one.\n\
         - `check`: lint and test, run after a turn changes files.\n\
         - `archive`: tears down whatever `setup` or `run` started.\n\
         - `copy`: files this project's own checkout has that a fresh space \
         needs, besides the `.env*` files Theodore already copies by \
         default.\n\
         - `ports`: how many ports `run` needs, if more than the one \
         `$THEODORE_PORT` already gives it.\n\
         \n\
         End with a short summary of what you set and why."
    )
}

/// "Fix with agent": the setup command that just failed, its exit status
/// and the tail of its output.
pub(crate) fn fix_setup_prompt(script: &str, failure: &str, log_tail: &[String]) -> String {
    let log = if log_tail.is_empty() {
        "(no output)".to_string()
    } else {
        log_tail.join("\n")
    };
    format!(
        "The space's setup command just failed:\n\
         \n\
         $ {script}\n\
         {failure}\n\
         \n\
         Its last output:\n\
         {log}\n\
         \n\
         Fix the cause — the project's own script, or the `spaces:` command \
         in {REPO_FILE} if that's what's wrong — so setup can succeed. End \
         by saying what you changed and that setup can be retried."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_assist_names_the_signals_the_commands_and_the_config_file() {
        let p = setup_assist_prompt();
        for word in [
            "package manager",
            "README",
            "Makefile",
            "docker-compose",
            REPO_FILE,
            "setup",
            "run",
            "check",
            "archive",
            "copy",
            "ports",
            "$THEODORE_PORT",
        ] {
            assert!(p.contains(word), "{word:?} missing from: {p}");
        }
    }

    #[test]
    fn fix_setup_quotes_the_command_status_and_log() {
        let p = fix_setup_prompt(
            "pnpm install",
            "setup exited with 1",
            &["npm ERR! x".to_string()],
        );
        assert!(p.contains("pnpm install"));
        assert!(p.contains("setup exited with 1"));
        assert!(p.contains("npm ERR! x"));
        assert!(p.contains(REPO_FILE));
    }

    #[test]
    fn fix_setup_notes_when_there_is_no_log() {
        let p = fix_setup_prompt("make setup", "setup timed out after 600 s", &[]);
        assert!(p.contains("no output"));
        assert!(p.contains("setup timed out after 600 s"));
    }
}
