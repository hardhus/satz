use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams,
};

use crate::convert::uri_to_path;
use crate::state::SatzState;

use super::Backend;
use super::refresh::{
    publish_for, publish_for_all, refresh_after_reparse, refresh_diagnostics,
    refresh_semantic_tokens, send_diagnostics_refresh,
};

/// The day changed under the open notes (midnight passed, or the daily-note settings changed): a
/// `[[bugün]]` now reaches another note, so which daily note is an orphan, and how the links are
/// coloured, may differ. The client is told once, after the request that noticed it is done with
/// the state.
async fn announce_daily_change(client: Client, state: Arc<RwLock<SatzState>>) {
    // Everything the peers depend on is announced here; the next reparse has nothing to add.
    state.write().await.clear_peers_dirty();
    tokio::join!(
        super::refresh::refresh_open_documents(&client, &state),
        refresh_semantic_tokens(&client)
    );
}

/// The debounced reparse of one open document after a change: wait, snapshot the buffer, parse it
/// WITHOUT holding the state lock (readers keep working while a big note is parsed), then apply
/// the result if the buffer has not moved on -- a newer change has its own task -- and refresh
/// diagnostics.
async fn run_reparse(
    state_arc: Arc<RwLock<SatzState>>,
    client: Client,
    uri: String,
    delay: std::time::Duration,
) {
    tokio::time::sleep(delay).await;

    let Some(job) = ({ state_arc.read().await.prepare_reparse(&uri) }) else {
        // Nothing to parse. If a request brought the index up to date first (`read_fresh`), the
        // notifications that follow a reparse are still owed: they are sent from here.
        let peers = {
            let mut state = state_arc.write().await;
            if !state.take_announce_pending(&uri) {
                return;
            }
            state.take_peer_refresh(&uri, true)
        };
        announce_reparse(&client, &state_arc, &uri, peers).await;
        return;
    };
    let version = job.version;
    let parsed = tokio::task::spawn_blocking(move || {
        satz_core::parse_document_owned(job.content, &job.rel_path)
    })
    .await;
    let Ok(new_doc) = parsed else {
        tracing::error!(%uri, "reparse: the parsing task failed");
        return;
    };

    let (applied, peers) = {
        let mut state = state_arc.write().await;
        let applied = state.apply_reparse(&uri, version, new_doc);
        (applied, state.take_peer_refresh(&uri, applied))
    };
    if !applied {
        return; // the buffer changed while parsing; the task of that change takes over
    }

    announce_reparse(&client, &state_arc, &uri, peers).await;
}

/// What follows a reparse that reached the index: the document's own diagnostics, the other open
/// documents' when they depend on it, and the refresh requests to pull clients.
async fn announce_reparse(
    client: &Client,
    state_arc: &Arc<RwLock<SatzState>>,
    uri: &str,
    peers: crate::state::PeerRefresh,
) {
    publish_for(client, state_arc, uri).await;

    let plan = refresh_after_reparse(peers.dirty, peers.supports_pull);
    if plan.push_peers {
        publish_for_all(client, state_arc, &peers.others).await;
    }
    // Neither refresh waits for the other: a client that never answers one must not keep the
    // other from being sent.
    tokio::join!(refresh_semantic_tokens(client), async {
        if plan.pull_diagnostics {
            refresh_diagnostics(client).await;
        }
    });
}

impl Backend {
    /// Tells the other open documents' diagnostics to catch up, when what they depend on changed:
    /// a pull client is asked to fetch again, a push client is sent them.
    async fn refresh_peers(&self, peers: &crate::state::PeerRefresh) {
        if !peers.dirty {
            return;
        }
        send_diagnostics_refresh(
            &self.client,
            &self.state,
            peers.supports_pull,
            &peers.others,
        )
        .await;
    }

    /// Read access to a state whose index reflects every open buffer.
    ///
    /// The debounced reparse trails typing by a few hundred milliseconds; a request that maps the
    /// client's (live) positions through the index would then land on the wrong text. So when an
    /// open document is stale it is reparsed here first, under a short write lock. With nothing
    /// stale (the usual case) this is just a read lock.
    pub(crate) async fn read_fresh(&self) -> tokio::sync::RwLockReadGuard<'_, SatzState> {
        let today = chrono::Local::now().date_naive();
        {
            let state = self.state.read().await;
            if !state.has_stale_open_documents() && !state.daily_is_stale(today) {
                return state;
            }
        }
        let mut state = self.state.write().await;
        state.refresh_stale_open_documents();
        // Midnight passed (or the daily settings changed): `[[bugün]]` means another note now.
        let day_moved = state.sync_daily(today);
        // Downgraded, not released and re-acquired: a `did_change` slipping in between would make
        // the state the caller reads stale again.
        let read = state.downgrade();
        if day_moved {
            // Only the request that moves the day gets here (afterwards the index is current),
            // so the client is told once. It cannot be told while this request holds the state.
            tokio::spawn(announce_daily_change(
                self.client.clone(),
                self.state.clone(),
            ));
        }
        read
    }

    async fn publish_diagnostics_for_uri(&self, uri: &str) {
        publish_for(&self.client, &self.state, uri).await;
    }
}

pub(crate) async fn did_open(backend: &Backend, params: DidOpenTextDocumentParams) {
    let uri = params.text_document.uri.to_string();
    let content = params.text_document.text;
    let version = params.text_document.version;
    tracing::debug!(%uri, version, "did_open");
    let Some(path) = uri_to_path(&uri) else {
        tracing::warn!(%uri, "did_open: not a local file (untitled buffer, remote or malformed URI); ignoring");
        return;
    };

    let peers = {
        let mut state = backend.state.write().await;
        // A note opened first thing after midnight is read on the new day: whether a daily
        // note is an orphan depends on it. Moving the day marks the peers as changed, which
        // `take_peer_refresh` below turns into their refresh.
        state.sync_daily(chrono::Local::now().date_naive());
        state.open_document(&uri, &content, &path, version);
        state.take_peer_refresh(&uri, true)
    };

    backend.publish_diagnostics_for_uri(&uri).await;
    backend.refresh_peers(&peers).await;
}

pub(crate) async fn did_change(backend: &Backend, params: DidChangeTextDocumentParams) {
    let uri = params.text_document.uri.to_string();
    let version = params.text_document.version;
    tracing::trace!(%uri, version, "did_change");

    // Everything happens under ONE lock: the buffer is updated, the previous reparse task is
    // cancelled and the new one is spawned and stored. (Storing the handle under a second lock
    // let two simultaneous changes overwrite each other's handle and leave a task that nothing
    // could cancel any more.)
    let mut state = backend.state.write().await;
    let debounce = std::time::Duration::from_millis(state.config.lsp.reparse_debounce_ms);
    let max_wait = std::time::Duration::from_millis(state.config.lsp.reparse_max_wait_ms);

    let Some(open_doc) = state.open_docs.get_mut(&uri) else {
        return;
    };
    if !open_doc.apply_change_events(version, params.content_changes) {
        return;
    }

    let now = std::time::Instant::now();
    let first = open_doc.first_change_at.get_or_insert(now);
    let elapsed = now.duration_since(*first);
    let delay = crate::state::debounce_delay(debounce, max_wait, elapsed);

    if let Some(previous) = open_doc.pending_task.take() {
        previous.abort();
    }
    open_doc.pending_task = Some(tokio::task::spawn(run_reparse(
        backend.state.clone(),
        backend.client.clone(),
        uri,
        delay,
    )));
}

pub(crate) async fn did_save(backend: &Backend, params: DidSaveTextDocumentParams) {
    let uri = params.text_document.uri.to_string();
    tracing::debug!(%uri, "did_save");

    let (peers, prev_task) = {
        let mut state = backend.state.write().await;
        let prev = if let Some(open_doc) = state.open_docs.get_mut(&uri) {
            open_doc.pending_task.take()
        } else {
            None
        };
        state.reparse_open_document(&uri);
        (state.take_peer_refresh(&uri, true), prev)
    };

    if let Some(task) = prev_task {
        task.abort();
    }

    backend.publish_diagnostics_for_uri(&uri).await;
    backend.refresh_peers(&peers).await;
}

pub(crate) async fn did_close(backend: &Backend, params: DidCloseTextDocumentParams) {
    let uri = params.text_document.uri.to_string();
    let lsp_uri = params.text_document.uri;
    tracing::debug!(%uri, "did_close");
    let peers = {
        let mut state = backend.state.write().await;
        state.close_document(&uri);
        state.take_peer_refresh(&uri, true)
    };
    backend
        .client
        .publish_diagnostics(lsp_uri, vec![], None)
        .await;

    // Discarded unsaved edits change what the remaining documents' diagnostics should say.
    backend.refresh_peers(&peers).await;
}
