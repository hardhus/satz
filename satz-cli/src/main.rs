use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser, Debug)]
#[command(
    name = "satz",
    version,
    about = "Fast Markdown knowledge-base CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Open or create today's daily note
    Daily(commands::daily_cmd::DailyArgs),
    /// Index vault and show summary
    Index(commands::index_cmd::IndexArgs),
    /// List documents by tag, orphan status, or broken links
    List(commands::list_cmd::ListArgs),
    /// Resolve a wikilink to its file path
    Resolve(commands::resolve_cmd::ResolveArgs),
    /// Show vault statistics
    Stats(commands::stats_cmd::StatsArgs),
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Daily(args) => commands::daily_cmd::run(args),
        Commands::Index(args) => commands::index_cmd::run(args),
        Commands::List(args) => commands::list_cmd::run(args),
        Commands::Resolve(args) => commands::resolve_cmd::run(args),
        Commands::Stats(args) => commands::stats_cmd::run(args),
    }
}
