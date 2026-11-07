//! `theodore sandbox …` (docs/sandbox.md): write the default scenario, and
//! drive a running sandbox through its control spool.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use theodore_contract::SandboxStatus;
use theodore_domain::CheckStatus;
use theodore_sandbox::scenario::{FileSpec, SideSpec};
use theodore_sandbox::{Action, PrSel, control, default_scenario};

#[derive(Debug, Clone, Args)]
pub struct SandboxArgs {
    /// Data dir of the running sandbox (default: THEODORE_DATA_DIR, else
    /// ~/.theodore-sandbox).
    #[arg(long, global = true, env = "THEODORE_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: SandboxCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SandboxCommand {
    /// Write the default scenario (scenario.json + assets/) to <dir>.
    Init { dir: PathBuf },
    /// Push a commit to a PR's head branch (`--file`/`--content` pairs; none
    /// = a generated one-line edit).
    Push {
        pr: String,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long = "content")]
        contents: Vec<String>,
        #[arg(long, short)]
        message: Option<String>,
        #[arg(long = "as")]
        author: Option<String>,
    },
    /// A teammate comments on `path:line` of a PR.
    Comment {
        pr: String,
        /// `path:line`
        at: String,
        text: String,
        #[arg(long = "as")]
        author: Option<String>,
    },
    /// Set a check's status.
    Check {
        pr: String,
        name: String,
        status: CheckArg,
    },
    /// Turn a fault on or off.
    Fault {
        name: String,
        state: OnOff,
        /// Fault parameter: latency ms, rate-limit reset seconds, 5xx rate
        /// (0..1), a PR number for not_found, approvals required, …
        #[arg(long)]
        value: Option<f64>,
    },
    /// Request a review (from you unless --user/--team).
    RequestReview {
        pr: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        team: Option<String>,
    },
    /// Merge a PR as someone else (default: its author).
    Merge {
        pr: String,
        #[arg(long = "as")]
        author: Option<String>,
    },
    /// Fire a scripted timeline event now.
    Fire { event: String },
    /// Faults, timeline and PR count of the running sandbox.
    Status,
    /// With the sandbox's app closed: N stored turns (markdown, tool steps) in
    /// one of its spaces, by id or name, to try a long chat without tokens.
    SeedChat {
        space: String,
        #[arg(long, default_value_t = 300)]
        turns: usize,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CheckArg {
    Pending,
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

fn pr_sel(s: &str) -> PrSel {
    match s.trim_start_matches('#').parse::<u64>() {
        Ok(n) => PrSel::Number(n),
        Err(_) => PrSel::Ref(s.to_string()),
    }
}

/// `path:line` → (`path`, line).
fn parse_at(at: &str) -> anyhow::Result<(String, u32)> {
    let (path, line) = at
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("expected path:line, got {at}"))?;
    let line = line
        .parse()
        .map_err(|_| anyhow::anyhow!("bad line number in {at}"))?;
    Ok((path.to_string(), line))
}

/// The action a subcommand sends; `None` for the local-only ones.
pub fn action_for(cmd: &SandboxCommand) -> anyhow::Result<Option<Action>> {
    Ok(Some(match cmd {
        SandboxCommand::Init { .. } | SandboxCommand::SeedChat { .. } => return Ok(None),
        SandboxCommand::Push {
            pr,
            files,
            contents,
            message,
            author,
        } => {
            if files.len() != contents.len() {
                anyhow::bail!("every --file needs a --content");
            }
            Action::Push {
                pr: pr_sel(pr),
                files: files
                    .iter()
                    .zip(contents)
                    .map(|(f, c)| FileSpec::text(f, c.replace("\\n", "\n")))
                    .collect(),
                message: message.clone(),
                author: author.clone(),
            }
        }
        SandboxCommand::Comment {
            pr,
            at,
            text,
            author,
        } => {
            let (path, line) = parse_at(at)?;
            Action::Comment {
                pr: pr_sel(pr),
                path,
                line,
                side: SideSpec::New,
                body: text.clone(),
                author: author.clone(),
            }
        }
        SandboxCommand::Check { pr, name, status } => Action::Check {
            pr: pr_sel(pr),
            name: name.clone(),
            status: match status {
                CheckArg::Pending => CheckStatus::Pending,
                CheckArg::Pass => CheckStatus::Pass,
                CheckArg::Fail => CheckStatus::Fail,
            },
        },
        SandboxCommand::Fault { name, state, value } => Action::Fault {
            name: name.clone(),
            on: matches!(state, OnOff::On),
            value: *value,
        },
        SandboxCommand::RequestReview { pr, user, team } => Action::RequestReview {
            pr: pr_sel(pr),
            user: user.clone(),
            team: team.clone(),
        },
        SandboxCommand::Merge { pr, author } => Action::Merge {
            pr: pr_sel(pr),
            author: author.clone(),
        },
        SandboxCommand::Fire { event } => Action::Fire {
            event: event.clone(),
        },
        SandboxCommand::Status => Action::Status,
    }))
}

pub fn print_status(s: &SandboxStatus) {
    println!(
        "sandbox \"{}\" — signed in as {}, {} PRs",
        s.info.scenario, s.viewer, s.prs
    );
    println!("  scenario  {}", s.info.scenario_dir);
    println!("  data dir  {}", s.info.data_dir);
    println!("faults:");
    for f in &s.faults {
        println!(
            "  [{}] {:<20} {}",
            if f.on { "on " } else { "off" },
            f.name,
            f.detail
        );
    }
    println!("timeline:");
    for e in &s.events {
        let due = e
            .at_ms
            .map(|ms| format!("at {}s", ms / 1000))
            .unwrap_or_else(|| "manual".into());
        println!(
            "  {} {:<16} {:<8} {}",
            if e.fired { "✓" } else { "·" },
            e.id,
            due,
            e.label
        );
    }
}

pub fn run(args: &SandboxArgs) -> anyhow::Result<()> {
    if let SandboxCommand::Init { dir } = &args.command {
        let path = default_scenario::write(dir)?;
        println!("wrote {}", path.display());
        println!(
            "run it: THEODORE_SANDBOX={} theodore-desktop  (or --sandbox {})",
            dir.display(),
            dir.display()
        );
        return Ok(());
    }
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(theodore_runtime::sandbox_data_dir);
    if let SandboxCommand::SeedChat { space, turns } = &args.command {
        let id = crate::sandbox_seed::seed_chat(&data_dir, space, *turns)?;
        println!("seeded {turns} turns in space {id}");
        return Ok(());
    }
    let root = theodore_sandbox::root_of(&data_dir);
    let action = action_for(&args.command)?.expect("not init");
    if matches!(action, Action::Status) && control::alive(&root).is_none() {
        return offline_status(&root);
    }
    let response = control::send(&root, action, Duration::from_secs(15))?;
    if !response.ok {
        anyhow::bail!("{}", response.message);
    }
    match (&args.command, &response.status) {
        (SandboxCommand::Status, Some(s)) => print_status(s),
        _ => println!("{}", response.message),
    }
    Ok(())
}

/// `status` with no app running: what the persisted state says.
fn offline_status(root: &Path) -> anyhow::Result<()> {
    let Some(state) = theodore_sandbox::state::State::load(root)? else {
        anyhow::bail!(
            "no sandbox at {} — run `theodore sandbox init <dir>` and start Theodore with \
             THEODORE_SANDBOX=<dir>",
            root.display()
        );
    };
    println!("sandbox at {} is not running", root.display());
    println!(
        "  {} PRs, {} timeline events fired",
        state.prs.len(),
        state.fired.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subcommands_map_to_actions() {
        let a = action_for(&SandboxCommand::Comment {
            pr: "acme/web#3".into(),
            at: "src/a.ts:12".into(),
            text: "hi".into(),
            author: Some("lee".into()),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(
            a,
            Action::Comment { line: 12, ref path, pr: PrSel::Ref(_), .. } if path == "src/a.ts"
        ));
        let a = action_for(&SandboxCommand::Push {
            pr: "#101".into(),
            files: vec!["a.txt".into()],
            contents: vec!["x\\ny".into()],
            message: None,
            author: None,
        })
        .unwrap()
        .unwrap();
        let Action::Push { pr, files, .. } = a else {
            panic!()
        };
        assert_eq!(pr, PrSel::Number(101));
        assert_eq!(files[0].text.as_deref(), Some("x\ny"));
        assert!(
            action_for(&SandboxCommand::Push {
                pr: "1".into(),
                files: vec!["a".into()],
                contents: vec![],
                message: None,
                author: None,
            })
            .is_err()
        );
        assert!(
            action_for(&SandboxCommand::Init { dir: "x".into() })
                .unwrap()
                .is_none()
        );
    }
}
