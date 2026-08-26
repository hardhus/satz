use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "satz", version, about = "Fast Markdown CLI")]
struct Cli {}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    Ok(())
}
