use super::*;
use satz_core::Index;
use satz_core::parse_document;
use std::collections::HashMap;
use std::path::Path;

fn state_with(docs: Vec<satz_core::Document>) -> SatzState {
    // path_to_uri (via Uri::from_file_path) requires an absolute vault root to succeed.
    let root = if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    };
    let mut state = SatzState::default();
    state.index = Index::build(docs);
    state.set_vault_root(Some(root));
    state
}

#[test]
fn test_only_dirty_documents_produce_changes() {
    let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
    let clean = parse_document("# Clean\n\nAlready tidy.\n", Path::new("clean.md"));
    let state = state_with(vec![dirty, clean]);

    let result = compute_format_changes(&state);
    assert_eq!(
        result.changes.len(),
        1,
        "only the dirty document should need an edit"
    );
    assert!(result.changes[0].uri.as_str().ends_with("dirty.md"));
    assert_eq!(
        crate::convert::apply_text_edits("Line 1   \n\n\n\nLine 2   ", &result.changes[0].edits),
        "Line 1\n\nLine 2\n"
    );
    // Both documents were freshly computed (cold cache), so both hashes get recorded: the
    // dirty one with its formatted text, the clean one as "already formatted" without a copy.
    assert_eq!(result.cache_updates.len(), 2);
    let unchanged = result
        .cache_updates
        .iter()
        .filter(|u| matches!(u, CacheUpdate::Unchanged(_)))
        .count();
    assert_eq!(unchanged, 1);
}

#[test]
fn test_no_changes_when_vault_already_clean() {
    let clean_a = parse_document("# A\n\nTidy.\n", Path::new("a.md"));
    let clean_b = parse_document("# B\n\nAlso tidy.\n", Path::new("b.md"));
    let state = state_with(vec![clean_a, clean_b]);

    let result = compute_format_changes(&state);
    assert!(result.changes.is_empty());
    assert_eq!(result.cache_updates.len(), 2);
}

#[test]
fn workspace_format_is_off_while_the_config_is_invalid_and_back_on_once_fixed() {
    let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
    let mut state = state_with(vec![dirty]);

    // Control: with a valid config the dirty document produces a change.
    assert_eq!(compute_format_changes(&state).changes.len(), 1);

    state.config_error = Some("invalid .satz.toml: line 1".to_string());
    let result = compute_format_changes(&state);
    assert!(
        result.changes.is_empty(),
        "no edits with an unusable config"
    );
    assert!(result.cache_updates.is_empty(), "and nothing cached either");

    state.config_error = None;
    assert_eq!(compute_format_changes(&state).changes.len(), 1);
}

#[test]
fn test_disabled_formatter_produces_no_changes() {
    let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
    let mut state = state_with(vec![dirty]);
    state.config.formatter.enabled = false;

    let result = compute_format_changes(&state);
    assert!(result.changes.is_empty());
    assert!(result.cache_updates.is_empty());
}

#[test]
fn test_workspace_edit_has_one_entry_per_changed_document() {
    let dirty_a = parse_document("A   \n", Path::new("a.md"));
    let dirty_b = parse_document("B   \n", Path::new("b.md"));
    let clean = parse_document("# C\n\nTidy.\n", Path::new("c.md"));
    let state = state_with(vec![dirty_a, dirty_b, clean]);

    let result = compute_format_changes(&state);
    assert_eq!(result.changes.len(), 2);

    let edit = build_workspace_edit(result.changes);
    let map = edit.changes.expect("changes map expected");
    assert_eq!(map.len(), 2);
}

#[test]
fn test_scattered_changes_produce_multiple_minimal_edits_not_one_blob() {
    let dirty = parse_document(
        "1 \t \n2\n3\n4\n5 \t \n6\n7\n8   \n",
        Path::new("scattered.md"),
    );
    let state = state_with(vec![dirty]);

    let result = compute_format_changes(&state);
    assert_eq!(result.changes.len(), 1);
    assert!(
        result.changes[0].edits.len() > 1,
        "scattered single-line changes must not collapse into one whole-document edit, got {:?}",
        result.changes[0].edits
    );
}

#[test]
fn test_cache_hit_skips_recomputation_and_is_consistent() {
    let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
    let content_hash = dirty.content_hash;
    let mut state = state_with(vec![dirty]);

    // Prime the cache as if a previous call had already computed this exact result.
    state
        .format_cache
        .insert(content_hash, "Line 1\n\nLine 2\n".to_string());

    let result = compute_format_changes(&state);
    assert_eq!(result.changes.len(), 1);
    assert_eq!(
        crate::convert::apply_text_edits("Line 1   \n\n\n\nLine 2   ", &result.changes[0].edits),
        "Line 1\n\nLine 2\n"
    );
    // Served entirely from cache: nothing new to record.
    assert!(result.cache_updates.is_empty());
}

#[test]
fn test_second_call_on_unchanged_vault_produces_no_new_cache_entries() {
    let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
    let mut state = state_with(vec![dirty]);

    let first = compute_format_changes(&state);
    assert_eq!(first.cache_updates.len(), 1);
    state.apply_format_cache_updates(first.cache_updates);

    // Second call, same state (as if the client declined to apply / vault re-scanned):
    // every document should now be a cache hit.
    let second = compute_format_changes(&state);
    assert_eq!(
        second.changes.len(),
        1,
        "still reports the same needed edit"
    );
    assert!(
        second.cache_updates.is_empty(),
        "nothing new to compute on a warm cache"
    );
}

fn backlinks_of_a(files: &[(&str, &str)], arg: serde_json::Value) -> Result<Vec<Location>, String> {
    let state = state_with(
        files
            .iter()
            .map(|(p, t)| parse_document(t, Path::new(p)))
            .collect(),
    );
    show_backlinks(&state, &[arg])
}

fn uri_for(state_root_file: &str) -> String {
    let root = if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    };
    path_to_uri(&root.join(state_root_file))
        .unwrap()
        .as_str()
        .to_string()
}

/// Unlike an ordinary vault root, a UNC one (`\\server\share\...`) really does round-trip
/// through a URI with backslashes still in it (`convert::uri_to_path` puts them back for a
/// host it finds in the URI): the one case that puts `show_backlinks`'s own `\` -> `/` to
/// work, a target note in a subfolder of it.
#[test]
fn backlinks_are_listed_for_a_note_in_a_unc_folder() {
    if !cfg!(windows) {
        return; // UNC paths are a Windows concept; `uri_to_path` only builds one there.
    }
    let unc_root = std::path::PathBuf::from("\\\\server\\share\\vault");
    let target = crate::convert::native_path("sub/deep/a.md");
    let state = state_with(vec![
        parse_document("# A\n", &target),
        parse_document("# B\n\n[[sub/deep/a]]\n", Path::new("b.md")),
    ]);
    let mut state = state;
    state.set_vault_root(Some(unc_root.clone()));
    let uri = path_to_uri(&unc_root.join(&target))
        .unwrap()
        .as_str()
        .to_string();
    let found = show_backlinks(&state, &[serde_json::json!(uri)]).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].uri.as_str().ends_with("b.md"), "{found:?}");
}

#[test]
fn both_commands_are_advertised() {
    assert!(SUPPORTED_COMMANDS.contains(&FORMAT_WORKSPACE_COMMAND));
    assert!(SUPPORTED_COMMANDS.contains(&SHOW_BACKLINKS_COMMAND));
    assert_eq!(SHOW_BACKLINKS_COMMAND, "satz.showBacklinks");
}

#[test]
fn backlinks_are_the_links_in_other_notes_that_point_at_the_note() {
    let files = [
        ("a.md", "# A\n\nself [[a]]\n"),
        ("b.md", "# B\n\nsee [[a]] and [t](a.md)\n"),
        ("c.md", "# C\n\n[[b]] only\n"),
        ("d.md", "# D\n\n[[a#Yok]] and [x](https://example.com)\n"),
    ];
    let found = backlinks_of_a(&files, serde_json::json!(uri_for("a.md"))).unwrap();
    let mut spots: Vec<(String, u32, u32, u32)> = found
        .iter()
        .map(|l| {
            (
                l.uri.as_str().rsplit('/').next().unwrap().to_string(),
                l.range.start.line,
                l.range.start.character,
                l.range.end.character,
            )
        })
        .collect();
    spots.sort();
    // b.md: `[[a]]` at 4..9 and `[t](a.md)` at 14..23; d.md: `[[a#Yok]]`; never a.md itself.
    assert_eq!(
        spots,
        vec![
            ("b.md".to_string(), 2, 4, 9),
            ("b.md".to_string(), 2, 14, 23),
            ("d.md".to_string(), 2, 0, 9),
        ]
    );
}

#[test]
fn the_answer_covers_the_same_notes_the_lens_counts() {
    let files = [
        ("a.md", "# A\n"),
        ("b.md", "[[a]] [[a]]\n"),
        ("c.md", "[[a]]\n"),
        ("d.md", "no link\n"),
    ];
    let state = state_with(
        files
            .iter()
            .map(|(p, t)| parse_document(t, Path::new(p)))
            .collect(),
    );
    let found = show_backlinks(&state, &[serde_json::json!(uri_for("a.md"))]).unwrap();
    let notes: std::collections::BTreeSet<&str> = found.iter().map(|l| l.uri.as_str()).collect();
    let id = satz_core::DocId::new("a.md");
    assert_eq!(notes.len(), state.index.incoming_from_others(&id).count());
    assert_eq!(found.len(), 3, "one location per link");
}

#[test]
fn an_unknown_note_or_a_note_without_backlinks_gives_an_empty_answer() {
    let files = [("a.md", "# A\n"), ("b.md", "# B\n")];
    assert_eq!(
        backlinks_of_a(&files, serde_json::json!(uri_for("nope.md"))).unwrap(),
        vec![]
    );
    assert_eq!(
        backlinks_of_a(&files, serde_json::json!(uri_for("a.md"))).unwrap(),
        vec![]
    );
}

#[test]
fn bad_arguments_are_rejected_with_a_reason() {
    let state = state_with(vec![parse_document("# A\n", Path::new("a.md"))]);
    for args in [
        vec![],
        vec![serde_json::json!(42)],
        vec![serde_json::json!(null)],
        vec![serde_json::json!(["a"])],
        vec![serde_json::json!("")],
        vec![serde_json::json!("not a uri at all")],
    ] {
        let err = show_backlinks(&state, &args).unwrap_err();
        assert!(!err.is_empty(), "{args:?}");
    }
}

#[test]
fn a_read_only_command_answers_with_a_json_array_of_locations() {
    let state = state_with(vec![
        parse_document("# A\n", Path::new("a.md")),
        parse_document("see [[a]]\n", Path::new("b.md")),
    ]);
    let answer = run_read_only_command(
        &state,
        SHOW_BACKLINKS_COMMAND,
        &[serde_json::json!(uri_for("a.md"))],
    )
    .expect("the command is handled")
    .expect("valid arguments");
    let list = answer.as_array().expect("an array");
    assert_eq!(list.len(), 1);
    assert!(list[0]["uri"].as_str().unwrap().ends_with("b.md"));
    assert_eq!(list[0]["range"]["start"]["line"], 0);
    assert_eq!(list[0]["range"]["start"]["character"], 4);
    assert_eq!(list[0]["range"]["end"]["character"], 9);
}

#[test]
fn bad_arguments_are_an_error_and_other_commands_are_not_handled_here() {
    let state = state_with(vec![parse_document("# A\n", Path::new("a.md"))]);
    let err = run_read_only_command(&state, SHOW_BACKLINKS_COMMAND, &[])
        .expect("handled")
        .unwrap_err();
    assert!(!err.is_empty());
    for other in [
        FORMAT_WORKSPACE_COMMAND,
        "satz.unknown",
        "",
        "SATZ.SHOWBACKLINKS",
    ] {
        assert!(
            run_read_only_command(&state, other, &[]).is_none(),
            "{other:?}"
        );
    }
}

#[test]
fn every_advertised_command_is_handled_somewhere() {
    let state = state_with(vec![parse_document("# A\n", Path::new("a.md"))]);
    for command in SUPPORTED_COMMANDS {
        let read_only = run_read_only_command(&state, command, &[]).is_some();
        assert!(
            read_only || command == FORMAT_WORKSPACE_COMMAND,
            "{command} is advertised but nothing handles it"
        );
    }
}

// ---- open documents: the live buffer is formatted, and the edit carries its version ----

fn root_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::path::PathBuf::from("C:\\")
    } else {
        std::path::PathBuf::from("/")
    }
}

/// A vault with `a.md` open at `version` whose buffer is `buffer`, while the index still holds
/// `indexed` (the debounced reparse has not run yet).
fn open_state(indexed: &str, buffer: &str, version: i32) -> (SatzState, String) {
    let mut state = SatzState::default();
    state.set_vault_root(Some(root_dir()));
    let uri = uri_for("a.md");
    state.open_document(&uri, indexed, &root_dir().join("a.md"), 1);
    let open = state.open_docs.get_mut(&uri).unwrap();
    open.rope = ropey::Rope::from_str(buffer);
    open.version = version;
    (state, uri)
}

#[test]
fn an_open_document_is_formatted_from_its_live_buffer_not_the_stale_index() {
    let (state, uri) = open_state(
        "# A\n\nclean\n",
        "# A\n\ntyped just now   \n\n\n\nmore   \n",
        7,
    );
    let result = compute_format_changes(&state);
    assert_eq!(result.changes.len(), 1);
    let change = &result.changes[0];
    assert_eq!(change.version, Some(7));
    assert_eq!(change.uri.as_str(), uri);
    // The edits turn exactly the live text into the formatted text.
    let live = "# A\n\ntyped just now   \n\n\n\nmore   \n";
    assert_eq!(
        crate::convert::apply_text_edits(live, &change.edits),
        "# A\n\ntyped just now\n\nmore\n"
    );
}

#[test]
fn an_open_document_that_is_already_formatted_produces_nothing_even_if_the_index_is_dirty() {
    let (state, _) = open_state("dirty   \n\n\n\nindex", "# A\n\nclean\n", 4);
    assert!(compute_format_changes(&state).changes.is_empty());
}

#[test]
fn a_closed_document_is_formatted_from_the_index_and_has_no_version() {
    let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
    let state = state_with(vec![dirty]);
    let result = compute_format_changes(&state);
    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].version, None);
}

#[test]
fn a_crlf_buffer_is_formatted_like_its_lf_twin_and_the_edits_fit_the_live_text() {
    let live = "# A\r\n\r\ntyped   \r\n\r\n\r\n\r\nmore\r\n";
    let (state, _) = open_state("# A\n", live, 2);
    let change = &compute_format_changes(&state).changes[0];
    assert_eq!(
        crate::convert::apply_text_edits(live, &change.edits),
        "# A\r\n\r\ntyped\r\n\r\nmore\r\n"
    );
}

#[test]
fn the_client_is_addressed_with_the_uri_it_opened_the_document_with() {
    // The client spells a Windows drive differently from how a path turns back into a URI.
    let client_uri = "file:///c%3A/notes/a.md".to_string();
    let path = if cfg!(windows) {
        std::path::PathBuf::from("C:\\notes\\a.md")
    } else {
        std::path::PathBuf::from("/notes/a.md")
    };
    let mut state = SatzState::default();
    state.set_vault_root(path.parent().map(|p| p.to_path_buf()));
    state.open_document(&client_uri, "dirty   \n\n\n\nx\n", &path, 3);
    let change = &compute_format_changes(&state).changes[0];
    assert_eq!(
        change.uri.as_str(),
        client_uri,
        "the open document's own URI"
    );
}

#[test]
fn the_versioned_edit_names_the_version_of_open_documents_only() {
    use tower_lsp_server::ls_types::{DocumentChanges, OneOf};
    let (mut state, uri) = open_state("# A\n", "# A\n\n\n\ntext   \n", 9);
    state.index.replace_doc(parse_document(
        "Line 1   \n\n\n\nLine 2   ",
        Path::new("closed.md"),
    ));
    let changes = compute_format_changes(&state).changes;
    assert_eq!(changes.len(), 2);
    let again = compute_format_changes(&state).changes;

    let edit = build_workspace_edit_versioned(changes);
    assert!(edit.changes.is_none(), "only the versioned form is used");
    let Some(DocumentChanges::Edits(edits)) = edit.document_changes else {
        panic!("expected versioned text document edits");
    };
    assert_eq!(edits.len(), 2);
    for e in &edits {
        let is_open = e.text_document.uri.as_str() == uri;
        assert_eq!(
            e.text_document.version,
            if is_open { Some(9) } else { None }
        );
        assert!(!e.edits.is_empty());
        assert!(e.edits.iter().all(|edit| matches!(edit, OneOf::Left(_))));
    }
    // The same text edits as the unversioned form, file for file.
    let plain = build_workspace_edit(again).changes.unwrap();
    for e in &edits {
        let plain_edits = &plain[&e.text_document.uri];
        let versioned: Vec<_> = e
            .edits
            .iter()
            .map(|o| match o {
                OneOf::Left(t) => t.clone(),
                OneOf::Right(a) => a.text_edit.clone(),
            })
            .collect();
        assert_eq!(&versioned, plain_edits);
    }
}

#[test]
fn the_cache_is_keyed_by_the_text_that_was_formatted() {
    let (mut state, _) = open_state("# A\n", "# A\n\n\n\ntext   \n", 2);
    let result = compute_format_changes(&state);
    state.apply_format_cache_updates(result.cache_updates);
    // The live buffer's hash is remembered (not pruned as "no document has this text").
    let again = compute_format_changes(&state);
    assert_eq!(again.changes.len(), 1);
    assert!(again.cache_updates.is_empty(), "served from the cache");
}

#[test]
fn a_buffers_cache_entry_survives_the_next_pass() {
    let (mut state, _) = open_state("# A\n", "# A\n\n\n\ntext   \n", 2);
    for pass in 0..3 {
        let result = compute_format_changes(&state);
        assert_eq!(result.changes.len(), 1, "pass {pass}");
        if pass > 0 {
            assert!(
                result.cache_updates.is_empty(),
                "pass {pass} is served from the cache"
            );
        }
        state.apply_format_cache_updates(result.cache_updates);
    }
    assert_eq!(state.format_cache.len(), 1);
}

#[test]
fn a_formatted_buffer_is_remembered_as_unchanged() {
    let (mut state, _) = open_state("dirty   \n\n\n\nindex", "# A\n\nclean\n", 2);
    let result = compute_format_changes(&state);
    assert!(result.changes.is_empty());
    state.apply_format_cache_updates(result.cache_updates);
    let hash = satz_core::content_hash("# A\n\nclean\n");
    assert!(state.format_cache.is_unchanged(hash));
}

#[test]
fn the_edit_is_meant_for_the_buffer_it_was_computed_from_not_for_formatted_text() {
    // Why the server leaves an open buffer alone after `applyEdit`: the client reports the edit
    // as a `didChange`, and the same edit applied to text that is already formatted is wrong.
    let live = "# A\n\n\n\ntext   \n\n\nmore   \n";
    let (state, _) = open_state("# A\n", live, 2);
    let change = &compute_format_changes(&state).changes[0];
    let formatted = crate::convert::apply_text_edits(live, &change.edits);
    assert_eq!(formatted, "# A\n\ntext\n\nmore\n");
    assert_ne!(
        crate::convert::apply_text_edits(&formatted, &change.edits),
        formatted
    );
}

// ---- what the workspace format sends and remembers must stay what it was ----

/// The result of the function as it was: a full formatted text in every change, and a copy of
/// every computed text for the cache.
struct RefChange {
    uri: Uri,
    formatted: String,
    edits: Vec<TextEdit>,
    version: Option<i32>,
}

struct RefResult {
    changes: Vec<RefChange>,
    cache_updates: Vec<(u64, String)>,
}

/// The function as it was before the change record was cut down to what is sent.
///
fn reference_compute(state: &SatzState) -> RefResult {
    tracing::debug!(
        doc_count = state.index.doc_count(),
        "compute_format_changes: starting"
    );
    if !state.formatting_allowed() {
        return RefResult {
            changes: Vec::new(),
            cache_updates: Vec::new(),
        };
    }

    let mut changes = Vec::new();
    let mut cache_updates = Vec::new();

    for doc in state.index.documents() {
        // An open document is formatted from its LIVE buffer: the index holds the text of the last
        // (debounced) reparse, which the user may already have typed past.
        let open = state.open_doc_for_path(&doc.path);
        let live_text: Option<String> = open.map(|(_, open_doc)| open_doc.rope.to_string());
        let live_index: Option<satz_core::LineIndex> =
            live_text.as_deref().map(satz_core::LineIndex::new);
        let (source, hash, line_index) = match (&live_text, &live_index) {
            (Some(text), Some(index)) => (text.as_str(), satz_core::content_hash(text), index),
            _ => (doc.line_index.source(), doc.content_hash, &doc.line_index),
        };
        if state.format_cache.is_unchanged(hash) {
            continue; // known to be formatted already
        }

        let formatted = match state.format_cache.get(hash) {
            Some(cached) => cached.to_string(),
            None => {
                let computed =
                    satz_core::formatter::format_document(source, &state.config.formatter);
                cache_updates.push((hash, computed.clone()));
                computed
            }
        };

        if formatted == source {
            continue;
        }

        // The client is addressed with the URI it opened the document with.
        let uri = match open.and_then(|(_, open_doc)| open_doc.uri.parse::<Uri>().ok()) {
            Some(uri) => uri,
            None => {
                let Some(uri) = state.doc_uri(doc) else {
                    continue;
                };
                uri
            }
        };

        let line_edits = line_diff(source, &formatted);
        let edits = line_edits_to_text_edits(line_index, &line_edits);

        changes.push(RefChange {
            uri,
            formatted,
            edits,
            version: open.map(|(_, open_doc)| open_doc.version),
        });
    }

    RefResult {
        changes,
        cache_updates,
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const LINES: &[&str] = &[
    "# Title",
    "## Part   ",
    "text   ",
    "text",
    "",
    "",
    "* item",
    "- item",
    "1. one",
    "1. two",
    "> quote  ",
    "```",
    "code   ",
    "| a | b |",
    "|---|---|",
    "| 1 | 22 |",
    "[[ link ]]   ",
    "$x$ and `c`   ",
    "İş ve ışık   ",
];

fn random_text(rng: &mut Rng) -> String {
    if rng.below(12) == 0 {
        return String::new();
    }
    let lines: Vec<&str> = (0..1 + rng.below(9))
        .map(|_| LINES[rng.below(LINES.len())])
        .collect();
    let eol = if rng.below(5) == 0 { "\r\n" } else { "\n" };
    let mut text = lines.join(eol);
    if rng.below(2) == 0 {
        text.push_str(eol);
    }
    text
}

/// What one pass answers, in a form that does not depend on how the answer is stored:
/// per change its URI, version, edits and the text those edits make of `source`; per cache
/// update the hash and either `None` (already formatted) or the formatted text.
type Observed = (
    Vec<(String, Option<i32>, Vec<TextEdit>, String)>,
    Vec<(u64, Option<String>)>,
);

fn observe_reference(
    result: &RefResult,
    sources: &HashMap<String, String>,
    by_hash: &HashMap<u64, String>,
) -> Observed {
    let changes = result
        .changes
        .iter()
        .map(|c| {
            let source = &sources[c.uri.as_str()];
            let made = crate::convert::apply_text_edits(source, &c.edits);
            assert_eq!(made, c.formatted, "the edits make the formatted text");
            (c.uri.as_str().to_string(), c.version, c.edits.clone(), made)
        })
        .collect();
    let updates = result
        .cache_updates
        .iter()
        .map(|(hash, text)| {
            (
                *hash,
                (by_hash.get(hash) != Some(text)).then(|| text.clone()),
            )
        })
        .collect();
    (changes, updates)
}

fn observe_now(
    result: &FormatWorkspaceResult,
    sources: &HashMap<String, String>,
    by_hash: &HashMap<u64, String>,
) -> Observed {
    let changes = result
        .changes
        .iter()
        .map(|c| {
            let source = &sources[c.uri.as_str()];
            (
                c.uri.as_str().to_string(),
                c.version,
                c.edits.clone(),
                crate::convert::apply_text_edits(source, &c.edits),
            )
        })
        .collect();
    let updates = result
        .cache_updates
        .iter()
        .map(|update| match update {
            CacheUpdate::Unchanged(hash) => (*hash, None),
            CacheUpdate::Formatted(hash, text) => (*hash, Some(text.clone())),
        })
        .collect::<Vec<_>>();
    let _ = by_hash;
    (changes, updates)
}

#[test]
fn the_workspace_format_sends_and_remembers_what_it_always_did() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut passes, mut with_changes, mut with_open_buffers) = (0, 0, 0);
    for round in 0..400 {
        let notes = 1 + rng.below(12);
        let mut docs = Vec::new();
        let mut texts: Vec<(String, String)> = Vec::new();
        for i in 0..notes {
            let path = format!("d{}/n{i}.md", i % 3);
            let text = random_text(&mut rng);
            docs.push(parse_document(&text, Path::new(&path)));
            texts.push((path, text));
        }
        let mut state = state_with(docs);
        if rng.below(12) == 0 {
            state.config.formatter.enabled = false;
        }
        // Without a vault root a note that is not open has no URI to be sent to: nothing is sent
        // for it, but what was worked out is still remembered.
        if rng.below(8) == 0 {
            state.set_vault_root(None);
        }

        // Some notes are open, with a buffer that is ahead of the index.
        let mut sources: HashMap<String, String> = HashMap::new();
        for (path, indexed) in &texts {
            let uri = uri_for(path);
            let mut source = indexed.clone();
            if rng.below(3) == 0 {
                let buffer = random_text(&mut rng);
                state.open_document(&uri, indexed, &root_dir().join(path), 1);
                let open = state.open_docs.get_mut(&uri).unwrap();
                open.rope = ropey::Rope::from_str(&buffer);
                open.version = 1 + rng.below(9) as i32;
                source = buffer;
                with_open_buffers += 1;
            }
            sources.insert(uri, source);
        }
        // The text of every hash, the way the cache pass used to look it up: what the index
        // holds, and the buffer of every open note.
        let mut by_hash: HashMap<u64, String> = state
            .index
            .documents()
            .map(|d| (d.content_hash, d.line_index.source().to_string()))
            .collect();
        for open in state.open_docs.values() {
            let text = open.rope.to_string();
            by_hash.insert(satz_core::content_hash(&text), text);
        }

        // Three passes: nothing cached, then what the first pass remembered, then again.
        for pass in 0..3 {
            let now = compute_format_changes(&state);
            let before = reference_compute(&state);
            assert_eq!(
                observe_now(&now, &sources, &by_hash),
                observe_reference(&before, &sources, &by_hash),
                "round {round}, pass {pass}"
            );
            passes += 1;
            with_changes += usize::from(!now.changes.is_empty());
            state.apply_format_cache_updates(now.cache_updates);
        }
    }
    assert!(passes >= 1200, "{passes} passes");
    assert!(with_changes > 300, "{with_changes} passes had changes");
    assert!(with_open_buffers > 500, "{with_open_buffers} open buffers");
}
