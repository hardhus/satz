pub mod commands;

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "satz",
    version,
    about = "Fast Markdown knowledge-base CLI",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Open or create today's daily note
    Daily(commands::daily_cmd::DailyArgs),
    /// Format vault files in place, or check whether they're already formatted
    Fmt(commands::fmt_cmd::FmtArgs),
    /// Export graph visualization (dot or json)
    Graph(commands::graph_cmd::GraphArgs),
    /// Index vault and show summary
    Index(commands::index_cmd::IndexArgs),
    /// List documents by tag, orphan status, or broken links
    List(commands::list_cmd::ListArgs),
    /// Resolve a wikilink to its file path
    Resolve(commands::resolve_cmd::ResolveArgs),
    /// Show vault statistics
    Stats(commands::stats_cmd::StatsArgs),
}

/// Runs the parsed CLI command and returns the process exit code.
pub fn run(cli: Cli) -> Result<ExitCode> {
    let ok = ExitCode::SUCCESS;
    match cli.command {
        Commands::Daily(args) => commands::daily_cmd::run(args).map(|()| ok),
        Commands::Fmt(args) => commands::fmt_cmd::run(args).map(|outcome| match outcome {
            commands::fmt_cmd::Outcome::Clean => ok,
            commands::fmt_cmd::Outcome::NeedsFormatting => ExitCode::from(1),
        }),
        Commands::Graph(args) => commands::graph_cmd::run(args).map(|()| ok),
        Commands::Index(args) => commands::index_cmd::run(args).map(|()| ok),
        Commands::List(args) => commands::list_cmd::run(args).map(|()| ok),
        Commands::Resolve(args) => commands::resolve_cmd::run(args).map(|()| ok),
        Commands::Stats(args) => commands::stats_cmd::run(args).map(|()| ok),
    }
}
