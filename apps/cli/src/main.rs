//! `theodore` CLI: grouping eval and debugging (docs/plan.md §5.4).

use std::sync::Arc;

use clap::{Parser, Subcommand};
use theodore_cli::{deps, endpoint, eval, graph_check, group, mcp_bridge, sandbox};
use theodore_domain::AgentId;
use theodore_grouping::GroupingOptions;
use theodore_ports::AgentPort;

#[derive(Parser)]
#[command(name = "theodore", version, about = "Theodore: AI-grouped PR review")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Group a PR's diff into reviewable steps.
    Group(group::GroupArgs),
    /// Replay saved PR fixtures through the grouping pipeline.
    Eval(eval::EvalArgs),
    /// stdio↔HTTP bridge for agent CLIs without HTTP MCP.
    McpBridge(mcp_bridge::McpBridgeArgs),
    /// Simulated host: write a scenario, drive a running sandbox
    /// (docs/sandbox.md).
    Sandbox(sandbox::SandboxArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // An agent's edit hook (docs/spaces.md §5): judges one edit and exits.
    theodore_agents::edit_guard::run_if_requested();
    let cli = Cli::parse();
    match cli.command {
        Command::Group(args) => run_group(args).await,
        Command::Eval(args) => run_eval(args).await,
        Command::McpBridge(args) => mcp_bridge::run(&args).await,
        Command::Sandbox(args) => tokio::task::spawn_blocking(move || sandbox::run(&args)).await?,
    }
}

async fn run_group(args: group::GroupArgs) -> anyhow::Result<()> {
    let deps = deps::real_deps();
    let (rev, pr, cwd) = match &args.pr_url {
        Some(url) if args.diff_file.is_none() => deps::load_pr(&deps, url).await?,
        _ => group::load_input(&args)?,
    };
    let agent_id: AgentId = args.agent.into();
    let report = group::run_with_model(
        deps.agent.as_ref(),
        agent_id,
        args.model.clone(),
        &pr,
        &rev,
        &cwd,
    )
    .await?;
    if let Some(path) = &args.save_fixture {
        let meta = group::FixtureMeta {
            title: pr.title.clone(),
            body: pr.body.clone(),
            commits: pr.commits.clone(),
        };
        group::save_fixture(path, &meta, &rev)?;
        eprintln!("saved fixture to {}", path.display());
    }
    group::emit(&report, &rev, args.json, args.save.as_deref())
}

async fn run_eval(args: eval::EvalArgs) -> anyhow::Result<()> {
    let fixtures = eval::load_fixtures(&args.fixtures_dir)?;
    if args.graph_only {
        let mut budget = theodore_grouping::PromptBudget::default();
        if let Some(max_chars) = args.max_chars {
            budget.max_chars = max_chars;
        }
        return graph_check::run(&fixtures, args.compare.as_deref(), &budget, args.show);
    }
    let agent_id: AgentId = args.agent.into();
    let cwd = std::env::current_dir()?;
    let agent: Arc<dyn AgentPort> = match &args.endpoint {
        Some(url) => Arc::new(endpoint::EndpointAgent::new(endpoint::EndpointConfig {
            base_url: url.clone(),
            model: args.model.clone().unwrap_or_else(|| "local".into()),
            max_tokens: args.max_tokens,
            constrain_ids: args.constrain_ids,
        })?),
        None => deps::real_deps().agent,
    };
    let mut opts = GroupingOptions {
        agent: agent_id,
        model: args.model.clone(),
        stream: args.stream,
        ..Default::default()
    };
    if let Some(max_chars) = args.max_chars {
        opts.budget.max_chars = max_chars;
    }
    if let Some(n) = args.concurrency {
        opts.chunk_concurrency = n;
    }
    let rows =
        eval::run_eval_with_options(agent.as_ref(), &opts, &fixtures, &cwd, args.save.as_deref())
            .await;
    let summary = eval::print_table(&rows);
    if !summary.meets_targets() {
        anyhow::bail!(
            "grouping quality regressed below the §5.4 M1 targets \
             (first try {:.0}% < {:.0}%, or after repair {:.0}% < {:.0}%)",
            summary.first_try_pct(),
            eval::TARGET_FIRST_TRY_PCT,
            summary.after_repair_pct(),
            eval::TARGET_AFTER_REPAIR_PCT,
        );
    }
    Ok(())
}
