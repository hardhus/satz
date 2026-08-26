use tower_lsp_server::{LspService, Server};
use tracing_subscriber::EnvFilter;

mod backend;
mod convert;
mod handlers;
mod rank;
mod state;
mod sync;
mod watcher;

use backend::Backend;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::WARN.into()))
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
