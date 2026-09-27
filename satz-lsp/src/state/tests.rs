// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

use super::*;

#[test]
fn get_rel_path_with_turkish_vault_root() {
    let root = Path::new("/notlar/İş");
    let path = Path::new("/notlar/İş/projeler/proje1.md");
    let rel = SatzState::get_rel_path(path, Some(root));
    assert_eq!(rel, PathBuf::from("projeler/proje1.md"));

    // Case-insensitive test on Windows path format (`\` separates only on Windows)
    if cfg!(windows) {
        let root_win = Path::new("C:\\Notlar\\İş");
        let path_win = Path::new("c:\\notlar\\iş\\projeler\\proje1.md");
        let rel_win = SatzState::get_rel_path(path_win, Some(root_win));
        assert_eq!(rel_win, PathBuf::from("projeler\\proje1.md"));
    }
}

// ---- `absolute_path`/`doc_path`/`doc_uri`: where a note is, whatever `doc.path` is relative to ----

#[test]
fn absolute_path_joins_a_relative_path_onto_the_root_and_leaves_everything_else_alone() {
    let root = Path::new("/vault");
    assert_eq!(
        absolute_path(Path::new("a.md"), Some(root)),
        PathBuf::from("/vault/a.md")
    );
    assert_eq!(
        absolute_path(Path::new("/elsewhere/a.md"), Some(root)),
        PathBuf::from("/elsewhere/a.md"),
        "already absolute: the root is not applied"
    );
    assert_eq!(
        absolute_path(Path::new("a.md"), None),
        PathBuf::from("a.md"),
        "no root: the path is returned as given"
    );
    assert_eq!(
        absolute_path(Path::new(""), Some(root)),
        PathBuf::from("/vault")
    );

    if cfg!(windows) {
        let root = Path::new("C:\\vault");
        assert_eq!(
            absolute_path(Path::new("sub\\a.md"), Some(root)),
            PathBuf::from("C:\\vault\\sub\\a.md")
        );
        // `C:\x` (a drive root) and `\\server\share\x` (UNC) are absolute: `join` discards
        // `root` for them on its own.
        for already_absolute in ["C:\\x.md", "\\\\server\\share\\x.md"] {
            assert_eq!(
                absolute_path(Path::new(already_absolute), Some(root)),
                PathBuf::from(already_absolute),
                "{already_absolute}"
            );
        }
        // `\x` (root-relative: "wherever the current drive is") and `C:x` (drive-relative: "the
        // working directory of that drive") are NOT absolute -- but `join` special-cases a
        // second path that already has its own root or its own prefix, and neither ends up
        // under `root` either.
        assert_eq!(
            absolute_path(Path::new("\\x.md"), Some(root)),
            PathBuf::from("C:\\x.md"),
            "root-relative: the drive of `root`, not `root` itself"
        );
        assert_eq!(
            absolute_path(Path::new("C:x.md"), Some(root)),
            PathBuf::from("C:x.md"),
            "drive-relative, and already names the drive `root` is on: unchanged"
        );
    }
}

#[test]
fn doc_path_and_doc_uri_agree_with_absolute_path() {
    let doc = satz_core::parse_document("# A\n", Path::new("sub/a.md"));
    let mut state = SatzState::default();
    state.set_vault_root(Some(if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }));
    assert_eq!(
        state.doc_path(&doc),
        absolute_path(&doc.path, state.vault_root())
    );
    assert!(
        state.doc_uri(&doc).unwrap().as_str().ends_with("/sub/a.md"),
        "{:?}",
        state.doc_uri(&doc)
    );

    // No vault root: `doc.path` is relative, so there is nowhere to open it from.
    let mut rootless = SatzState::default();
    rootless.set_vault_root(None);
    assert_eq!(rootless.doc_path(&doc), doc.path);
    assert!(rootless.doc_uri(&doc).is_none());
}

// ---- `documents_linking_to`/`links_to`: one place for "who links to this note" ----

fn linking_state() -> SatzState {
    let mut state = SatzState::default();
    state.index = Index::build(vec![
        satz_core::parse_document("# A\n\n[[a]] self-link, [[b]]\n", Path::new("a.md")),
        satz_core::parse_document("# B\n\n[[a]] and [[a#Nope]]\n", Path::new("b.md")),
        satz_core::parse_document("# C\n\nno links here\n", Path::new("c.md")),
    ]);
    state
}

#[test]
fn documents_linking_to_include_has_the_target_exactly_once_even_with_a_self_link() {
    let state = linking_state();
    let a = satz_core::DocId::new("a.md");
    let mut ids: Vec<&str> = state
        .documents_linking_to(&a, SelfLinks::Include)
        .map(|d| d.id.as_str())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["a.md", "b.md"],
        "a links to itself once, b links to it once"
    );
}

#[test]
fn documents_linking_to_exclude_drops_the_self_link() {
    let state = linking_state();
    let a = satz_core::DocId::new("a.md");
    let ids: Vec<&str> = state
        .documents_linking_to(&a, SelfLinks::Exclude)
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(ids, vec!["b.md"]);

    let c = satz_core::DocId::new("c.md");
    assert_eq!(
        state.documents_linking_to(&c, SelfLinks::Exclude).count(),
        0
    );
}

#[test]
fn links_to_finds_only_the_links_that_resolve_to_the_target() {
    let state = linking_state();
    let a_doc = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
    let a = satz_core::DocId::new("a.md");
    // `[[a]]` resolves to a, `[[b]]` does not.
    assert_eq!(state.links_to(a_doc, &a).count(), 1);
    let b = satz_core::DocId::new("b.md");
    assert_eq!(state.links_to(a_doc, &b).count(), 1);

    let b_doc = state.index.get_doc(&satz_core::DocId::new("b.md")).unwrap();
    // `[[a]]` and `[[a#Nope]]` both resolve to a (a missing heading still names the note).
    assert_eq!(state.links_to(b_doc, &a).count(), 2);
}

#[test]
fn test_identity_keys_change_detected() {
    let doc1 = satz_core::parse_document(
        "---\ntitle: Eski Başlık\naliases: [alias1]\n---\n# Content",
        Path::new("doc.md"),
    );
    let doc2 = satz_core::parse_document(
        "---\ntitle: Yeni Başlık\naliases: [alias1]\n---\n# Content",
        Path::new("doc.md"),
    );

    let keys1 = doc1.identity_keys();
    let keys2 = doc2.identity_keys();

    assert_ne!(keys1, keys2);
    assert!(keys1.contains("eski baslik") || keys1.contains("eski başlık"));
    assert!(keys2.contains("yeni baslik") || keys2.contains("yeni başlık"));
}

fn state_with_broken_link_to_new() -> SatzState {
    let a = satz_core::parse_document("# A\n\nSee [[new]].", Path::new("a.md"));
    SatzState {
        index: Index::build(vec![a]),
        indexing_complete: true,
        ..Default::default()
    }
}

#[test]
fn open_new_note_marks_peers_dirty() {
    // A note that isn't in the index yet (e.g. just created via "Create note") can resolve
    // links other open documents show as broken, so their diagnostics must be refreshed.
    let mut state = state_with_broken_link_to_new();
    assert!(!state.peers_dirty);
    state.open_document("file:///new.md", "# New", Path::new("new.md"), 1);
    assert!(state.peers_dirty);
}

#[test]
fn reopening_an_unchanged_indexed_note_does_not_mark_peers_dirty() {
    let mut state = state_with_broken_link_to_new();
    state.open_document("file:///a.md", "# A\n\nSee [[new]].", Path::new("a.md"), 1);
    assert!(!state.peers_dirty);
}

#[test]
fn create_note_flow_clears_broken_link_and_orphan_diagnostics() {
    use crate::handlers::diagnostics::compute_diagnostics;
    let mut state = state_with_broken_link_to_new();

    let a_before = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
    let codes = |diags: &[tower_lsp_server::ls_types::Diagnostic]| -> Vec<String> {
        diags
            .iter()
            .filter_map(|d| match &d.code {
                Some(tower_lsp_server::ls_types::NumberOrString::String(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    };
    assert!(
        codes(&compute_diagnostics(a_before, &state.index, &state.config))
            .contains(&"broken-link".to_string())
    );

    state.open_document("file:///new.md", "# New\n\nBody.", Path::new("new.md"), 1);

    let new_doc = state
        .index
        .get_doc(&satz_core::DocId::new("new.md"))
        .unwrap();
    let new_codes = codes(&compute_diagnostics(new_doc, &state.index, &state.config));
    assert!(
        !new_codes.contains(&"orphan-note".to_string()),
        "freshly created note wrongly flagged orphan: {new_codes:?}"
    );
    let a_after = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
    let a_codes = codes(&compute_diagnostics(a_after, &state.index, &state.config));
    assert!(
        !a_codes.contains(&"broken-link".to_string()),
        "link to the new note still reported broken: {a_codes:?}"
    );
}

#[test]
fn the_debounce_delay_is_the_debounce_until_max_wait_cuts_it_short() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let delay =
        |debounce, max_wait, elapsed| debounce_delay(ms(debounce), ms(max_wait), ms(elapsed));

    // Defaults (200 / 500): the debounce while there is plenty of max-wait left...
    assert_eq!(delay(200, 500, 0), ms(200), "first change of a series");
    assert_eq!(
        delay(200, 500, 100),
        ms(200),
        "typing on, max-wait far away"
    );
    assert_eq!(
        delay(200, 500, 300),
        ms(200),
        "exactly as much max-wait left as debounce"
    );
    // ...the rest of the max-wait once that is shorter...
    assert_eq!(delay(200, 500, 400), ms(100), "max-wait caps the delay");
    assert_eq!(delay(200, 500, 499), ms(1), "one millisecond left");
    // ...and nothing once it is used up (no underflow, no panic).
    assert_eq!(delay(200, 500, 500), ms(0), "max-wait exactly used up");
    assert_eq!(delay(200, 500, 550), ms(0), "max-wait exceeded");
    assert_eq!(
        debounce_delay(ms(200), ms(500), Duration::MAX),
        ms(0),
        "an absurd elapsed time"
    );

    // A max-wait shorter than the debounce wins from the start.
    assert_eq!(delay(300, 100, 0), ms(100), "max-wait below debounce");
    // Zero settings mean "reparse at once", never a wait.
    assert_eq!(delay(0, 500, 0), ms(0), "no debounce");
    assert_eq!(delay(200, 0, 0), ms(0), "no max-wait");
    assert_eq!(delay(0, 0, 0), ms(0), "neither");
}

#[test]
fn the_debounce_delay_never_grows_while_typing_continues() {
    use std::time::Duration;
    let mut previous = Duration::MAX;
    for elapsed in 0..700 {
        let delay = debounce_delay(
            Duration::from_millis(200),
            Duration::from_millis(500),
            Duration::from_millis(elapsed),
        );
        assert!(
            delay <= previous,
            "at {elapsed} ms: {delay:?} after {previous:?}"
        );
        assert!(
            Duration::from_millis(elapsed) + delay
                <= Duration::from_millis(500).max(Duration::from_millis(elapsed)),
            "at {elapsed} ms the reparse would land past the max-wait: {delay:?}"
        );
        previous = delay;
    }
}

/// A unique, self-cleaning vault directory containing one note.
struct TempVault(PathBuf);
impl TempVault {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "satz_lsp_{tag}_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "# A\n\nText.\n").unwrap();
        Self(dir)
    }
    fn config(&self, content: &str) {
        std::fs::write(self.0.join(".satz.toml"), content).unwrap();
    }
}
impl Drop for TempVault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn initialize_index_reports_an_invalid_config_but_still_indexes() {
    let v = TempVault::new("badcfg");
    v.config("[formatter\nline_width = 1\n");

    let state = SatzState::initialize_index(v.0.clone()).unwrap();

    let error = state
        .config_error
        .as_deref()
        .expect("error must be recorded");
    assert!(error.contains(".satz.toml"), "{error}");
    assert!(error.contains("line 1"), "{error}");
    assert_eq!(state.config, VaultConfig::default());
    assert_eq!(state.index.doc_count(), 1, "notes are still indexed");
    assert!(!state.formatting_allowed());
}

#[test]
fn initialize_index_reports_wrong_types_as_an_error() {
    let v = TempVault::new("badkey");
    v.config("[hover]\npreview_lines = \"many\"\n");
    let state = SatzState::initialize_index(v.0.clone()).unwrap();
    let error = state
        .config_error
        .as_deref()
        .expect("a config error must be recorded");
    assert!(error.contains("preview_lines"), "{error}");
    assert!(!state.formatting_allowed());
}

#[test]
fn initialize_index_applies_a_valid_config_without_error() {
    let v = TempVault::new("okcfg");
    v.config("[hover]\npreview_lines = 3\n");

    let state = SatzState::initialize_index(v.0.clone()).unwrap();

    assert_eq!(state.config_error, None);
    assert_eq!(state.config.hover.preview_lines, 3);
    assert!(state.formatting_allowed());
}

#[test]
fn initialize_index_reads_the_vault_with_the_gitignore_setting_of_its_config() {
    // A folder that is no git repository, with a `.gitignore` naming one note.
    let v = TempVault::new("gitignore_mode");
    std::fs::write(
        v.0.join("secret.md"),
        "# Secret
",
    )
    .unwrap();
    std::fs::write(
        v.0.join(".gitignore"),
        "secret.md
",
    )
    .unwrap();
    let notes = |state: &SatzState| {
        let mut ids: Vec<String> = state
            .index
            .documents()
            .map(|d| d.id.as_str().to_string())
            .collect();
        ids.sort();
        ids
    };

    // The default reads every note (there is no repository).
    let state = SatzState::initialize_index(v.0.clone()).unwrap();
    assert_eq!(notes(&state), vec!["a.md", "secret.md"]);

    v.config(
        "[vault]
gitignore = \"always\"
",
    );
    let state = SatzState::initialize_index(v.0.clone()).unwrap();
    assert_eq!(state.config_error, None);
    assert_eq!(notes(&state), vec!["a.md"]);
}

#[test]
fn initialize_index_without_a_config_file_uses_defaults_without_error() {
    let v = TempVault::new("nocfg");

    let state = SatzState::initialize_index(v.0.clone()).unwrap();

    assert_eq!(state.config_error, None);
    assert_eq!(state.config, VaultConfig::default());
    assert!(state.formatting_allowed());
}

#[test]
fn formatting_allowed_truth_table() {
    for (enabled, error, allowed) in [
        (true, None, true),
        (false, None, false),
        (true, Some("broken"), false),
        (false, Some("broken"), false),
    ] {
        let mut state = SatzState::default();
        state.config.formatter.enabled = enabled;
        state.config_error = error.map(str::to_string);
        assert_eq!(
            state.formatting_allowed(),
            allowed,
            "enabled={enabled} error={error:?}"
        );
    }
}

#[test]
fn config_error_message_names_the_problem_the_fallback_and_the_consequence() {
    let msg = config_error_message("invalid /v/.satz.toml: line 3", "default");
    assert!(msg.contains("invalid /v/.satz.toml: line 3"), "{msg}");
    assert!(msg.contains("default settings"), "{msg}");
    assert!(msg.to_lowercase().contains("formatting"), "{msg}");
    let msg = config_error_message("x", "the previous");
    assert!(msg.contains("the previous settings"), "{msg}");
}

// ---- peers_dirty follows everything another open document's diagnostics depend on ----

/// An open `a.md` (`before`), settled, then re-parsed as `after`; returns `peers_dirty`.
fn dirty_after_edit(before: &str, after: &str) -> bool {
    let mut state = SatzState::default();
    state.open_document("file:///a.md", before, Path::new("a.md"), 1);
    state.peers_dirty = false;
    state.open_document("file:///a.md", after, Path::new("a.md"), 2);
    state.peers_dirty
}

#[test]
fn a_new_or_removed_link_marks_peers_dirty() {
    // The target's orphan status depends on who links to it.
    assert!(dirty_after_edit("# A\n", "# A\n\nSee [[b]].\n"));
    assert!(dirty_after_edit("# A\n\nSee [[b]].\n", "# A\n"));
    assert!(dirty_after_edit(
        "# A\n\nSee [[b]].\n",
        "# A\n\nSee [[c]].\n"
    ));
    assert!(dirty_after_edit("# A\n", "# A\n\n![[img]]\n"));
    assert!(dirty_after_edit("# A\n", "# A\n\n[t](b.md)\n"));
}

#[test]
fn headings_and_block_anchors_mark_peers_dirty() {
    // Anchor diagnostics elsewhere (`[[a#Section]]`) depend on them.
    assert!(dirty_after_edit("# A\n", "# A\n\n## Section\n"));
    assert!(dirty_after_edit("# A\n\n## One\n", "# A\n\n## Two\n"));
    assert!(dirty_after_edit("# A\n\ntext\n", "# A\n\ntext ^blk\n"));
}

#[test]
fn ordinary_edits_do_not_mark_peers_dirty() {
    assert!(!dirty_after_edit(
        "# A\n\nsome text\n",
        "# A\n\nsome more text\n"
    ));
    assert!(!dirty_after_edit(
        "# A\n\nSee [[b]].\n",
        "# A\n\nSee [[b]] and words.\n"
    ));
    // Display text and link kind spelling are irrelevant to peers.
    assert!(!dirty_after_edit(
        "# A\n\nSee [[b]].\n",
        "# A\n\nSee [[b|shown]].\n"
    ));
    // External links never involve another note.
    assert!(!dirty_after_edit(
        "# A\n",
        "# A\n\n[m](mailto:a@b.c) [w](https://a.b)\n"
    ));
    // Same content parsed again.
    assert!(!dirty_after_edit(
        "# A\n\nSee [[b]].\n",
        "# A\n\nSee [[b]].\n"
    ));
}

#[test]
fn a_link_added_to_one_open_note_removes_the_orphan_hint_of_another() {
    use crate::handlers::diagnostics::compute_diagnostics;
    let mut state = SatzState::default();
    state.open_document("file:///a.md", "# A\n", Path::new("a.md"), 1);
    state.open_document("file:///b.md", "# B\n", Path::new("b.md"), 1);
    let orphan = |state: &SatzState| {
        let b = state.index.get_doc(&satz_core::DocId::new("b.md")).unwrap();
        compute_diagnostics(b, &state.index, &state.config)
            .iter()
            .any(|d| {
                d.code
                    == Some(tower_lsp_server::ls_types::NumberOrString::String(
                        "orphan-note".into(),
                    ))
            })
    };
    assert!(orphan(&state));
    state.peers_dirty = false;
    state.open_document("file:///a.md", "# A\n\nSee [[b]].\n", Path::new("a.md"), 2);
    assert!(state.peers_dirty, "the peers must be told to refresh");
    assert!(!orphan(&state));
}

// ---- closing a document returns the index to what is on disk ----

/// A fresh, empty directory under the system temp dir (removed by the caller).
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "satz-test-{}-{}-{}",
        std::process::id(),
        tag,
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn state_for(dir: &Path) -> SatzState {
    let mut state = SatzState::default();
    state.vault_root = Some(dir.to_path_buf());
    state.indexing_complete = true;
    state
}

fn link_targets(state: &SatzState, id: &str) -> Vec<String> {
    state
        .index
        .get_doc(&satz_core::DocId::new(id))
        .map(|d| d.links.iter().map(|l| l.target_doc.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn closing_an_unsaved_buffer_puts_the_disk_version_back_in_the_index() {
    let dir = temp_dir("close-dirty");
    let path = dir.join("a.md");
    std::fs::write(&path, "# A\n\ndisk [[x]]\n").unwrap();
    let mut state = state_for(&dir);
    state.open_document("file:///a.md", "# A\n\nunsaved [[y]]\n", &path, 1);
    assert_eq!(link_targets(&state, "a.md"), vec!["y"]);

    state.close_document("file:///a.md");

    assert!(state.open_docs.is_empty());
    assert_eq!(link_targets(&state, "a.md"), vec!["x"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_a_buffer_whose_file_does_not_exist_removes_it_from_the_index() {
    let dir = temp_dir("close-missing");
    let path = dir.join("never-saved.md");
    let mut state = state_for(&dir);
    state.open_document("file:///n.md", "# N\n", &path, 1);
    assert!(
        state
            .index
            .get_doc(&satz_core::DocId::new("never-saved.md"))
            .is_some()
    );

    state.close_document("file:///n.md");

    assert!(
        state
            .index
            .get_doc(&satz_core::DocId::new("never-saved.md"))
            .is_none()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_marks_peers_dirty_only_when_what_they_see_changed() {
    let dir = temp_dir("close-peers");
    let path = dir.join("a.md");
    std::fs::write(&path, "# A\n\nSee [[b]].\n").unwrap();

    // Buffer identical to disk: nothing changes for anyone.
    let mut state = state_for(&dir);
    state.open_document("file:///a.md", "# A\n\nSee [[b]].\n", &path, 1);
    state.peers_dirty = false;
    state.close_document("file:///a.md");
    assert!(!state.peers_dirty);

    // Unsaved edit changed a link and a heading: closing reverts them, peers must refresh.
    let mut state = state_for(&dir);
    state.open_document("file:///a.md", "# A\n\n## New\n\nSee [[c]].\n", &path, 1);
    state.peers_dirty = false;
    state.close_document("file:///a.md");
    assert!(state.peers_dirty);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_twice_or_closing_an_unknown_uri_is_harmless() {
    let dir = temp_dir("close-twice");
    let path = dir.join("a.md");
    std::fs::write(&path, "# A\n").unwrap();
    let mut state = state_for(&dir);
    state.open_document("file:///a.md", "# A\n", &path, 1);
    state.close_document("file:///a.md");
    state.close_document("file:///a.md");
    state.close_document("file:///never-opened.md");
    assert!(
        state
            .index
            .get_doc(&satz_core::DocId::new("a.md"))
            .is_some()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_cannot_be_read_leaves_the_index_alone_on_close() {
    let dir = temp_dir("close-unreadable");
    // A directory where the file should be: exists, but read_to_string fails.
    let path = dir.join("a.md");
    std::fs::create_dir_all(&path).unwrap();
    let mut state = state_for(&dir);
    state.open_document("file:///a.md", "# A\n\n[[kept]]\n", &path, 1);
    state.close_document("file:///a.md");
    assert_eq!(link_targets(&state, "a.md"), vec!["kept"]);
    let _ = std::fs::remove_dir_all(&dir);
}

fn state_with_open(root: &str, open: &str, rel_files: &[&str]) -> SatzState {
    let mut state = SatzState::default();
    state.index = Index::build(
        rel_files
            .iter()
            .map(|p| satz_core::parse_document("# t\n", Path::new(p)))
            .collect(),
    );
    state.vault_root = Some(PathBuf::from(root));
    state.open_docs.insert(
        "file:///x".to_string(),
        OpenDocument::new("file:///x", PathBuf::from(open), "# t\n", 1),
    );
    state
}

#[test]
fn doc_for_uri_finds_an_open_and_indexed_document() {
    let state = state_with_open("/vault", "/vault/sub/a.md", &["sub/a.md", "b.md"]);
    let (open, doc) = state.doc_for_uri("file:///x").expect("open and indexed");
    assert_eq!(open.path, PathBuf::from("/vault/sub/a.md"));
    assert_eq!(doc.id.as_str(), "sub/a.md");
}

#[test]
fn doc_for_uri_is_none_for_unknown_or_unindexed_documents() {
    let state = state_with_open("/vault", "/vault/sub/a.md", &["b.md"]);
    assert!(
        state.doc_for_uri("file:///x").is_none(),
        "open but not indexed"
    );
    assert!(state.doc_for_uri("file:///other").is_none(), "not open");
    assert!(state.doc_for_uri("").is_none());
}

#[test]
fn doc_for_uri_follows_the_same_path_rules_as_get_rel_path() {
    // Windows separators and a differently cased root still land on the indexed note.
    let state = state_with_open("C:\\Notlar\\İş", "c:\\notlar\\iş\\projeler\\p1.md", &[]);
    assert!(
        state.doc_for_uri("file:///x").is_none(),
        "nothing indexed yet"
    );
    // A path outside the root is looked up as given (and is not indexed under that name).
    let state = state_with_open("/vault", "/elsewhere/a.md", &["a.md"]);
    assert!(state.doc_for_uri("file:///x").is_none());
    // No vault root: the path itself is the id.
    let mut state = SatzState::default();
    state.index = Index::build(vec![satz_core::parse_document("# t\n", Path::new("a.md"))]);
    state.open_docs.insert(
        "file:///y".to_string(),
        OpenDocument::new("file:///y", PathBuf::from("a.md"), "# t\n", 1),
    );
    assert_eq!(
        state.doc_for_uri("file:///y").unwrap().1.id.as_str(),
        "a.md"
    );
}

// ---- notes in a folder: the path has the separator of the platform, the id has `/` ----

/// The root of a vault (absolute on every platform).
fn folder_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }
}

#[test]
fn a_note_in_a_folder_is_found_by_its_uri_whatever_the_separator_of_its_path() {
    let rel = crate::convert::native_path("sub/deep/a.md");
    let mut state = SatzState::default();
    state.index = Index::build(vec![satz_core::parse_document(
        "# A
", &rel,
    )]);
    state.vault_root = Some(folder_root());
    state.open_docs.insert(
        "file:///x".to_string(),
        OpenDocument::new(
            "file:///x",
            folder_root().join(&rel),
            "# A
",
            1,
        ),
    );
    let (_, doc) = state.doc_for_uri("file:///x").expect("open and indexed");
    assert_eq!(doc.id.as_str(), "sub/deep/a.md");
}

#[test]
fn opening_a_note_in_a_folder_that_the_index_already_holds_changes_nothing_for_its_peers() {
    let rel = crate::convert::native_path("sub/deep/a.md");
    let text = "# A

[[b]]
";
    let mut state = SatzState::default();
    state.vault_root = Some(folder_root());
    state.index = Index::build(vec![satz_core::parse_document(text, &rel)]);
    state.open_document("file:///x", text, &folder_root().join(&rel), 1);
    assert!(
        !state.peers_dirty(),
        "the note was found under its id: what it links to and offers has not changed"
    );
    // A note the index does not hold yet is a change, in the same folder or not.
    let other = crate::convert::native_path("sub/deep/new.md");
    state.open_document(
        "file:///y",
        "# New
",
        &folder_root().join(other),
        1,
    );
    assert!(state.peers_dirty());
}

#[test]
fn closing_a_note_in_a_folder_that_is_as_the_index_has_it_changes_nothing_for_its_peers() {
    let dir = scratch_dir("closefolder");
    let rel = crate::convert::native_path("sub/deep/a.md");
    let text = "# A

[[b]]
";
    std::fs::create_dir_all(dir.join("sub").join("deep")).unwrap();
    std::fs::write(dir.join(&rel), text).unwrap();
    let mut state = SatzState::default();
    state.vault_root = Some(dir.clone());
    state.index = Index::build(vec![satz_core::parse_document(text, &rel)]);
    state.open_document("file:///x", text, &dir.join(&rel), 1);
    assert!(!state.peers_dirty());
    state.close_document("file:///x");
    assert!(
        !state.peers_dirty(),
        "the file is what the index held: nothing changed for the peers"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the workspace-format cache: no stale entries, no copies of already formatted text ----

fn state_with_docs(texts: &[(&str, &str)], capacity: usize) -> SatzState {
    let mut state = SatzState::default();
    state.index = Index::build(
        texts
            .iter()
            .map(|(p, t)| satz_core::parse_document(t, Path::new(p)))
            .collect(),
    );
    state.format_cache = FormatCache::new(capacity);
    // Formatting results need file locations, so the vault root must be absolute.
    state.vault_root = Some(if cfg!(windows) {
        PathBuf::from("C:\\")
    } else {
        PathBuf::from("/")
    });
    state
}

fn hash_of(state: &SatzState, path: &str) -> u64 {
    state
        .index
        .documents()
        .find(|d| d.path == Path::new(path))
        .unwrap()
        .content_hash
}

/// One workspace-format pass: compute, then record what was computed the way the server does.
fn run_pass(state: &mut SatzState) -> usize {
    let result = crate::handlers::execute_command::compute_format_changes(state);
    let changes = result.changes.len();
    state.apply_format_cache_updates(result.cache_updates);
    changes
}

const DIRTY_A: &str = "Line 1   \n\n\n\nLine 2   ";
const DIRTY_B: &str = "Other   \n\n\n\nText   ";
const CLEAN: &str = "# Clean\n\nAlready tidy.\n";

#[test]
fn entries_of_documents_that_no_longer_exist_do_not_keep_current_ones_out() {
    let mut state = state_with_docs(&[("a.md", DIRTY_A), ("b.md", DIRTY_B)], 2);
    // The cache is full of hashes no document has any more (edited or deleted notes).
    state.format_cache.insert(1001, "old one".to_string());
    state.format_cache.insert(1002, "old two".to_string());

    assert_eq!(run_pass(&mut state), 2);
    assert!(state.format_cache.get(1001).is_none());
    assert!(state.format_cache.get(1002).is_none());
    for path in ["a.md", "b.md"] {
        assert!(
            state.format_cache.get(hash_of(&state, path)).is_some(),
            "{path} should be cached"
        );
    }
    assert_eq!(state.format_cache.len(), 2);
}

#[test]
fn an_already_formatted_document_is_remembered_without_a_copy() {
    let mut state = state_with_docs(&[("clean.md", CLEAN)], 10);
    assert_eq!(run_pass(&mut state), 0);
    let hash = hash_of(&state, "clean.md");
    assert!(state.format_cache.is_unchanged(hash));
    assert!(
        state.format_cache.get(hash).is_none(),
        "no formatted text is stored for an unchanged document"
    );
    // The next pass is answered entirely from the cache.
    let again = crate::handlers::execute_command::compute_format_changes(&state);
    assert!(again.changes.is_empty());
    assert!(again.cache_updates.is_empty(), "nothing was recomputed");
}

#[test]
fn changed_and_unchanged_documents_are_both_served_from_the_cache() {
    let mut state = state_with_docs(&[("a.md", DIRTY_A), ("clean.md", CLEAN)], 10);
    assert_eq!(run_pass(&mut state), 1);
    let again = crate::handlers::execute_command::compute_format_changes(&state);
    assert_eq!(
        again.changes.len(),
        1,
        "the dirty note still needs its edit"
    );
    assert_eq!(
        crate::convert::apply_text_edits(DIRTY_A, &again.changes[0].edits),
        "Line 1\n\nLine 2\n"
    );
    assert!(again.cache_updates.is_empty());
    assert_eq!(state.format_cache.len(), 2);
}

#[test]
fn an_edited_document_replaces_its_old_entry() {
    let mut state = state_with_docs(&[("a.md", DIRTY_A)], 10);
    run_pass(&mut state);
    let old_hash = hash_of(&state, "a.md");
    state
        .index
        .replace_doc(satz_core::parse_document(DIRTY_B, Path::new("a.md")));
    run_pass(&mut state);
    assert!(state.format_cache.get(old_hash).is_none());
    assert!(state.format_cache.get(hash_of(&state, "a.md")).is_some());
    assert_eq!(state.format_cache.len(), 1);
}

#[test]
fn the_capacity_is_never_exceeded_and_zero_capacity_is_harmless() {
    let mut state = state_with_docs(
        &[
            ("a.md", DIRTY_A),
            ("b.md", DIRTY_B),
            ("c.md", "x   \n\n\n\ny"),
        ],
        2,
    );
    assert_eq!(run_pass(&mut state), 3);
    assert_eq!(state.format_cache.len(), 2);

    let mut state = state_with_docs(&[("a.md", DIRTY_A), ("clean.md", CLEAN)], 0);
    assert_eq!(run_pass(&mut state), 1);
    assert_eq!(state.format_cache.len(), 0);
    assert!(state.format_cache.is_empty());
}

#[test]
fn identical_content_in_two_files_is_one_entry() {
    let mut state = state_with_docs(&[("a.md", DIRTY_A), ("copy.md", DIRTY_A)], 10);
    assert_eq!(run_pass(&mut state), 2);
    assert_eq!(state.format_cache.len(), 1);
}

#[test]
fn retaining_hashes_keeps_only_the_live_ones_of_both_kinds() {
    let mut cache = FormatCache::new(10);
    cache.insert(1, "one".to_string());
    cache.insert_unchanged(2);
    cache.insert(3, "three".to_string());
    cache.insert_unchanged(4);
    cache.retain_hashes(&std::collections::HashSet::from([2, 3]));
    assert_eq!(cache.len(), 2);
    assert!(cache.get(1).is_none() && !cache.is_unchanged(4));
    assert!(cache.is_unchanged(2));
    assert_eq!(cache.get(3), Some("three"));
}

// ---- the initial indexing: a failure must not leave the server dead ----

fn scratch_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "satz-index-{}-{tag}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn doc_ids(state: &SatzState) -> Vec<String> {
    let mut ids: Vec<String> = state
        .index
        .documents()
        .map(|d| d.id.as_str().to_string())
        .collect();
    ids.sort();
    ids
}

/// A state that already has a document open, as if the client opened it during indexing.
fn state_with_open_buffer(dir: &Path) -> SatzState {
    let mut state = SatzState::default();
    state.client_supports_pull_diagnostics = true;
    state.open_document(
        "file:///b.md",
        "# B\n\n[[from-the-buffer]]\n",
        &dir.join("b.md"),
        3,
    );
    state
}

#[test]
fn a_finished_index_keeps_the_open_buffers_and_the_client_settings() {
    let dir = scratch_dir("ok");
    std::fs::write(dir.join("a.md"), "# A\n").unwrap();
    std::fs::write(dir.join("b.md"), "# B on disk\n").unwrap();
    let mut state = state_with_open_buffer(&dir);

    let outcome = state.finish_indexing(SatzState::initialize_index(dir.clone()), &dir);

    assert_eq!(outcome.failure, None);
    assert_eq!(outcome.doc_count, 2);
    assert!(state.indexing_complete);
    assert!(state.client_supports_pull_diagnostics);
    assert_eq!(doc_ids(&state), vec!["a.md", "b.md"]);
    // The buffer, not the file, is what is indexed for the open note.
    let b = state.index.get_doc(&satz_core::DocId::new("b.md")).unwrap();
    assert_eq!(b.links[0].target_doc, "from-the-buffer");
    assert_eq!(state.open_docs["file:///b.md"].version, 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_index_does_not_leave_the_server_waiting_forever() {
    let dir = scratch_dir("fail");
    let missing = dir.join("no-such-vault");
    let mut state = state_with_open_buffer(&missing);
    assert!(!state.indexing_complete);

    let result = SatzState::initialize_index(missing.clone());
    let outcome = state.finish_indexing(result, &missing);

    let failure = outcome.failure.expect("the failure is reported");
    assert!(failure.contains("no-such-vault"), "{failure}");
    assert!(state.indexing_complete, "handlers must not stay silent");
    assert_eq!(state.vault_root.as_deref(), Some(missing.as_path()));
    assert!(state.client_supports_pull_diagnostics);
    // What the user has open still works.
    assert_eq!(doc_ids(&state), vec!["b.md"]);
    assert_eq!(state.open_docs.len(), 1);
    assert_eq!(outcome.doc_count, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn indexing_can_be_tried_again_after_a_failure() {
    let dir = scratch_dir("retry");
    let missing = dir.join("later");
    let mut state = state_with_open_buffer(&missing);
    state.finish_indexing(SatzState::initialize_index(missing.clone()), &missing);
    assert!(state.indexing_complete);

    std::fs::create_dir_all(&missing).unwrap();
    std::fs::write(missing.join("c.md"), "# C\n").unwrap();
    let outcome = state.finish_indexing(SatzState::initialize_index(missing.clone()), &missing);
    assert_eq!(outcome.failure, None);
    assert_eq!(outcome.doc_count, 1);
    assert_eq!(doc_ids(&state), vec!["b.md", "c.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unusable_config_is_reported_with_the_index_and_defaults_are_used() {
    let dir = scratch_dir("config");
    std::fs::write(dir.join("a.md"), "# A\n").unwrap();
    std::fs::write(dir.join(".satz.toml"), "this is = = not toml").unwrap();
    let mut state = SatzState::default();
    let outcome = state.finish_indexing(SatzState::initialize_index(dir.clone()), &dir);
    assert_eq!(outcome.failure, None);
    assert!(outcome.config_error.is_some());
    assert!(state.config_error.is_some());
    assert_eq!(doc_ids(&state), vec!["a.md"]);
    // A failed index with a broken config reports the config problem too.
    let missing = dir.join("nope");
    let outcome = state.finish_indexing(SatzState::initialize_index(missing.clone()), &missing);
    assert!(outcome.failure.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- re-parsing an open document without holding the lock while parsing ----

fn open_state(text: &str) -> SatzState {
    let mut state = SatzState::default();
    state.vault_root = Some(PathBuf::from("/vault"));
    state.open_document("file:///a.md", text, Path::new("/vault/a.md"), 1);
    state
}

fn type_into(state: &mut SatzState, version: i32, new_text: &str) {
    let doc = state.open_docs.get_mut("file:///a.md").unwrap();
    doc.rope = Rope::from_str(new_text);
    doc.version = version;
}

fn links_of_a(state: &SatzState) -> Vec<String> {
    state
        .index
        .get_doc(&satz_core::DocId::new("a.md"))
        .unwrap()
        .links
        .iter()
        .map(|l| l.target_doc.clone())
        .collect()
}

#[test]
fn a_reparse_job_carries_the_text_and_version_it_was_prepared_from() {
    let mut state = open_state("# A\n\n[[one]]\n");
    type_into(&mut state, 2, "# A\n\n[[two]] [[three]]\n");
    let job = state
        .prepare_reparse("file:///a.md")
        .expect("an open document");
    assert_eq!(job.version, 2);
    assert_eq!(job.content, "# A\n\n[[two]] [[three]]\n");
    assert_eq!(job.rel_path, PathBuf::from("a.md"));
    assert!(state.prepare_reparse("file:///unknown.md").is_none());
}

#[test]
fn a_reparse_of_the_current_version_is_applied() {
    let mut state = open_state("# A\n\n[[one]]\n");
    type_into(&mut state, 2, "# A\n\n[[two]]\n");
    let job = state.prepare_reparse("file:///a.md").unwrap();
    // The parse happens with no lock and no state at all.
    let parsed = satz_core::parse_document(&job.content, &job.rel_path);
    assert!(state.apply_reparse("file:///a.md", job.version, parsed));
    assert_eq!(links_of_a(&state), vec!["two".to_string()]);
    assert!(state.open_docs["file:///a.md"].first_change_at.is_none());
}

#[test]
fn a_reparse_that_the_user_has_typed_past_is_dropped() {
    let mut state = open_state("# A\n\n[[one]]\n");
    type_into(&mut state, 2, "# A\n\n[[two]]\n");
    let job = state.prepare_reparse("file:///a.md").unwrap();
    let parsed = satz_core::parse_document(&job.content, &job.rel_path);

    // While the parse ran (outside the lock) the user typed more.
    type_into(&mut state, 3, "# A\n\n[[three]]\n");
    state.peers_dirty = false;
    assert!(!state.apply_reparse("file:///a.md", job.version, parsed));
    assert_eq!(
        links_of_a(&state),
        vec!["one".to_string()],
        "the index keeps what it had; the newer text has its own reparse queued"
    );
    assert!(!state.peers_dirty, "a dropped result changes nothing");
}

#[test]
fn peers_are_marked_only_when_a_reparse_is_applied_and_changes_what_they_see() {
    let mut state = open_state("# A\n\n[[one]]\n");
    state.peers_dirty = false;
    // Same signature (the link to `one` stays): not dirty.
    type_into(&mut state, 2, "# A\n\n[[one]] and more text\n");
    let job = state.prepare_reparse("file:///a.md").unwrap();
    let parsed = satz_core::parse_document(&job.content, &job.rel_path);
    assert!(state.apply_reparse("file:///a.md", job.version, parsed));
    assert!(!state.peers_dirty);
    // A new heading is something other documents can link to: dirty.
    type_into(&mut state, 3, "# A\n\n## New heading\n\n[[one]]\n");
    let job = state.prepare_reparse("file:///a.md").unwrap();
    let parsed = satz_core::parse_document(&job.content, &job.rel_path);
    assert!(state.apply_reparse("file:///a.md", job.version, parsed));
    assert!(state.peers_dirty);
}

#[test]
fn a_reparse_for_a_document_that_was_closed_meanwhile_is_dropped() {
    let mut state = open_state("# A\n");
    // A reparse is only prepared for a buffer the index is behind (the user has typed).
    type_into(&mut state, 2, "# A\n\ntyped\n");
    let job = state.prepare_reparse("file:///a.md").unwrap();
    let parsed = satz_core::parse_document(&job.content, &job.rel_path);
    state.close_document("file:///a.md");
    assert!(!state.apply_reparse("file:///a.md", job.version, parsed));
}

#[test]
fn the_synchronous_reparse_still_works_for_save_and_format() {
    let mut state = open_state("# A\n\n[[one]]\n");
    type_into(&mut state, 5, "# A\n\n[[five]]\n");
    state.reparse_open_document("file:///a.md");
    assert_eq!(links_of_a(&state), vec!["five".to_string()]);
    // Unknown documents are ignored.
    state.reparse_open_document("file:///nope.md");
}

// ---- an index that always reflects the open buffers ----

fn edit_buffer(state: &mut SatzState, version: i32, new_text: &str) {
    let doc = state.open_docs.get_mut("file:///a.md").unwrap();
    let changes = vec![TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: new_text.to_string(),
    }];
    assert!(doc.apply_change_events(version, changes));
}

#[test]
fn a_freshly_opened_document_is_not_stale_and_typing_makes_it_so() {
    let mut state = open_state("# A\n\n[[one]]\n");
    assert!(!state.has_stale_open_documents());
    edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
    assert!(state.has_stale_open_documents());
    assert_eq!(
        links_of_a(&state),
        vec!["one".to_string()],
        "the index is one edit behind"
    );
}

#[test]
fn refreshing_brings_the_index_up_to_the_buffers_once() {
    let mut state = open_state("# A\n\n[[one]]\n");
    edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
    assert_eq!(state.refresh_stale_open_documents(), 1);
    assert_eq!(links_of_a(&state), vec!["two".to_string()]);
    assert!(!state.has_stale_open_documents());
    assert_eq!(
        state.refresh_stale_open_documents(),
        0,
        "nothing left to do"
    );
    // A reparse task that fires later finds nothing to do either.
    assert!(state.prepare_reparse("file:///a.md").is_none());
}

#[test]
fn a_refresh_by_a_request_leaves_its_notifications_owed_exactly_once() {
    let mut state = open_state("# A\n\n[[one]]\n");
    assert!(
        !state.take_announce_pending("file:///a.md"),
        "a document nobody refreshed owes nothing"
    );

    edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
    assert!(
        !state.take_announce_pending("file:///a.md"),
        "typing alone is the debounced task's business"
    );
    assert_eq!(state.refresh_stale_open_documents(), 1);
    assert!(state.take_announce_pending("file:///a.md"), "now owed");
    assert!(
        !state.take_announce_pending("file:///a.md"),
        "taken: sent once"
    );

    assert_eq!(state.refresh_stale_open_documents(), 0);
    assert!(
        !state.take_announce_pending("file:///a.md"),
        "a refresh that found nothing to parse owes nothing"
    );
    assert!(!state.take_announce_pending("file:///nope.md"));
}

#[test]
fn whoever_applies_a_parse_next_takes_over_the_notifications() {
    // A request refreshed the buffer (debt), then the user typed on: the debounced task that
    // parses the newer text announces for both, so nothing is owed any more.
    let mut state = open_state("# A\n\n[[one]]\n");
    edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
    state.refresh_stale_open_documents();
    edit_buffer(&mut state, 3, "# A\n\n[[three]]\n");
    state.reparse_open_document("file:///a.md");
    assert!(!state.take_announce_pending("file:///a.md"));
}

#[test]
fn a_change_that_reuses_the_version_number_is_still_seen_as_stale() {
    let mut state = open_state("# A\n\n[[one]]\n");
    edit_buffer(&mut state, 1, "# A\n\n[[same-version]]\n");
    assert!(state.has_stale_open_documents());
    assert_eq!(state.refresh_stale_open_documents(), 1);
    assert_eq!(links_of_a(&state), vec!["same-version".to_string()]);
}

#[test]
fn only_the_stale_documents_are_reparsed_and_closed_ones_do_not_count() {
    let mut state = open_state("# A\n");
    state.open_document("file:///b.md", "# B\n", Path::new("/vault/b.md"), 1);
    state.open_document("file:///c.md", "# C\n", Path::new("/vault/c.md"), 1);
    edit_buffer(&mut state, 2, "# A\n\nnew\n");
    {
        let c = state.open_docs.get_mut("file:///c.md").unwrap();
        c.apply_change_events(
            2,
            vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "# C\n\nnew\n".to_string(),
            }],
        );
    }
    assert_eq!(state.refresh_stale_open_documents(), 2);
    state.close_document("file:///a.md");
    assert!(!state.has_stale_open_documents());
}

#[test]
fn refreshing_marks_peers_only_when_what_they_depend_on_changed() {
    let mut state = open_state("# A\n\n[[one]]\n");
    state.peers_dirty = false;
    edit_buffer(&mut state, 2, "# A\n\n[[one]] more words\n");
    state.refresh_stale_open_documents();
    assert!(!state.peers_dirty);
    edit_buffer(&mut state, 3, "# A\n\n## New heading\n\n[[one]]\n");
    state.refresh_stale_open_documents();
    assert!(state.peers_dirty);
}

#[test]
fn a_big_buffer_is_parsed_once_and_then_answered_from_the_index() {
    let mut state = open_state("# A\n");
    let big = format!("# A\n\n{}", "some words [[x]] and more\n".repeat(80_000));
    edit_buffer(&mut state, 2, &big);
    assert_eq!(state.refresh_stale_open_documents(), 1);
    assert_eq!(
        state.refresh_stale_open_documents(),
        0,
        "the second pass parsed the buffer again"
    );
    assert!(links_of_a(&state).len() >= 80_000);
}

// ---- daily-note aliases follow the config and the calendar ----

fn day(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn daily_vault() -> SatzState {
    let mut state = SatzState::default();
    state.vault_root = Some(PathBuf::from("/vault"));
    state.index = satz_core::Index::build(vec![
        satz_core::parse_document(
            "# Log

[[bugün]]
",
            Path::new("log.md"),
        ),
        satz_core::parse_document(
            "# 14
",
            Path::new("daily/2026-03-14.md"),
        ),
        satz_core::parse_document(
            "# 15
",
            Path::new("daily/2026-03-15.md"),
        ),
    ]);
    state
}

fn backlinks_to(state: &SatzState, path: &str) -> usize {
    state
        .index
        .backlinks_of(&satz_core::DocId::new(path))
        .count()
}

#[test]
fn syncing_the_daily_date_gives_the_alias_link_its_backlink() {
    let mut state = daily_vault();
    assert!(state.daily_is_stale(day(2026, 3, 14)), "nothing set yet");
    assert!(state.sync_daily(day(2026, 3, 14)));
    assert_eq!(backlinks_to(&state, "daily/2026-03-14.md"), 1);
    assert!(!state.daily_is_stale(day(2026, 3, 14)));
    assert!(
        !state.sync_daily(day(2026, 3, 14)),
        "nothing changed the second time"
    );
}

#[test]
fn a_new_day_makes_the_daily_setting_stale_and_moves_the_backlink() {
    let mut state = daily_vault();
    state.sync_daily(day(2026, 3, 14));
    assert!(state.daily_is_stale(day(2026, 3, 15)), "midnight passed");
    assert!(state.sync_daily(day(2026, 3, 15)));
    assert_eq!(backlinks_to(&state, "daily/2026-03-14.md"), 0);
    assert_eq!(backlinks_to(&state, "daily/2026-03-15.md"), 1);
}

#[test]
fn a_changed_daily_config_is_stale_even_on_the_same_day() {
    let mut state = daily_vault();
    state.sync_daily(day(2026, 3, 14));
    state.config.daily_note.aliases.today = vec!["heute".to_string()];
    assert!(state.daily_is_stale(day(2026, 3, 14)));
    assert!(state.sync_daily(day(2026, 3, 14)));
    assert_eq!(
        backlinks_to(&state, "daily/2026-03-14.md"),
        0,
        "`bugün` is no longer an alias"
    );
}

#[test]
fn syncing_the_daily_date_asks_for_the_peers_to_be_refreshed_only_when_something_changed() {
    let mut state = daily_vault();
    state.peers_dirty = false;
    state.sync_daily(day(2026, 3, 14));
    assert!(state.peers_dirty, "orphan status of the daily note changed");
    state.peers_dirty = false;
    state.sync_daily(day(2026, 3, 14));
    assert!(!state.peers_dirty);
}

// ---- what the other open documents are told after a change ----

fn three_open_docs() -> SatzState {
    let mut state = SatzState::default();
    state.vault_root = Some(PathBuf::from("/vault"));
    for name in ["c", "a", "b"] {
        state.open_document(
            &format!("file:///{name}.md"),
            "# T\n",
            Path::new(&format!("/vault/{name}.md")),
            1,
        );
    }
    state.peers_dirty = false;
    state
}

#[test]
fn nothing_dirty_means_nothing_to_tell_and_the_others_are_listed_in_order() {
    let mut state = three_open_docs();
    let peers = state.take_peer_refresh("file:///a.md", true);
    assert!(!peers.dirty);
    assert_eq!(peers.others, vec!["file:///b.md", "file:///c.md"]);
}

#[test]
fn a_dirty_flag_is_taken_once_when_the_change_took_effect() {
    let mut state = three_open_docs();
    state.peers_dirty = true;
    state.client_supports_pull_diagnostics = true;
    let peers = state.take_peer_refresh("file:///b.md", true);
    assert!(peers.dirty && peers.supports_pull);
    assert!(!state.peers_dirty, "taken");
    assert!(
        !state.take_peer_refresh("file:///b.md", true).dirty,
        "and not told twice"
    );
}

#[test]
fn a_change_that_did_not_take_effect_leaves_the_flag_for_the_one_that_did() {
    let mut state = three_open_docs();
    state.peers_dirty = true;
    let peers = state.take_peer_refresh("file:///b.md", false);
    assert!(!peers.dirty, "nothing to tell yet");
    assert!(state.peers_dirty, "still pending");
    assert!(state.take_peer_refresh("file:///b.md", true).dirty);
}

#[test]
fn the_changed_document_and_unknown_uris_are_handled() {
    let mut state = three_open_docs();
    let peers = state.take_peer_refresh("file:///nope.md", true);
    assert_eq!(peers.others.len(), 3);
    state.close_document("file:///a.md");
    state.close_document("file:///b.md");
    state.close_document("file:///c.md");
    assert!(
        state
            .take_peer_refresh("file:///a.md", true)
            .others
            .is_empty()
    );
}

// ---- the state's own invariants are read and changed through methods ----

#[test]
fn a_new_state_has_no_root_is_not_indexed_and_has_nothing_dirty() {
    let state = SatzState::default();
    assert_eq!(state.vault_root(), None);
    assert!(!state.is_indexing_complete());
    assert!(!state.peers_dirty());
}

#[test]
fn the_indexing_flag_and_the_dirty_flag_can_be_set_and_cleared_repeatedly() {
    let mut state = SatzState::default();
    state.set_indexing_complete(true);
    state.set_indexing_complete(true);
    assert!(state.is_indexing_complete());
    state.set_indexing_complete(false);
    assert!(!state.is_indexing_complete());
    state.mark_peers_dirty();
    state.mark_peers_dirty();
    assert!(state.peers_dirty());
    state.clear_peers_dirty();
    state.clear_peers_dirty();
    assert!(!state.peers_dirty());
}

#[test]
fn a_finished_first_index_is_complete_and_keeps_the_root() {
    let mut state = SatzState::with_vault_root("/vault");
    assert_eq!(state.vault_root(), Some(Path::new("/vault")));
    let fresh = SatzState::with_vault_root("/vault");
    let mut fresh = fresh;
    fresh.set_indexing_complete(true);
    state.finish_indexing(Ok(fresh), Path::new("/vault"));
    assert!(state.is_indexing_complete());
    assert_eq!(state.vault_root(), Some(Path::new("/vault")));
}

#[test]
fn changing_the_root_changes_how_paths_are_made_relative() {
    let mut state = SatzState::with_vault_root("/vault");
    let inside =
        |s: &SatzState| SatzState::get_rel_path(Path::new("/vault/sub/a.md"), s.vault_root());
    assert_eq!(inside(&state), PathBuf::from("sub/a.md"));
    state.set_vault_root(Some(PathBuf::from("/vault/sub")));
    assert_eq!(inside(&state), PathBuf::from("a.md"));
    state.set_vault_root(None);
    assert_eq!(inside(&state), PathBuf::from("/vault/sub/a.md"));
}

#[test]
fn a_finished_index_keeps_what_the_client_can_do() {
    let mut state = SatzState::default();
    state.client_supports_pull_diagnostics = true;
    state.client_supports_document_changes = true;
    state.finish_indexing(Ok(SatzState::default()), Path::new("/vault"));
    assert!(state.client_supports_pull_diagnostics);
    assert!(state.client_supports_document_changes);
}

// ---- a config with mistakes: the rest applies, the mistakes are reported ----

#[test]
fn unknown_keys_and_bad_values_are_warnings_and_formatting_stays_on() {
    for (label, content, expect) in [
        (
            "unknown key",
            "[formatter.wrap]\nenabled = true\n",
            "enabled",
        ),
        (
            "bad daily format",
            "[daily_note]\nformat = \"%Q\"\n",
            "daily_note.format",
        ),
        (
            "bad choice",
            "[formatter.misc]\nhr_style = \"====\"\n",
            "hr_style",
        ),
    ] {
        let v = TempVault::new("warnkey");
        v.config(&format!("{content}\n[hover]\npreview_lines = 3\n"));
        let state = SatzState::initialize_index(v.0.clone()).unwrap();
        assert_eq!(state.config_error, None, "{label}");
        assert_eq!(
            state.config.hover.preview_lines, 3,
            "{label}: the rest applies"
        );
        assert_eq!(
            state.config_warnings.len(),
            1,
            "{label}: {:?}",
            state.config_warnings
        );
        assert!(
            state.config_warnings[0].contains(expect),
            "{label}: {:?}",
            state.config_warnings
        );
        assert!(state.formatting_allowed(), "{label}");
    }
}

#[test]
fn the_warnings_come_back_with_the_indexing_outcome_and_survive_the_index_swap() {
    let v = TempVault::new("warnswap");
    v.config("[bogus]\nx = 1\n");
    let fresh = SatzState::initialize_index(v.0.clone()).unwrap();
    let mut state = SatzState::default();
    let outcome = state.finish_indexing(Ok(fresh), &v.0);
    assert_eq!(outcome.config_warnings.len(), 1);
    assert_eq!(state.config_warnings, outcome.config_warnings);
    // A failed walk still loads the settings and reports their mistakes.
    let mut state = SatzState::default();
    let outcome = state.finish_indexing(Err(anyhow::anyhow!("no folder")), &v.0);
    assert!(outcome.failure.is_some());
    assert_eq!(outcome.config_warnings.len(), 1);
}

#[test]
fn the_warning_message_lists_every_ignored_setting_and_says_the_rest_applies() {
    let message = config_warnings_message(&[
        ".satz.toml: unknown setting formatter.wrap.enabled (ignored)".to_string(),
        ".satz.toml: invalid formatter.misc.hr_style \"====\"; using \"---\"".to_string(),
    ]);
    assert!(message.contains("formatter.wrap.enabled"), "{message}");
    assert!(message.contains("hr_style"), "{message}");
    assert!(message.contains("everything else"), "{message}");
    assert_eq!(
        message.lines().count(),
        3,
        "a header and one line each: {message}"
    );
    assert_eq!(config_warnings_message(&[]), "");
}

// ---- a byte order mark at the start of the text ----

#[test]
fn a_byte_order_mark_is_not_part_of_the_open_buffer_so_buffer_and_index_agree() {
    let mut state = SatzState::with_vault_root("/vault");
    state.open_document(
        "file:///a.md",
        "\u{feff}# T\n\n[[b]] here\n",
        Path::new("/vault/a.md"),
        1,
    );
    let open = &state.open_docs["file:///a.md"];
    let buffer = open.rope.to_string();
    let indexed = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
    assert_eq!(buffer, "# T\n\n[[b]] here\n");
    assert_eq!(
        buffer,
        indexed.line_index.source(),
        "the live text and the parsed text are the same text"
    );
    assert!(!state.has_stale_open_documents());
}

#[test]
fn a_buffer_without_a_mark_and_one_that_is_only_a_mark_are_handled() {
    let mut state = SatzState::with_vault_root("/vault");
    state.open_document("file:///a.md", "# T\n", Path::new("/vault/a.md"), 1);
    assert_eq!(state.open_docs["file:///a.md"].rope.to_string(), "# T\n");
    state.open_document("file:///b.md", "\u{feff}", Path::new("/vault/b.md"), 1);
    assert_eq!(state.open_docs["file:///b.md"].rope.to_string(), "");
}
