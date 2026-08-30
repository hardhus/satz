pub mod commands;

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

/// Runs the parsed CLI command.
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Daily(args) => commands::daily_cmd::run(args),
        Commands::Fmt(args) => commands::fmt_cmd::run(args),
        Commands::Graph(args) => commands::graph_cmd::run(args),
        Commands::Index(args) => commands::index_cmd::run(args),
        Commands::List(args) => commands::list_cmd::run(args),
        Commands::Resolve(args) => commands::resolve_cmd::run(args),
        Commands::Stats(args) => commands::stats_cmd::run(args),
    }
}
