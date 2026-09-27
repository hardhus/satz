use super::*;
// Not one of `backend`'s own items (`super::*` no longer carries it now that `backend.rs` itself
// has no direct use for it): a helper test computes diagnostics itself to compare against.
use crate::handlers::diagnostics::compute_diagnostics;

#[test]
fn a_pull_client_is_always_asked_to_refetch_after_a_reparse() {
    for peers_dirty in [false, true] {
        let plan = refresh_after_reparse(peers_dirty, true);
        assert!(plan.pull_diagnostics, "peers_dirty={peers_dirty}");
        assert!(!plan.push_peers, "a pull client is not pushed to");
    }
}

#[test]
fn a_push_client_gets_peers_only_when_they_are_affected() {
    let clean = refresh_after_reparse(false, false);
    assert!(!clean.pull_diagnostics && !clean.push_peers);
    let dirty = refresh_after_reparse(true, false);
    assert!(!dirty.pull_diagnostics && dirty.push_peers);
}

// ---- the server's advertised commands and how execute_command answers them ----

/// A real `Backend` (its client end is never connected: the paths tested here do not talk to
/// the client).
fn test_service() -> tower_lsp_server::LspService<Backend> {
    let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
    let (service, _socket) =
        tower_lsp_server::LspService::new(|client| Backend::new(client, handle));
    service
}

fn command(name: &str, arguments: Vec<serde_json::Value>) -> ExecuteCommandParams {
    ExecuteCommandParams {
        command: name.to_string(),
        arguments,
        work_done_progress_params: Default::default(),
    }
}

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }
}

#[test]
fn the_server_advertises_exactly_the_supported_commands() {
    let caps = server_capabilities();
    let commands = caps
        .execute_command_provider
        .expect("execute commands are offered")
        .commands;
    let expected: Vec<String> = crate::handlers::execute_command::SUPPORTED_COMMANDS
        .iter()
        .map(|c| c.to_string())
        .collect();
    assert_eq!(commands, expected);
    assert!(commands.contains(&"satz.showBacklinks".to_string()));
    assert!(commands.contains(&"satz.formatWorkspace".to_string()));
}

#[tokio::test]
async fn show_backlinks_answers_with_the_locations_and_rejects_bad_arguments() {
    let service = test_service();
    let backend = service.inner();
    {
        let mut state = backend.state.write().await;
        state.set_vault_root(Some(root()));
        state.index = satz_core::Index::build(vec![
            satz_core::parse_document("# A\n", std::path::Path::new("a.md")),
            satz_core::parse_document("see [[a]]\n", std::path::Path::new("b.md")),
        ]);
    }
    let uri = crate::convert::path_to_uri(&root().join("a.md"))
        .unwrap()
        .as_str()
        .to_string();

    let answer = backend
        .execute_command(command("satz.showBacklinks", vec![serde_json::json!(uri)]))
        .await
        .expect("valid arguments")
        .expect("a value");
    let list = answer.as_array().expect("an array of locations");
    assert_eq!(list.len(), 1);
    assert!(list[0]["uri"].as_str().unwrap().ends_with("b.md"));

    for bad in [
        vec![],
        vec![serde_json::json!(42)],
        vec![serde_json::json!(null)],
        vec![serde_json::json!("")],
        vec![serde_json::json!("not a uri")],
    ] {
        let err = backend
            .execute_command(command("satz.showBacklinks", bad.clone()))
            .await
            .expect_err(&format!("{bad:?} must be rejected"));
        assert_eq!(err.code, jsonrpc::ErrorCode::InvalidParams, "{bad:?}");
        assert!(!err.message.is_empty());
    }
}

#[tokio::test]
async fn an_unknown_or_differently_cased_command_is_method_not_found() {
    let service = test_service();
    let backend = service.inner();
    for name in [
        "satz.nope",
        "",
        "SATZ.SHOWBACKLINKS",
        "satz.showbacklinks",
        "satz.showBacklinks ",
    ] {
        let err = backend
            .execute_command(command(name, vec![]))
            .await
            .expect_err(&format!("{name:?} is not a command"));
        assert_eq!(err.code, jsonrpc::ErrorCode::MethodNotFound, "{name:?}");
    }
}

// ---- did_change: one reparse task per document, whoever wins the lock ----

fn change_params(uri: &str, version: i32, text: &str) -> DidChangeTextDocumentParams {
    DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.parse().unwrap(),
            version,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }],
    }
}

/// A backend that can be shared between tasks, with `a.md` open and a debounce so long that no
/// reparse ever fires during a test.
async fn shared_backend() -> (Arc<Backend>, tower_lsp_server::LspService<Backend>) {
    let client_slot = std::sync::Mutex::new(None);
    let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
    let (service, _socket) = tower_lsp_server::LspService::new(|client| {
        *client_slot.lock().unwrap() = Some(client.clone());
        Backend::new(client, handle)
    });
    let client = client_slot.lock().unwrap().take().unwrap();
    let (_layer2, handle2): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
    let backend = Arc::new(Backend::new(client, handle2));
    {
        let mut state = backend.state.write().await;
        state.set_vault_root(Some(root()));
        state.config.lsp.reparse_debounce_ms = 600_000;
        state.config.lsp.reparse_max_wait_ms = 600_000;
        state.set_indexing_complete(true);
        state.open_document("file:///a.md", "# A\n", &root().join("a.md"), 1);
    }
    (backend, service)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_simultaneous_changes_leave_exactly_one_pending_reparse() {
    let (backend, _service) = shared_backend().await;
    let alive_before = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();

    let mut changes = Vec::new();
    for i in 0..64 {
        let backend = backend.clone();
        changes.push(tokio::spawn(async move {
            backend
                .did_change(change_params(
                    "file:///a.md",
                    2,
                    &format!("# A\n\n[[n{i}]]\n"),
                ))
                .await;
        }));
    }
    for change in changes {
        change.await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let alive_after = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    assert_eq!(
        alive_after - alive_before,
        1,
        "every earlier reparse must have been cancelled: only the stored one may live"
    );
    let state = backend.state.read().await;
    let pending = state.open_docs["file:///a.md"].pending_task.as_ref();
    assert!(pending.is_some_and(|task| !task.is_finished()));
}

#[tokio::test]
async fn a_change_stores_its_reparse_task_before_it_returns() {
    let (backend, _service) = shared_backend().await;
    backend
        .did_change(change_params("file:///a.md", 2, "# A\n\nfirst\n"))
        .await;
    let first = {
        let state = backend.state.read().await;
        let task = state.open_docs["file:///a.md"]
            .pending_task
            .as_ref()
            .unwrap();
        task.abort_handle()
    };
    backend
        .did_change(change_params("file:///a.md", 3, "# A\n\nsecond\n"))
        .await;
    // The first task was cancelled by the second change and a new one is stored.
    for _ in 0..50 {
        if first.is_finished() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(first.is_finished());
    let state = backend.state.read().await;
    let second = state.open_docs["file:///a.md"]
        .pending_task
        .as_ref()
        .unwrap();
    assert!(!second.is_finished());
}

#[tokio::test]
async fn changes_that_cannot_be_applied_start_no_task() {
    let (backend, _service) = shared_backend().await;
    // Unknown document.
    backend
        .did_change(change_params("file:///unknown.md", 2, "x"))
        .await;
    // A version older than the buffer's is stale.
    backend
        .did_change(change_params("file:///a.md", 5, "# A\n\nfive\n"))
        .await;
    let first = {
        let state = backend.state.read().await;
        state.open_docs["file:///a.md"]
            .pending_task
            .as_ref()
            .unwrap()
            .abort_handle()
    };
    backend
        .did_change(change_params("file:///a.md", 4, "# A\n\nstale\n"))
        .await;
    let state = backend.state.read().await;
    assert!(
        !first.is_finished(),
        "a stale change must not cancel the pending task"
    );
    assert_eq!(
        state.open_docs["file:///a.md"].rope.to_string(),
        "# A\n\nfive\n"
    );
}

#[test]
fn versioned_edits_are_used_only_when_the_client_says_it_supports_them() {
    let mut caps = ClientCapabilities::default();
    assert!(!client_supports_document_changes(&caps));
    caps.workspace = Some(WorkspaceClientCapabilities::default());
    assert!(!client_supports_document_changes(&caps));
    caps.workspace.as_mut().unwrap().workspace_edit = Some(WorkspaceEditClientCapabilities {
        document_changes: Some(false),
        ..Default::default()
    });
    assert!(!client_supports_document_changes(&caps));
    caps.workspace.as_mut().unwrap().workspace_edit = Some(WorkspaceEditClientCapabilities {
        document_changes: Some(true),
        ..Default::default()
    });
    assert!(client_supports_document_changes(&caps));
}

// ---- refresh requests only go to clients that said they understand them ----

#[test]
fn a_failed_refresh_request_is_reported_not_swallowed() {
    let ok: Result<(), String> = Ok(());
    assert!(refresh_succeeded("workspace/diagnostic/refresh", &ok));
    let failed: Result<(), String> = Err("method not found".to_string());
    assert!(!refresh_succeeded("workspace/diagnostic/refresh", &failed));
}

// ---- requests see the buffer, not the last debounced parse ----

fn type_full(state: &mut SatzState, uri: &str, version: i32, text: &str) {
    let open = state.open_docs.get_mut(uri).unwrap();
    assert!(open.apply_change_events(
        version,
        vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }],
    ));
}

fn position_params(uri: &str, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
            uri: uri.parse().unwrap(),
        },
        position: Position::new(line, character),
    }
}

/// `a.md` is open; its buffer already has a link to `b.md` that the index does not know yet.
async fn backend_with_an_unparsed_link() -> (Arc<Backend>, tower_lsp_server::LspService<Backend>) {
    let (backend, service) = shared_backend().await;
    {
        let mut state = backend.state.write().await;
        state.index.replace_doc(satz_core::parse_document(
            "# B\n\nbody of b\n",
            std::path::Path::new("b.md"),
        ));
        type_full(&mut state, "file:///a.md", 2, "# A\n\nsee [[b]] here\n");
        assert!(state.has_stale_open_documents());
    }
    (backend, service)
}

#[tokio::test]
async fn go_to_definition_follows_a_link_typed_a_moment_ago() {
    let (backend, _service) = backend_with_an_unparsed_link().await;
    let found = backend
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: position_params("file:///a.md", 2, 7),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    let Some(GotoDefinitionResponse::Scalar(location)) = found else {
        panic!("the fresh link must resolve: {found:?}");
    };
    assert!(location.uri.as_str().ends_with("b.md"));
}

#[tokio::test]
async fn hover_shows_the_note_a_freshly_typed_link_points_at() {
    let (backend, _service) = backend_with_an_unparsed_link().await;
    let hover = backend
        .hover(HoverParams {
            text_document_position_params: position_params("file:///a.md", 2, 7),
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("a hover for the fresh link");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!()
    };
    assert!(markup.value.contains("body of b"), "{}", markup.value);
}

#[tokio::test]
async fn references_and_highlights_use_the_buffers_positions() {
    let (backend, _service) = backend_with_an_unparsed_link().await;
    let highlights = backend
        .document_highlight(DocumentHighlightParams {
            text_document_position_params: position_params("file:///a.md", 2, 7),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("the link is highlighted");
    assert_eq!(highlights.len(), 1);
    assert_eq!(highlights[0].range.start, Position::new(2, 4));
    assert_eq!(highlights[0].range.end, Position::new(2, 9));
}

#[tokio::test]
async fn folding_uses_the_lines_of_the_buffer() {
    let (backend, _service) = shared_backend().await;
    {
        let mut state = backend.state.write().await;
        type_full(
            &mut state,
            "file:///a.md",
            2,
            "# A\n\ntext\n\n## Sub\n\nmore\n",
        );
    }
    let folds = backend
        .folding_range(FoldingRangeParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///a.md".parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        folds
            .iter()
            .map(|f| (f.start_line, f.end_line))
            .collect::<Vec<_>>(),
        vec![(0, 6), (4, 6)]
    );
}

#[tokio::test]
async fn a_rename_lands_on_the_right_lines_after_the_buffer_moved() {
    let (backend, _service) = shared_backend().await;
    let old = "# Old\n\n[[a#Old]]\n";
    let typed = "\n\n# Old\n\n[[a#Old]]\n"; // two lines added above; not reparsed yet
    {
        let mut state = backend.state.write().await;
        state.open_document("file:///a.md", old, &root().join("a.md"), 1);
        type_full(&mut state, "file:///a.md", 2, typed);
    }
    let edit = backend
        .rename(RenameParams {
            text_document_position: position_params("file:///a.md", 2, 3),
            new_name: "New".to_string(),
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("an edit");
    let Some(DocumentChanges::Operations(ops)) = edit.document_changes else {
        panic!("versioned edits expected: {edit:?}");
    };
    let mut result = typed.to_string();
    let mut seen_version = None;
    for op in ops {
        let DocumentChangeOperation::Edit(doc_edit) = op else {
            continue;
        };
        seen_version = doc_edit.text_document.version;
        let edits: Vec<TextEdit> = doc_edit
            .edits
            .into_iter()
            .map(|e| match e {
                OneOf::Left(t) => t,
                OneOf::Right(a) => a.text_edit,
            })
            .collect();
        result = crate::convert::apply_text_edits(&result, &edits);
    }
    assert_eq!(result, "\n\n# New\n\n[[a#New]]\n");
    assert_eq!(
        seen_version,
        Some(2),
        "the edit names the buffer version it was computed for"
    );
}

#[tokio::test]
async fn requests_without_a_stale_buffer_do_not_take_the_write_lock() {
    let (backend, _service) = shared_backend().await;
    let read = backend.read_fresh().await;
    // Fast path: with a read guard held, nobody could have refreshed under a write lock.
    assert!(backend.state.try_write().is_err());
    assert!(!read.has_stale_open_documents());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_requests_and_edits_stay_consistent() {
    let (backend, _service) = shared_backend().await;
    let mut jobs = Vec::new();
    for i in 0..24 {
        let backend = backend.clone();
        jobs.push(tokio::spawn(async move {
            if i % 3 == 0 {
                backend
                    .did_change(change_params(
                        "file:///a.md",
                        2 + i,
                        &format!("# A\n\n[[n{i}]]\n"),
                    ))
                    .await;
            } else {
                let _ = backend
                    .hover(HoverParams {
                        text_document_position_params: position_params("file:///a.md", 2, 3),
                        work_done_progress_params: Default::default(),
                    })
                    .await;
            }
        }));
    }
    for job in jobs {
        tokio::time::timeout(std::time::Duration::from_secs(10), job)
            .await
            .expect("no deadlock")
            .unwrap();
    }
    // A last request leaves the index in step with the buffer.
    let _ = backend
        .hover(HoverParams {
            text_document_position_params: position_params("file:///a.md", 2, 3),
            work_done_progress_params: Default::default(),
        })
        .await;
    assert!(!backend.state.read().await.has_stale_open_documents());
}

/// `a.md` and `p.md` (which links to `a`) are open, with a debounce short enough to fire
/// inside a test.
async fn backend_with_a_peer_and_a_short_debounce()
-> (Arc<Backend>, tower_lsp_server::LspService<Backend>) {
    let (backend, service) = shared_backend().await;
    backend
        .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
        .await;
    {
        let mut state = backend.state.write().await;
        state.config.lsp.reparse_debounce_ms = 40;
        state.config.lsp.reparse_max_wait_ms = 40;
    }
    (backend, service)
}

#[tokio::test]
async fn a_request_before_the_debounce_leaves_the_peer_flag_to_the_debounced_task() {
    // The request reparses the stale buffer itself (`read_fresh`); the debounced task then
    // finds nothing to parse. It must still tell the other open documents that what they
    // depend on (here: the title `a` is linked by) changed -- and consume the flag.
    let (backend, _service) = backend_with_a_peer_and_a_short_debounce().await;
    backend
        .did_change(change_params("file:///a.md", 2, "# Renamed\n"))
        .await;
    let _ = backend
        .hover(HoverParams {
            text_document_position_params: position_params("file:///a.md", 0, 3),
            work_done_progress_params: Default::default(),
        })
        .await;
    assert!(
        backend.state.read().await.peers_dirty(),
        "the request's own reparse changed what peers depend on"
    );

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    assert!(
        !backend.state.read().await.peers_dirty(),
        "the debounced task must have told the peers and consumed the flag"
    );
}

// ---- what the client actually receives (a real JSON-RPC connection over an in-memory pipe) ----

pub(crate) async fn write_frame(
    out: &mut (impl tokio::io::AsyncWrite + Unpin),
    message: serde_json::Value,
) {
    use tokio::io::AsyncWriteExt;
    let body = message.to_string();
    let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
    out.write_all(frame.as_bytes()).await.unwrap();
}

pub(crate) async fn read_frame(
    input: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

/// Everything the server sends for `quiet` without a pause: `(method, uri of the document)`.
pub(crate) async fn sent_meanwhile(
    input: &mut (impl tokio::io::AsyncBufRead + Unpin),
    quiet: std::time::Duration,
) -> Vec<(String, Option<String>)> {
    let mut seen = Vec::new();
    while let Ok(Some(frame)) = tokio::time::timeout(quiet, read_frame(input)).await {
        if let Some(method) = frame["method"].as_str() {
            let uri = frame["params"]["uri"].as_str().map(str::to_string);
            seen.push((method.to_string(), uri));
        }
    }
    seen
}

/// `shared_backend`'s setup (a.md open, indexing done) but with a connected push client
/// (it announced no pull-diagnostics support) whose incoming messages can be read.
pub(crate) async fn connected_backend() -> (
    Arc<Backend>,
    tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) {
    let (backend, from_server, to_server) = connected_backend_io().await;
    // Kept open for as long as the test (leaked: a closed pipe would end the server).
    std::mem::forget(to_server);
    (backend, from_server)
}

/// `connected_backend`, and the way back to the server as well: for a client that answers what
/// the server asks of it.
pub(crate) async fn connected_backend_io() -> (
    Arc<Backend>,
    tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
) {
    let client_slot = std::sync::Mutex::new(None);
    let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
    let (service, socket) = tower_lsp_server::LspService::new(|client| {
        *client_slot.lock().unwrap() = Some(client.clone());
        Backend::new(client, handle)
    });
    let client = client_slot.lock().unwrap().take().unwrap();
    let (client_io, server_io) = tokio::io::duplex(1 << 20);
    let (server_read, server_write) = tokio::io::split(server_io);
    tokio::spawn(async move {
        tower_lsp_server::Server::new(server_read, server_write, socket)
            .serve(service)
            .await;
    });
    let (client_read, mut client_write) = tokio::io::split(client_io);
    let mut client_read = tokio::io::BufReader::new(client_read);

    // The handshake: a client is only sent anything once it has initialized the server.
    write_frame(
        &mut client_write,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"processId": null, "rootUri": null, "capabilities": {}}}),
    )
    .await;
    loop {
        let frame = read_frame(&mut client_read)
            .await
            .expect("initialize answer");
        if frame["id"] == 1 {
            break;
        }
    }
    write_frame(
        &mut client_write,
        serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    )
    .await;
    let (_layer2, handle2): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
    let backend = Arc::new(Backend::new(client, handle2));
    {
        let mut state = backend.state.write().await;
        state.set_vault_root(Some(root()));
        state.set_indexing_complete(true);
        state.open_document("file:///a.md", "# A\n", &root().join("a.md"), 1);
    }
    (backend, client_read, client_write)
}

#[tokio::test]
async fn a_request_before_the_debounce_still_gets_its_diagnostics_and_refreshes_sent() {
    let (backend, mut from_server) = connected_backend().await;
    backend
        .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
        .await;
    {
        let mut state = backend.state.write().await;
        state.config.lsp.reparse_debounce_ms = 40;
        state.config.lsp.reparse_max_wait_ms = 40;
    }
    // What the handshake and the opening sent is not what is asserted below.
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

    backend
        .did_change(change_params("file:///a.md", 2, "# Renamed\n"))
        .await;
    let _ = backend
        .hover(HoverParams {
            text_document_position_params: position_params("file:///a.md", 0, 3),
            work_done_progress_params: Default::default(),
        })
        .await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;

    let published = |uri: &str| {
        sent.iter().any(|(method, target)| {
            method == "textDocument/publishDiagnostics" && target.as_deref() == Some(uri)
        })
    };
    assert!(
        published("file:///a.md"),
        "the edited note's own diagnostics: {sent:?}"
    );
    assert!(
        published("file:///p.md"),
        "the open note that links to the renamed one: {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|(method, _)| method == "workspace/semanticTokens/refresh"),
        "the client is asked for fresh colours: {sent:?}"
    );
}

#[tokio::test]
async fn the_diagnostics_of_an_open_note_in_a_folder_are_published() {
    // The path of a note in a folder has the separator of the platform; its id has `/`.
    let (backend, mut from_server) = connected_backend().await;
    let uri = crate::convert::path_to_uri(&root().join(crate::convert::native_path("sub/n.md")))
        .unwrap()
        .as_str()
        .to_string();
    backend
        .did_open(open_params(
            &uri,
            1,
            "# N

[[missing]]
",
        ))
        .await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;
    assert!(
        sent.iter().any(|(method, target)| {
            method == "textDocument/publishDiagnostics" && target.as_deref() == Some(uri.as_str())
        }),
        "{sent:?}"
    );
}

/// Unlike an ordinary vault root, a UNC one (`\\server\share\...`) really does round-trip
/// through a URI with backslashes still in it (`convert::uri_to_path` puts them back for a
/// host it finds in the URI): the one case that puts `publish_for`'s own `\` -> `/` the same
/// way to work, an open note in a subfolder of it.
#[tokio::test]
async fn the_diagnostics_of_an_open_note_in_a_unc_folder_are_published() {
    if !cfg!(windows) {
        return; // UNC paths are a Windows concept; `uri_to_path` only builds one there.
    }
    let (backend, mut from_server) = connected_backend().await;
    let unc_root = std::path::PathBuf::from("\\\\server\\share\\vault");
    {
        let mut state = backend.state.write().await;
        state.set_vault_root(Some(unc_root.clone()));
    }
    let uri = crate::convert::path_to_uri(&unc_root.join(crate::convert::native_path("sub/n.md")))
        .unwrap()
        .as_str()
        .to_string();
    backend
        .did_open(open_params(
            &uri,
            1,
            "# N

[[missing]]
",
        ))
        .await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;
    assert!(
        sent.iter().any(|(method, target)| {
            method == "textDocument/publishDiagnostics" && target.as_deref() == Some(uri.as_str())
        }),
        "{sent:?}"
    );
}

#[tokio::test]
async fn without_a_request_the_debounced_task_announces_once() {
    // The ordinary path, which must not change: typing, the debounce fires, one round of
    // notifications (no duplicate from the request-refresh debt).
    let (backend, mut from_server) = connected_backend().await;
    backend
        .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
        .await;
    {
        let mut state = backend.state.write().await;
        state.config.lsp.reparse_debounce_ms = 40;
        state.config.lsp.reparse_max_wait_ms = 40;
    }
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

    backend
        .did_change(change_params("file:///a.md", 2, "# Renamed\n"))
        .await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;

    let count = |method: &str, uri: Option<&str>| {
        sent.iter()
            .filter(|(m, u)| m == method && (uri.is_none() || u.as_deref() == uri))
            .count()
    };
    assert_eq!(
        count("textDocument/publishDiagnostics", Some("file:///a.md")),
        1,
        "{sent:?}"
    );
    assert_eq!(
        count("textDocument/publishDiagnostics", Some("file:///p.md")),
        1,
        "{sent:?}"
    );
    assert_eq!(
        count("workspace/semanticTokens/refresh", None),
        1,
        "{sent:?}"
    );
}

// ---- debounce and max-wait, through the real `did_change` ----

/// The title the index holds for the open `a.md`.
async fn indexed_title_of_a(backend: &Backend) -> String {
    let state = backend.state.read().await;
    state
        .index
        .get_doc(&satz_core::DocId::new("a.md"))
        .expect("a.md is indexed")
        .title
        .clone()
}

#[tokio::test]
async fn quick_successive_changes_are_parsed_once_with_the_last_text() {
    let (backend, _service) = shared_backend().await;
    {
        let mut state = backend.state.write().await;
        state.config.lsp.reparse_debounce_ms = 50;
        state.config.lsp.reparse_max_wait_ms = 5_000;
    }
    for version in 2..=4 {
        backend
            .did_change(change_params(
                "file:///a.md",
                version,
                &format!("# Version {version}\n"),
            ))
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        indexed_title_of_a(&backend).await,
        "A",
        "the debounce has not run out yet: the index still holds the old text"
    );

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    assert_eq!(indexed_title_of_a(&backend).await, "Version 4");
    let state = backend.state.read().await;
    assert!(!state.has_stale_open_documents());
    assert!(
        state.open_docs["file:///a.md"]
            .pending_task
            .as_ref()
            .is_some_and(|task| task.is_finished()),
        "only the reparse of the last change was left, and it is done"
    );
}

#[tokio::test]
async fn typing_without_a_pause_is_still_parsed_once_the_max_wait_is_up() {
    // Every change comes sooner (30 ms) than the debounce (100 ms) would fire, so without the
    // max-wait (250 ms) the index would not move until the typing stops.
    let (backend, _service) = shared_backend().await;
    {
        let mut state = backend.state.write().await;
        state.config.lsp.reparse_debounce_ms = 100;
        state.config.lsp.reparse_max_wait_ms = 250;
    }
    let mut title_while_typing = String::new();
    for i in 0..14 {
        backend
            .did_change(change_params(
                "file:///a.md",
                2 + i,
                &format!("# Typing {i}\n"),
            ))
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        if i == 10 {
            // ~330 ms in and still typing: the max-wait fired at ~250 ms.
            title_while_typing = indexed_title_of_a(&backend).await;
        }
    }
    assert!(
        title_while_typing.starts_with("Typing"),
        "the index never moved while typing went on: {title_while_typing:?}"
    );

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(indexed_title_of_a(&backend).await, "Typing 13");
    assert!(!backend.state.read().await.has_stale_open_documents());
}

#[tokio::test]
async fn a_request_after_midnight_moves_the_daily_alias_to_the_new_day() {
    let (backend, _service) = shared_backend().await;
    let today = chrono::Local::now().date_naive();
    {
        let mut state = backend.state.write().await;
        let config = state.config.daily_note.clone();
        state
            .index
            .set_daily(Some((config, today.pred_opt().unwrap())));
        assert!(state.daily_is_stale(today));
    }
    let read = backend.read_fresh().await;
    assert_eq!(read.index.daily().map(|(_, d)| *d), Some(today));
    assert!(!read.daily_is_stale(today));
}

// ---- the document lifecycle through the real handlers ----

fn open_params(uri: &str, version: i32, text: &str) -> DidOpenTextDocumentParams {
    DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.parse().unwrap(),
            language_id: "markdown".to_string(),
            version,
            text: text.to_string(),
        },
    }
}

fn close_params(uri: &str) -> DidCloseTextDocumentParams {
    DidCloseTextDocumentParams {
        text_document: TextDocumentIdentifier {
            uri: uri.parse().unwrap(),
        },
    }
}

fn range_change(uri: &str, version: i32, range: Range, text: &str) -> DidChangeTextDocumentParams {
    DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.parse().unwrap(),
            version,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: Some(range),
            range_length: None,
            text: text.to_string(),
        }],
    }
}

#[tokio::test]
async fn opening_a_note_indexes_its_buffer_and_closing_it_forgets_a_note_without_a_file() {
    let (backend, _service) = shared_backend().await;
    // Inside the vault root, so the path is the same on every operating system.
    let uri = crate::convert::path_to_uri(&root().join("new-note.md"))
        .unwrap()
        .to_string();
    backend
        .did_open(open_params(&uri, 1, "# New\n\n[[a]]\n"))
        .await;
    {
        let state = backend.state.read().await;
        assert!(state.open_docs.contains_key(&uri));
        assert!(
            state
                .index
                .get_doc(&satz_core::DocId::new("new-note.md"))
                .is_some()
        );
        assert_eq!(
            state
                .index
                .backlinks_of(&satz_core::DocId::new("a.md"))
                .count(),
            1
        );
    }
    backend.did_close(close_params(&uri)).await;
    let state = backend.state.read().await;
    assert!(!state.open_docs.contains_key(&uri));
    assert!(
        state
            .index
            .get_doc(&satz_core::DocId::new("new-note.md"))
            .is_none(),
        "no file on disk: the unsaved note is gone with its buffer"
    );
    assert_eq!(
        state
            .index
            .backlinks_of(&satz_core::DocId::new("a.md"))
            .count(),
        0
    );
}

#[tokio::test]
async fn a_buffer_that_is_not_a_local_file_is_ignored() {
    let (backend, _service) = shared_backend().await;
    let before = backend.state.read().await.open_docs.len();
    backend
        .did_open(open_params("untitled:Untitled-1", 1, "# X\n"))
        .await;
    backend
        .did_open(open_params("https://example.com/x.md", 1, "# X\n"))
        .await;
    assert_eq!(backend.state.read().await.open_docs.len(), before);
}

#[tokio::test]
async fn crlf_text_is_kept_and_ranged_edits_address_its_lines() {
    let (backend, _service) = shared_backend().await;
    backend
        .did_open(open_params("file:///c.md", 1, "one\r\ntwo\r\nthree\r\n"))
        .await;
    // Replace "two" (line 1, columns 0-3) and then insert at the start of line 2.
    backend
        .did_change(range_change(
            "file:///c.md",
            2,
            Range::new(Position::new(1, 0), Position::new(1, 3)),
            "2",
        ))
        .await;
    backend
        .did_change(range_change(
            "file:///c.md",
            3,
            Range::new(Position::new(2, 0), Position::new(2, 0)),
            ">",
        ))
        .await;
    // A range that spans the line break itself.
    backend
        .did_change(range_change(
            "file:///c.md",
            4,
            Range::new(Position::new(0, 3), Position::new(1, 0)),
            " ",
        ))
        .await;
    let state = backend.state.read().await;
    let open = &state.open_docs["file:///c.md"];
    assert_eq!(open.rope.to_string(), "one 2\r\n>three\r\n");
    assert_eq!(open.version, 4);
}

#[tokio::test]
async fn a_change_for_a_note_that_was_never_opened_is_ignored() {
    let (backend, _service) = shared_backend().await;
    backend
        .did_change(change_params("file:///never-opened.md", 2, "# X\n"))
        .await;
    let state = backend.state.read().await;
    assert!(!state.open_docs.contains_key("file:///never-opened.md"));
    assert!(
        state
            .index
            .get_doc(&satz_core::DocId::new("never-opened.md"))
            .is_none()
    );
}

#[tokio::test]
async fn opening_changing_and_closing_consume_the_peer_flag_once() {
    let (backend, _service) = shared_backend().await;
    backend
        .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
        .await;
    assert!(
        !backend.state.read().await.peers_dirty(),
        "consumed by did_open"
    );

    backend.state.write().await.mark_peers_dirty();
    backend
        .did_change(change_params("file:///p.md", 2, "# P\n\n[[nothing]]\n"))
        .await;
    backend.state.write().await.mark_peers_dirty();
    backend.did_close(close_params("file:///p.md")).await;
    assert!(
        !backend.state.read().await.peers_dirty(),
        "consumed by did_close"
    );
}

#[tokio::test]
async fn shutdown_stops_the_running_watcher_and_is_harmless_without_one() {
    let (backend, _service) = shared_backend().await;
    backend.shutdown().await.unwrap();
    let handle = crate::watcher::WatcherHandle::default();
    *backend.watcher.lock().unwrap() = Some(handle.clone());
    backend.shutdown().await.unwrap();
    assert!(handle.is_stopped());
    assert!(backend.watcher.lock().unwrap().is_none());
    backend.shutdown().await.unwrap();
}

// ---- which workspace folder is the vault ----

fn uri(path: &str) -> String {
    crate::convert::path_to_uri(&root().join(path))
        .unwrap()
        .as_str()
        .to_string()
}

#[test]
fn one_folder_is_the_vault_and_nothing_is_ignored() {
    let choice = pick_workspace_root(&[uri("v")], None);
    assert_eq!(choice.root, Some(root().join("v")));
    assert!(choice.ignored.is_empty());
}

#[test]
fn with_several_folders_the_first_is_the_vault_and_the_rest_are_ignored() {
    let choice = pick_workspace_root(&[uri("a"), uri("b"), uri("c")], None);
    assert_eq!(choice.root, Some(root().join("a")));
    assert_eq!(choice.ignored, vec![root().join("b"), root().join("c")]);
}

#[test]
fn the_same_folder_twice_is_not_another_folder() {
    let choice = pick_workspace_root(&[uri("a"), uri("a"), uri("b")], None);
    assert_eq!(choice.root, Some(root().join("a")));
    assert_eq!(choice.ignored, vec![root().join("b")]);
}

#[test]
fn folders_that_are_not_local_files_are_skipped() {
    let choice = pick_workspace_root(
        &[
            "untitled:x".to_string(),
            "https://example.com/w".to_string(),
            uri("real"),
        ],
        None,
    );
    assert_eq!(choice.root, Some(root().join("real")));
    assert!(choice.ignored.is_empty());
}

#[test]
fn without_folders_the_root_uri_is_used_and_without_either_there_is_no_vault() {
    let choice = pick_workspace_root(&[], Some(&uri("legacy")));
    assert_eq!(choice.root, Some(root().join("legacy")));
    assert_eq!(pick_workspace_root(&[], None), WorkspaceChoice::default());
    assert_eq!(
        pick_workspace_root(&["untitled:x".to_string()], None).root,
        None
    );
    // Folders win over the legacy root.
    let both = pick_workspace_root(&[uri("f")], Some(&uri("legacy")));
    assert_eq!(both.root, Some(root().join("f")));
}

#[tokio::test]
async fn completion_answers_from_the_live_buffer_without_waiting_for_a_reparse() {
    // The note is stale (typed since the last parse) and another reader holds the state: a
    // request that had to re-parse first would wait for that reader; completion reads the
    // live text and must not.
    let (backend, _service) = backend_with_an_unparsed_link().await;
    let _other_reader = backend.state.read().await;
    let params = CompletionParams {
        text_document_position: position_params("file:///a.md", 2, 6),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    };
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        backend.completion(params),
    )
    .await;
    assert!(answer.is_ok(), "completion waited for a re-parse");
}

// ---- the first colours are right, and the refreshes do not wait for each other ----

// The clock is virtual: it moves only when nothing else can run, so what is measured is the
// order of things, not the speed of the machine. `never` is decided by its 200 ms limit,
// `quick_one` by nothing at all, and the outer limit turns a `send_refresh` that stopped
// honouring its limit into a failure instead of a hang.
#[tokio::test(start_paused = true)]
async fn a_refresh_that_is_never_answered_times_out_and_does_not_hold_back_another() {
    use tokio::time::{Duration, Instant, timeout};
    let started = Instant::now();
    let both = async {
        let never = send_refresh(
            "a",
            std::future::pending::<Result<(), String>>(),
            Duration::from_millis(200),
        );
        let quick_one = async {
            let outcome =
                send_refresh("b", async { Ok::<(), String>(()) }, Duration::from_secs(5)).await;
            (outcome, started.elapsed())
        };
        tokio::join!(never, quick_one)
    };
    let (never_outcome, (quick_outcome, quick_took)) = timeout(Duration::from_secs(60), both)
        .await
        .expect("a refresh that is never answered still ends at its own limit");
    assert_eq!(never_outcome, RefreshOutcome::TimedOut);
    assert_eq!(quick_outcome, RefreshOutcome::Answered);
    assert_eq!(quick_took, Duration::ZERO, "the answered one waited");
    assert_eq!(started.elapsed(), Duration::from_millis(200));
}

#[tokio::test]
async fn an_error_answer_is_reported_as_failed_not_as_answered() {
    let outcome = send_refresh(
        "x",
        async { Err::<(), _>("method not found") },
        std::time::Duration::from_secs(1),
    )
    .await;
    assert_eq!(outcome, RefreshOutcome::Failed);
}

// ---- initialize waits for the first indexing, so the first request already sees every note ----

async fn fresh_backend(
    job: fn(PathBuf) -> anyhow::Result<SatzState>,
    wait: std::time::Duration,
) -> (Backend, tower_lsp_server::LspService<Backend>) {
    let client_slot = std::sync::Mutex::new(None);
    let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
    let (service, _socket) = tower_lsp_server::LspService::new(|client| {
        *client_slot.lock().unwrap() = Some(client.clone());
        Backend::new(client, handle)
    });
    let client = client_slot.lock().unwrap().take().unwrap();
    let (_layer2, handle2): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
    let mut backend = Backend::new(client, handle2);
    backend.index_job = job;
    backend.initialize_wait = wait;
    (backend, service)
}

struct VaultDir(PathBuf);
impl VaultDir {
    fn new(tag: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("satz_init_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, text) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for VaultDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn init_params(root: Option<&std::path::Path>) -> InitializeParams {
    InitializeParams {
        workspace_folders: root.map(|r| {
            vec![WorkspaceFolder {
                uri: crate::convert::path_to_uri(r).unwrap(),
                name: "vault".to_string(),
            }]
        }),
        ..Default::default()
    }
}

fn slow_job(root: PathBuf) -> anyhow::Result<SatzState> {
    std::thread::sleep(std::time::Duration::from_millis(300));
    SatzState::initialize_index(root)
}

/// The first indexing of the one test that needs it to be held: it does not start until the
/// test opens the gate (so "still indexing" is a fact, not a race against a sleep) and gives
/// up on its own after 30 s, so a broken test cannot keep the runtime from shutting down.
/// One gate: only `a_first_indexing_slower_than_the_limit_does_not_hold_initialize_up` uses it.
static FIRST_INDEXING_GATE: (std::sync::Mutex<bool>, std::sync::Condvar) =
    (std::sync::Mutex::new(false), std::sync::Condvar::new());

fn set_first_indexing_gate(open: bool) {
    *FIRST_INDEXING_GATE.0.lock().unwrap() = open;
    FIRST_INDEXING_GATE.1.notify_all();
}

/// Opens the gate when dropped, so a failed assertion does not leave the indexing held.
struct OpenGateOnDrop;
impl Drop for OpenGateOnDrop {
    fn drop(&mut self) {
        set_first_indexing_gate(true);
    }
}

fn gated_job(root: PathBuf) -> anyhow::Result<SatzState> {
    let (open, opened) = &FIRST_INDEXING_GATE;
    let _ = opened
        .wait_timeout_while(
            open.lock().unwrap(),
            std::time::Duration::from_secs(30),
            |open| !*open,
        )
        .unwrap();
    SatzState::initialize_index(root)
}

static NO_VAULT_JOB_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The job of a server that has no vault folder: it must never be called.
fn counting_job(_root: PathBuf) -> anyhow::Result<SatzState> {
    NO_VAULT_JOB_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    anyhow::bail!("there is no vault to index")
}

fn failing_job(_root: PathBuf) -> anyhow::Result<SatzState> {
    anyhow::bail!("the vault folder cannot be read")
}

#[tokio::test]
async fn the_first_document_link_after_initialize_already_has_every_link() {
    // What Helix does: `initialize`, then `didOpen`, then `documentLink` at once. With the
    // index still empty at that moment the answer used to be `[]`, and Helix only asks again
    // after the next edit: the links of the first note stayed uncoloured.
    let vault = VaultDir::new(
        "links",
        &[
            ("tlp/a.md", "# A\n\nsee [[tlp/b]] and [[c#Head]]\n"),
            ("tlp/b.md", "# B\n"),
            ("c.md", "# C\n\n## Head\n"),
        ],
    );
    let (backend, _service) = fresh_backend(slow_job, std::time::Duration::from_secs(10)).await;
    backend
        .initialize(init_params(Some(&vault.0)))
        .await
        .unwrap();
    assert!(backend.state.read().await.is_indexing_complete());
    assert_eq!(backend.state.read().await.index.doc_count(), 3);

    let uri = crate::convert::path_to_uri(&vault.0.join("tlp/a.md")).unwrap();
    backend
        .did_open(open_params(
            uri.as_str(),
            0,
            "# A\n\nsee [[tlp/b]] and [[c#Head]]\n",
        ))
        .await;
    let links = backend
        .document_link(DocumentLinkParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("links");
    let targets: Vec<String> = links
        .iter()
        .map(|l| l.target.as_ref().unwrap().as_str().to_string())
        .collect();
    assert_eq!(targets.len(), 2, "{targets:?}");
    assert!(
        targets[0].ends_with("tlp/b.md") && targets[1].ends_with("c.md"),
        "{targets:?}"
    );
}

#[tokio::test]
async fn a_first_indexing_slower_than_the_limit_does_not_hold_initialize_up() {
    use std::time::Duration;
    let vault = VaultDir::new("slow", &[("a.md", "# A\n"), ("b.md", "# B\n")]);
    set_first_indexing_gate(false);
    let _open_at_the_end = OpenGateOnDrop;
    let (backend, _service) = fresh_backend(gated_job, Duration::from_millis(50)).await;
    // The indexing cannot finish while the gate is shut, so `initialize` can only return by
    // giving up its wait; the 10 s only turns an `initialize` that waits for the indexing
    // into a failure instead of a hang.
    tokio::time::timeout(
        Duration::from_secs(10),
        backend.initialize(init_params(Some(&vault.0))),
    )
    .await
    .expect("initialize waited for an indexing that was held")
    .unwrap();
    assert!(
        !backend.state.read().await.is_indexing_complete(),
        "still indexing"
    );
    // The indexing finishes in the background and the state becomes complete.
    set_first_indexing_gate(true);
    let finished = async {
        while !backend.state.read().await.is_indexing_complete() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), finished)
        .await
        .expect("the indexing never finished in the background");
    assert_eq!(backend.state.read().await.index.doc_count(), 2);
}

#[tokio::test]
async fn the_result_of_the_first_indexing_is_announced_once_by_initialized() {
    let vault = VaultDir::new("announce", &[("a.md", "# A\n")]);
    std::fs::write(vault.0.join(".satz.toml"), "[nonsense]\nx = 1\n").unwrap();
    let (backend, _service) = fresh_backend(slow_job, std::time::Duration::from_secs(10)).await;
    backend
        .initialize(init_params(Some(&vault.0)))
        .await
        .unwrap();
    {
        let pending = backend.pending_announcement.lock().unwrap();
        let outcome = pending.as_ref().expect("kept for `initialized`");
        assert_eq!(outcome.doc_count, 1);
        assert_eq!(outcome.config_warnings.len(), 1);
    }
    backend.initialized(InitializedParams {}).await;
    assert!(
        backend.pending_announcement.lock().unwrap().is_none(),
        "announced and cleared"
    );
    backend.initialized(InitializedParams {}).await; // a second one has nothing left to say
    assert!(backend.pending_announcement.lock().unwrap().is_none());
}

#[tokio::test]
async fn an_indexing_that_fails_still_answers_initialize_and_leaves_a_working_server() {
    let vault = VaultDir::new("fail", &[("a.md", "# A\n")]);
    let (backend, _service) = fresh_backend(failing_job, std::time::Duration::from_secs(10)).await;
    backend
        .initialize(init_params(Some(&vault.0)))
        .await
        .unwrap();
    assert!(
        backend.state.read().await.is_indexing_complete(),
        "nothing left to wait for"
    );
    let pending = backend.pending_announcement.lock().unwrap();
    assert!(pending.as_ref().unwrap().failure.is_some());
}

// A virtual clock: it moves only when the runtime waits for a timer, so an `initialize` that
// slept or timed out for the 10 s it is allowed would show as 10 s here, however fast the
// machine is.
#[tokio::test(start_paused = true)]
async fn without_a_vault_folder_initialize_does_not_wait_at_all() {
    let (backend, _service) = fresh_backend(counting_job, std::time::Duration::from_secs(10)).await;
    let started = tokio::time::Instant::now();
    backend.initialize(init_params(None)).await.unwrap();
    assert_eq!(started.elapsed(), std::time::Duration::ZERO);
    assert_eq!(
        NO_VAULT_JOB_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "there is no folder to index"
    );
    assert!(backend.state.read().await.is_indexing_complete());
    assert!(backend.pending_announcement.lock().unwrap().is_none());
}

// ---- the day changes under open notes (T3): what the client is told ----

/// The index still holds yesterday's date, as it does after midnight until a request moves it.
async fn make_the_day_stale(backend: &Backend) {
    let mut state = backend.state.write().await;
    let today = chrono::Local::now().date_naive();
    let config = state.config.daily_note.clone();
    state
        .index
        .set_daily(Some((config, today.pred_opt().unwrap())));
}

/// The index is up to date with today.
async fn make_the_day_current(backend: &Backend) {
    let mut state = backend.state.write().await;
    state.sync_daily(chrono::Local::now().date_naive());
    state.clear_peers_dirty();
}

async fn hover_a(backend: &Backend) {
    let _ = backend
        .hover(HoverParams {
            text_document_position_params: position_params("file:///a.md", 0, 3),
            work_done_progress_params: Default::default(),
        })
        .await;
}

pub(crate) fn count_of(
    sent: &[(String, Option<String>)],
    method: &str,
    uri: Option<&str>,
) -> usize {
    sent.iter()
        .filter(|(m, u)| m == method && (uri.is_none() || u.as_deref() == uri))
        .count()
}

/// Two open notes, the second one linking to the daily note by its alias.
async fn day_backend() -> (
    Arc<Backend>,
    tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) {
    let (backend, mut from_server) = connected_backend().await;
    backend
        .did_open(open_params("file:///p.md", 1, "# P\n\n[[today]]\n"))
        .await;
    make_the_day_stale(&backend).await;
    // What the handshake and the opening sent is not what is asserted below.
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;
    (backend, from_server)
}

#[tokio::test]
async fn a_request_that_moves_the_day_tells_a_push_client_to_refresh_every_open_note() {
    let (backend, mut from_server) = day_backend().await;

    hover_a(&backend).await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

    for uri in ["file:///a.md", "file:///p.md"] {
        assert_eq!(
            count_of(&sent, "textDocument/publishDiagnostics", Some(uri)),
            1,
            "{uri}: {sent:?}"
        );
    }
    assert_eq!(
        count_of(&sent, "workspace/semanticTokens/refresh", None),
        1,
        "the client is asked for fresh colours: {sent:?}"
    );
}

#[tokio::test]
async fn a_request_that_moves_the_day_tells_a_pull_client_to_fetch_again() {
    let (backend, mut from_server) = day_backend().await;
    backend.state.write().await.client_supports_pull_diagnostics = true;

    hover_a(&backend).await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

    assert_eq!(
        count_of(&sent, "workspace/diagnostic/refresh", None),
        1,
        "{sent:?}"
    );
    assert_eq!(
        count_of(&sent, "workspace/semanticTokens/refresh", None),
        1,
        "{sent:?}"
    );
    assert_eq!(
        count_of(&sent, "textDocument/publishDiagnostics", None),
        0,
        "a pull client is not sent diagnostics: {sent:?}"
    );
}

#[tokio::test]
async fn a_request_on_the_same_day_tells_nobody_anything() {
    let (backend, mut from_server) = day_backend().await;
    make_the_day_current(&backend).await;

    hover_a(&backend).await;
    hover_a(&backend).await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

    assert!(
        sent.is_empty(),
        "nothing changed, nothing is sent: {sent:?}"
    );
}

#[tokio::test]
async fn two_requests_that_meet_midnight_together_tell_the_client_once() {
    let (backend, mut from_server) = day_backend().await;

    tokio::join!(hover_a(&backend), hover_a(&backend));
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

    for uri in ["file:///a.md", "file:///p.md"] {
        assert_eq!(
            count_of(&sent, "textDocument/publishDiagnostics", Some(uri)),
            1,
            "{uri}: {sent:?}"
        );
    }
    assert_eq!(
        count_of(&sent, "workspace/semanticTokens/refresh", None),
        1,
        "{sent:?}"
    );
}

#[tokio::test]
async fn what_a_refresh_at_midnight_announced_is_not_announced_again_by_the_next_reparse() {
    let (backend, mut from_server) = day_backend().await;
    {
        let mut state = backend.state.write().await;
        state.config.lsp.reparse_debounce_ms = 40;
        state.config.lsp.reparse_max_wait_ms = 40;
    }
    hover_a(&backend).await;
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
    assert!(
        !backend.state.read().await.peers_dirty(),
        "the refresh took the flag"
    );

    // An ordinary edit that changes nothing the neighbours depend on: only its own
    // diagnostics go out.
    backend
        .did_change(change_params("file:///a.md", 2, "# A\n\ntext\n"))
        .await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
    assert_eq!(
        count_of(
            &sent,
            "textDocument/publishDiagnostics",
            Some("file:///p.md")
        ),
        0,
        "{sent:?}"
    );
}

/// What a note is told about its links on the day the index holds.
fn messages_on(
    index: &mut satz_core::Index,
    config: &satz_core::VaultConfig,
    day: chrono::NaiveDate,
    note: &str,
) -> Vec<String> {
    index.set_daily(Some((config.daily_note.clone(), day)));
    let doc = index.get_doc(&satz_core::DocId::new(note)).unwrap();
    compute_diagnostics(doc, index, config)
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn the_day_the_index_holds_decides_which_daily_note_is_an_orphan() {
    // `[[today]]` reaches the daily note of the day the index holds: on yesterday's date
    // today's daily note has no backlink, on today's it has one.
    let today = chrono::Local::now().date_naive();
    let yesterday = today.pred_opt().unwrap();
    let daily = format!("daily/{}.md", today.format("%Y-%m-%d"));
    let mut index = satz_core::Index::build(vec![
        satz_core::parse_document(
            "# P

[[today]]
",
            std::path::Path::new("p.md"),
        ),
        satz_core::parse_document(
            "# Entry
",
            std::path::Path::new(&daily),
        ),
    ]);
    let config = satz_core::VaultConfig::default();

    let on_yesterday = messages_on(&mut index, &config, yesterday, &daily);
    let on_today = messages_on(&mut index, &config, today, &daily);
    assert!(
        on_yesterday.iter().any(|m| m.starts_with("Orphan note")),
        "{on_yesterday:?}"
    );
    assert!(on_today.is_empty(), "{on_today:?}");
}

/// The messages of every `publishDiagnostics` sent for `quiet` without a pause, by document.
async fn diagnostics_meanwhile(
    input: &mut (impl tokio::io::AsyncBufRead + Unpin),
    quiet: std::time::Duration,
) -> Vec<(String, Vec<String>)> {
    let mut seen = Vec::new();
    while let Ok(Some(frame)) = tokio::time::timeout(quiet, read_frame(input)).await {
        if frame["method"] == "textDocument/publishDiagnostics" {
            let uri = frame["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let messages = frame["params"]["diagnostics"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|d| d["message"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            seen.push((uri, messages));
        }
    }
    seen
}

#[tokio::test]
async fn a_note_opened_after_midnight_is_read_on_the_new_day() {
    let (backend, mut from_server) = connected_backend().await;
    let today = chrono::Local::now().date_naive();
    let daily = format!("{}.md", today.format("%Y-%m-%d"));
    {
        let mut state = backend.state.write().await;
        state.index.replace_doc(satz_core::parse_document(
            "# P

[[today]]
",
            std::path::Path::new("p.md"),
        ));
    }
    make_the_day_stale(&backend).await;
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

    // No request has come since midnight: today's daily note is opened first.
    let uri = crate::convert::path_to_uri(&root().join("daily").join(&daily))
        .unwrap()
        .as_str()
        .to_string();
    backend
        .did_open(open_params(
            &uri, 1, "# Entry
",
        ))
        .await;
    let shown =
        diagnostics_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
    let of_note: Vec<&Vec<String>> = shown
        .iter()
        .filter(|(u, _)| *u == uri)
        .map(|(_, messages)| messages)
        .collect();
    assert!(!of_note.is_empty(), "{shown:?}");
    assert!(
        shown.iter().any(|(u, _)| u == "file:///a.md"),
        "the note that was already open is told as well: {shown:?}"
    );
    assert!(
        of_note.iter().all(|messages| messages.is_empty()),
        "[[today]] links to it, so it is no orphan: {shown:?}"
    );
}

#[tokio::test]
async fn a_request_that_only_refreshes_a_stale_buffer_does_not_announce_a_day_change() {
    let (backend, mut from_server) = day_backend().await;
    make_the_day_current(&backend).await;
    {
        let mut state = backend.state.write().await;
        state.config.lsp.reparse_debounce_ms = 40;
        state.config.lsp.reparse_max_wait_ms = 40;
    }

    backend
        .did_change(change_params(
            "file:///a.md",
            2,
            "# Renamed
",
        ))
        .await;
    hover_a(&backend).await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

    // The reparse that follows the edit announces once; the same day adds nothing.
    assert_eq!(
        count_of(&sent, "workspace/semanticTokens/refresh", None),
        1,
        "{sent:?}"
    );
    assert_eq!(
        count_of(
            &sent,
            "textDocument/publishDiagnostics",
            Some("file:///a.md")
        ),
        1,
        "{sent:?}"
    );
}

#[tokio::test]
async fn a_note_that_appears_on_disk_refreshes_the_open_notes() {
    let (backend, mut from_server) = connected_backend().await;
    let vault = std::env::temp_dir().join(format!("satz_t3_watch_{}", std::process::id()));
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("new.md"),
        "# New
",
    )
    .unwrap();
    {
        // The vault is this folder, and the note that is open lies in it.
        let mut state = backend.state.write().await;
        state.set_vault_root(Some(vault.clone()));
        state.open_document(
            "file:///a.md",
            "# A
",
            &vault.join("a.md"),
            1,
        );
    }
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

    crate::watcher::process_file_event(
        &vault.join("new.md"),
        &vault,
        &backend.state,
        &backend.client,
    )
    .await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
    let _ = std::fs::remove_dir_all(&vault);

    assert_eq!(
        count_of(
            &sent,
            "textDocument/publishDiagnostics",
            Some("file:///a.md")
        ),
        1,
        "the open note is told, a new note may resolve its links: {sent:?}"
    );
}

// ---- the refresh requests need nothing from the client's announcements, nor from the state ----

#[tokio::test]
async fn the_refresh_requests_do_not_wait_for_the_state() {
    // A writer holds the state (an edit being applied, a reparse taking its turn). Asking the
    // client to fetch again is not something the state has a say in: the requests go out.
    let (backend, mut from_server) = connected_backend().await;
    backend.state.write().await.client_supports_pull_diagnostics = true;
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

    let writer = backend.state.write().await;
    let client = backend.client.clone();
    let asking = tokio::spawn(async move {
        tokio::join!(
            refresh_semantic_tokens(&client),
            refresh_diagnostics(&client)
        )
    });
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;
    drop(writer);
    asking.abort();

    assert_eq!(
        count_of(&sent, "workspace/semanticTokens/refresh", None),
        1,
        "{sent:?}"
    );
    assert_eq!(
        count_of(&sent, "workspace/diagnostic/refresh", None),
        1,
        "{sent:?}"
    );
}

/// What a client is sent after the note it has open was edited and the reparse went through:
/// `(publishDiagnostics for the note, workspace/diagnostic/refresh, workspace/semanticTokens/refresh)`.
/// The client announced nothing about refreshing (the handshake of `connected_backend`).
async fn sent_after_a_reparse(pull: bool) -> (usize, usize, usize) {
    let (backend, mut from_server) = connected_backend().await;
    backend
        .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
        .await;
    {
        let mut state = backend.state.write().await;
        state.client_supports_pull_diagnostics = pull;
        state.config.lsp.reparse_debounce_ms = 40;
        state.config.lsp.reparse_max_wait_ms = 40;
    }
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

    backend
        .did_change(change_params("file:///a.md", 2, "# Renamed\n"))
        .await;
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;
    (
        count_of(&sent, "textDocument/publishDiagnostics", None),
        count_of(&sent, "workspace/diagnostic/refresh", None),
        count_of(&sent, "workspace/semanticTokens/refresh", None),
    )
}

#[tokio::test]
async fn after_a_reparse_every_client_is_asked_for_fresh_colours_and_gets_the_diagnostics_its_way()
{
    // Whatever the client said about refreshing (here: nothing), it is asked to fetch the colours
    // again (Helix does not announce it and still answers). A pull client is asked to fetch the
    // diagnostics again; a push client is sent them: its own note's and the other open one's.
    let (published, diagnostic_refreshes, colour_refreshes) = sent_after_a_reparse(false).await;
    assert_eq!(
        (published, diagnostic_refreshes, colour_refreshes),
        (2, 0, 1),
        "push client"
    );
    let (published, diagnostic_refreshes, colour_refreshes) = sent_after_a_reparse(true).await;
    assert_eq!(
        (published, diagnostic_refreshes, colour_refreshes),
        (0, 1, 1),
        "pull client"
    );
}

/// What the client is sent when the first indexing has finished while a note was already open:
/// `(publishDiagnostics, workspace/diagnostic/refresh, workspace/semanticTokens/refresh)`. The
/// client announced nothing about refreshing.
async fn sent_after_the_first_indexing(pull: bool) -> (usize, usize, usize) {
    let (backend, mut from_server) = connected_backend().await;
    backend.state.write().await.client_supports_pull_diagnostics = pull;
    let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

    // The requests wait for an answer that never comes (up to `REFRESH_TIMEOUT`): what is
    // counted is what was sent.
    let (client, state) = (backend.client.clone(), backend.state.clone());
    let asking = tokio::spawn(async move { refresh_after_first_index(&client, &state).await });
    let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;
    asking.abort();
    (
        count_of(
            &sent,
            "textDocument/publishDiagnostics",
            Some("file:///a.md"),
        ),
        count_of(&sent, "workspace/diagnostic/refresh", None),
        count_of(&sent, "workspace/semanticTokens/refresh", None),
    )
}

#[tokio::test]
async fn after_the_first_indexing_every_client_is_asked_for_fresh_colours_and_gets_the_diagnostics_its_way()
 {
    assert_eq!(
        sent_after_the_first_indexing(false).await,
        (1, 0, 1),
        "push client"
    );
    assert_eq!(
        sent_after_the_first_indexing(true).await,
        (0, 1, 1),
        "pull client"
    );
}
