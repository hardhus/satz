use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;
use satz_core::VaultGraph;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GraphFormat {
    Dot,
    Json,
}

#[derive(clap::Args, Debug)]
pub struct GraphArgs {
    /// Vault root directory
    #[arg(long, short = 'v', default_value = ".")]
    pub vault: PathBuf,

    /// Output format: dot or json
    #[arg(long, short = 'f', value_enum, default_value_t = GraphFormat::Json)]
    pub format: GraphFormat,

    /// Output file path (defaults to stdout)
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
}

pub fn run(args: GraphArgs) -> Result<()> {
    super::with_stdout(|out| run_with_output(args, out))
}

/// `run`, writing the graph to `out` unless `--output` names a file (the summary of that goes to
/// stderr).
pub fn run_with_output(args: GraphArgs, out: &mut dyn Write) -> Result<()> {
    let index = super::load_index(&args.vault)?;
    let graph = VaultGraph::build(&index);

    let output_str = match args.format {
        GraphFormat::Dot => graph.export_dot(),
        GraphFormat::Json => graph.export_json()?,
    };

    if let Some(out_path) = args.output {
        std::fs::write(&out_path, output_str)?;
        eprintln!(
            "Graph exported to {} ({} nodes, {} edges)",
            out_path.display(),
            graph.node_count(),
            graph.edge_count()
        );
    } else {
        writeln!(out, "{}", output_str)?;
    }

    Ok(())
}
