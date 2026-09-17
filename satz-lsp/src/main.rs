use satz_lsp::backend::Backend;
use tower_lsp_server::{LspService, Server};
use tracing_subscriber::{EnvFilter, prelude::*, reload};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging is off by default and stays off unless the client opts in via
    // `initializationOptions.logLevel` (see `Backend::initialize`) — no env
    // var, no rebuild needed to change verbosity mid-session.
    let (filter_layer, reload_handle) = reload::Layer::new(EnvFilter::new("off"));
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) =
        LspService::new(move |client| Backend::new(client, reload_handle.clone()));
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
