use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::Uri;
use tower_lsp_server::ls_types::request::{SemanticTokensRefresh, WorkspaceDiagnosticRefresh};

use crate::handlers::diagnostics::compute_diagnostics;
use crate::state::SatzState;

/// Logs a failed refresh request and says whether it succeeded. Nothing is retried: the client refetches on its own schedule anyway.
pub(crate) fn refresh_succeeded<T, E: std::fmt::Display>(
    what: &str,
    result: &Result<T, E>,
) -> bool {
    match result {
        Ok(_) => true,
        Err(error) => {
            // Not a warning: a client that never announced the request may simply not know it.
            tracing::debug!(request = what, %error, "the client did not accept a refresh request");
            false
        }
    }
}

/// How long a refresh request may wait for the client's answer.
pub(crate) const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How a refresh request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    Answered,
    Failed,
    TimedOut,
}

/// Sends a refresh request and waits for the answer, at most `limit`: a client that never answers
/// must not hold back what comes after (the other refresh, for instance).
pub(crate) async fn send_refresh<F, T, E>(
    what: &str,
    request: F,
    limit: std::time::Duration,
) -> RefreshOutcome
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let started = std::time::Instant::now();
    tracing::debug!(request = what, "sending a refresh request");
    match tokio::time::timeout(limit, request).await {
        Ok(result) => {
            let ok = refresh_succeeded(what, &result);
            tracing::debug!(request = what, ok, took = ?started.elapsed(), "refresh request answered");
            if ok {
                RefreshOutcome::Answered
            } else {
                RefreshOutcome::Failed
            }
        }
        Err(_) => {
            tracing::debug!(request = what, after = ?limit, "refresh request not answered in time");
            RefreshOutcome::TimedOut
        }
    }
}

/// Asks a pull-diagnostics client to fetch again. The caller has decided that the client pulls; what
/// the client announced about refreshing does not matter: many clients answer the request (or ignore
/// it harmlessly) without announcing `refreshSupport`, and the refresh after the first indexing is what
/// makes them fetch again.
pub(crate) async fn refresh_diagnostics(client: &Client) {
    send_refresh(
        "workspace/diagnostic/refresh",
        client.send_request::<WorkspaceDiagnosticRefresh>(()),
        REFRESH_TIMEOUT,
    )
    .await;
}

/// Publishes the diagnostics of each of the notes `uris`: what a push client is sent when what
/// they show may have changed.
pub(crate) async fn publish_for_all(
    client: &Client,
    state: &Arc<RwLock<SatzState>>,
    uris: &[String],
) {
    for uri in uris {
        publish_for(client, state, uri).await;
    }
}

/// Tells the client that the diagnostics of the notes `uris` may have changed: a pull client is
/// asked to fetch them again, a push client is sent them.
pub(crate) async fn send_diagnostics_refresh(
    client: &Client,
    state: &Arc<RwLock<SatzState>>,
    supports_pull: bool,
    uris: &[String],
) {
    if supports_pull {
        refresh_diagnostics(client).await;
    } else {
        publish_for_all(client, state, uris).await;
    }
}

/// Asks the client to fetch semantic tokens again: every client is sent this, whatever it announced.
/// The colours of a note opened during the first indexing are computed from an incomplete index
/// (links look unresolved); this request is what makes the client ask again once it is complete. It
/// must not depend on what the client announced (`refreshSupport`, or the semantic token
/// capability): a client that announces neither but still answers it (Helix) would keep the wrong
/// colours. A client that does not know the request just answers with an error, which is logged at
/// debug level.
pub(crate) async fn refresh_semantic_tokens(client: &Client) {
    send_refresh(
        "workspace/semanticTokens/refresh",
        client.send_request::<SemanticTokensRefresh>(()),
        REFRESH_TIMEOUT,
    )
    .await;
}

/// Tells the client that what the open notes show may have changed under them: a pull client is
/// asked to fetch again, a push client is sent the diagnostics of every open note.
pub(crate) async fn refresh_open_documents(client: &Client, state: &Arc<RwLock<SatzState>>) {
    let (supports_pull, uris) = {
        let s = state.read().await;
        (
            s.client_supports_pull_diagnostics,
            s.open_docs.keys().cloned().collect::<Vec<_>>(),
        )
    };
    send_diagnostics_refresh(client, state, supports_pull, &uris).await;
}

/// Computes diagnostics for the specified open document URI and sends them to the client.
pub(crate) async fn publish_for(client: &Client, state: &Arc<RwLock<SatzState>>, uri: &str) {
    let (diagnostics, uri_obj) = {
        let state_guard = state.read().await;

        if state_guard.client_supports_pull_diagnostics {
            return;
        }

        if !state_guard.is_indexing_complete() {
            tracing::debug!(
                uri,
                "publish_for: initial indexing not complete yet, skipping"
            );
            return;
        }

        let Some(open_doc) = state_guard.open_docs.get(uri) else {
            return;
        };
        let doc_id = state_guard.doc_id_for_path(&open_doc.path);

        let Some(doc) = state_guard.index.get_doc(&doc_id) else {
            return;
        };

        let diags = compute_diagnostics(doc, &state_guard.index, &state_guard.config);
        let uri_obj = match uri.parse::<Uri>() {
            Ok(u) => u,
            Err(_) => return,
        };
        (diags, uri_obj)
    };

    client.publish_diagnostics(uri_obj, diagnostics, None).await;
}

/// What to tell the client after an open document has been re-parsed. (`workspace/semanticTokens/
/// refresh` is not part of the plan: every client is sent it.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefreshPlan {
    /// Send `workspace/diagnostic/refresh` so a pull client fetches the new results.
    pub pull_diagnostics: bool,
    /// Publish diagnostics for the other open documents (push clients).
    pub push_peers: bool,
}

/// Decides the notifications after a debounced re-parse.
///
/// The client fetches diagnostics and tokens right after each edit -- before the debounced
/// re-parse has updated the index -- so it always holds results one edit old. A pull client is
/// therefore asked to fetch again after EVERY re-parse (not only when other documents are
/// affected), and so is the colouring of every client. A push client already gets its own
/// document's diagnostics published; other documents only when what they depend on changed.
pub(crate) fn refresh_after_reparse(peers_dirty: bool, supports_pull: bool) -> RefreshPlan {
    RefreshPlan {
        pull_diagnostics: supports_pull,
        push_peers: peers_dirty && !supports_pull,
    }
}
