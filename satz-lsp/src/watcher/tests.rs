// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

use super::*;

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }
}

#[test]
fn only_the_vault_roots_dot_satz_toml_is_the_config_file() {
    let r = root();
    for (rel, expected) in [
        (".satz.toml", true),
        ("sub/.satz.toml", false),
        ("a/b/.satz.toml", false),
        ("satz.toml", false),
        ("sub/satz.toml", false),
        (".satz.toml.bak", false),
        (".satz.tom", false),
        ("x.satz.toml", false),
        (".SATZ.TOML", false),
        ("notes.md", false),
        (".satz.toml/inner.md", false),
    ] {
        assert_eq!(
            is_config_file(&r.join(rel), &r),
            expected,
            "relative path {rel:?}"
        );
    }
}

#[test]
fn config_file_is_recognised_despite_a_case_different_vault_prefix() {
    // Windows drive letters / folder names differ in case between the client and the OS.
    if cfg!(windows) {
        let path = PathBuf::from("c:\\VAULT\\.satz.toml");
        assert!(is_config_file(&path, &root()));
    }
}

#[test]
fn a_path_outside_the_vault_is_never_the_config_file() {
    let other = if cfg!(windows) {
        PathBuf::from("D:\\elsewhere\\.satz.toml")
    } else {
        PathBuf::from("/elsewhere/.satz.toml")
    };
    assert!(!is_config_file(&other, &root()));
}

struct TempVault(PathBuf);
impl TempVault {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "satz_watch_{tag}_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, content: &str) {
        std::fs::write(self.0.join(".satz.toml"), content).unwrap();
    }
    fn delete(&self) {
        std::fs::remove_file(self.0.join(".satz.toml")).unwrap();
    }
}
impl Drop for TempVault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn reload_config_keeps_the_previous_settings_on_error_and_recovers_when_fixed() {
    let v = TempVault::new("reload");
    let mut state = SatzState::default();

    v.write("[hover]\npreview_lines = 3\n");
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
    assert_eq!(state.config.hover.preview_lines, 3);
    assert_eq!(state.config_error, None);

    // Broken TOML: previous settings stay, the error is recorded and reported.
    v.write("[hover\npreview_lines = 9\n");
    let outcome = reload_config(&mut state, &v.0);
    let ReloadOutcome::Failed(message) = outcome else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert!(message.contains(".satz.toml"), "{message}");
    assert_eq!(state.config.hover.preview_lines, 3, "previous config kept");
    assert_eq!(state.config_error.as_deref(), Some(message.as_str()));
    assert!(!state.formatting_allowed());

    // Fixed: new settings apply and the error clears.
    v.write("[hover]\npreview_lines = 5\n");
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
    assert_eq!(state.config.hover.preview_lines, 5);
    assert_eq!(state.config_error, None);
    assert!(state.formatting_allowed());

    // A typo'd key no longer stops the reload: the valid keys next to it apply and the typo is
    // reported as a warning; formatting stays on.
    v.write("[hover]\npreview_lines = 7\nbogus = 1\n");
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
    assert_eq!(state.config.hover.preview_lines, 7);
    assert_eq!(
        state.config_warnings.len(),
        1,
        "{:?}",
        state.config_warnings
    );
    assert!(state.config_warnings[0].contains("bogus"));
    assert!(state.formatting_allowed());

    // Fixed: the warning goes away.
    v.write("[hover]\npreview_lines = 7\n");
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
    assert!(state.config_warnings.is_empty());

    // A value of the wrong type is still an error, and nothing is half-applied.
    v.write("[hover]\npreview_lines = \"many\"\nline = 1\n");
    assert!(matches!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::Failed(m) if m.contains("preview_lines")
    ));
    assert_eq!(state.config.hover.preview_lines, 7, "nothing half-applied");
}

#[test]
fn a_reload_with_mistakes_applies_the_rest_and_keeps_the_warnings_until_it_is_fixed() {
    let v = TempVault::new("reload-warn");
    let mut state = SatzState::default();
    v.write("[formatter.wrap]\nenabled = true\n[formatter.misc]\nhr_style = \"====\"\n[daily_note]\nfolder = \"j\"\n");
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
    assert_eq!(state.config.daily_note.folder, "j");
    assert_eq!(state.config.formatter.misc.hr_style, "---");
    assert_eq!(
        state.config_warnings.len(),
        2,
        "{:?}",
        state.config_warnings
    );
    assert!(state.formatting_allowed());
    // Deleting the file: defaults, no warnings.
    v.delete();
    assert_eq!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::RevertedToDefaults
    );
    assert!(state.config_warnings.is_empty());
}

#[test]
fn reload_config_when_the_file_is_deleted_reverts_to_defaults_and_clears_the_error() {
    let v = TempVault::new("delete");
    let mut state = SatzState::default();

    v.write("[hover]\npreview_lines = 3\n");
    reload_config(&mut state, &v.0);
    assert_eq!(state.config.hover.preview_lines, 3);
    v.delete();
    assert_eq!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::RevertedToDefaults
    );
    assert_eq!(state.config, satz_core::VaultConfig::default());
    assert_eq!(state.config_error, None);

    // Deleting a file that was in an error state clears the error as well.
    v.write("not = [valid\n");
    assert!(matches!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::Failed(_)
    ));
    assert!(state.config_error.is_some());
    v.delete();
    assert_eq!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::RevertedToDefaults
    );
    assert_eq!(state.config_error, None);
    assert!(state.formatting_allowed());
}

#[test]
fn reload_config_reports_an_unreadable_config_path() {
    // A DIRECTORY named .satz.toml can't be read as a file.
    let v = TempVault::new("isdir");
    std::fs::create_dir_all(v.0.join(".satz.toml")).unwrap();
    let mut state = SatzState::default();

    let outcome = reload_config(&mut state, &v.0);

    assert!(matches!(outcome, ReloadOutcome::Failed(_)), "{outcome:?}");
    assert!(state.config_error.is_some());
}

// ---- on-disk changes vs. open documents ----

fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "satz-watch-{}-{}-{}",
        std::process::id(),
        tag,
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn state_in(dir: &Path) -> SatzState {
    let mut state = SatzState::default();
    state.set_vault_root(Some(dir.to_path_buf()));
    state.set_indexing_complete(true);
    state
}

fn targets(state: &SatzState, id: &str) -> Option<Vec<String>> {
    state
        .index
        .get_doc(&satz_core::DocId::new(id))
        .map(|d| d.links.iter().map(|l| l.target_doc.clone()).collect())
}

#[test]
fn a_closed_note_is_reindexed_on_change_and_dropped_on_delete() {
    let dir = temp_dir("closed");
    let path = dir.join("a.md");
    std::fs::write(&path, "# A\n\n[[one]]\n").unwrap();
    let mut state = state_in(&dir);

    assert_eq!(
        apply_fs_change(&mut state, &dir, &path),
        FsChange::Reindexed
    );
    assert_eq!(targets(&state, "a.md"), Some(vec!["one".into()]));

    std::fs::write(&path, "# A\n\n[[two]]\n").unwrap();
    assert_eq!(
        apply_fs_change(&mut state, &dir, &path),
        FsChange::Reindexed
    );
    assert_eq!(targets(&state, "a.md"), Some(vec!["two".into()]));

    std::fs::remove_file(&path).unwrap();
    assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Removed);
    assert_eq!(targets(&state, "a.md"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_open_note_is_never_dropped_when_its_file_briefly_disappears() {
    // Editors that save by writing a temp file and renaming leave a window with no file.
    let dir = temp_dir("open-missing");
    let path = dir.join("a.md");
    let mut state = state_in(&dir);
    state.open_document("file:///a.md", "# A\n\n[[buffer]]\n", &path, 1);
    assert!(!path.exists());

    assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Skipped);

    assert_eq!(targets(&state, "a.md"), Some(vec!["buffer".into()]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_open_note_is_not_overwritten_by_its_file_on_disk() {
    let dir = temp_dir("open-modify");
    let path = dir.join("a.md");
    std::fs::write(&path, "# A\n\n[[disk]]\n").unwrap();
    let mut state = state_in(&dir);
    state.open_document("file:///a.md", "# A\n\n[[buffer]]\n", &path, 1);

    assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Skipped);

    assert_eq!(targets(&state, "a.md"), Some(vec!["buffer".into()]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_differently_spelled_event_path_still_finds_the_open_note() {
    // notify may report the path with other casing (Windows) than the client's URI gave.
    let dir = temp_dir("open-case");
    let path = dir.join("Notes.md");
    std::fs::write(&path, "# N\n\n[[disk]]\n").unwrap();
    let mut state = state_in(&dir);
    state.open_document("file:///n.md", "# N\n\n[[buffer]]\n", &path, 1);

    let event_path = dir.join("NOTES.md");
    let expected = if event_path.exists() {
        // Case-insensitive file system: the same file, spelled differently.
        FsChange::Skipped
    } else {
        // Case-sensitive: a different path; only the open-note check is being probed, so
        // stop here rather than assert on a file that does not exist.
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    assert_eq!(apply_fs_change(&mut state, &dir, &event_path), expected);
    assert_eq!(targets(&state, "Notes.md"), Some(vec!["buffer".into()]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_path_matching_ignores_case_and_separator_spelling() {
    let mut state = SatzState::default();
    state.set_vault_root(Some(PathBuf::from("/Vault")));
    state.open_document("file:///x", "# X\n", Path::new("/Vault/Sub/X.md"), 1);
    assert!(state.is_open_path(Path::new("/Vault/Sub/X.md")));
    assert!(state.is_open_path(Path::new("/vault/sub/x.md")));
    assert!(!state.is_open_path(Path::new("/Vault/Sub/Y.md")));
    assert!(!state.is_open_path(Path::new("/Vault/Other/X.md")));
}

#[test]
fn an_unreadable_path_is_skipped_without_touching_the_index() {
    let dir = temp_dir("unreadable");
    let path = dir.join("a.md");
    std::fs::create_dir_all(&path).unwrap(); // exists, but is a directory
    let mut state = state_in(&dir);
    assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Skipped);
    assert_eq!(targets(&state, "a.md"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the workspace-format cache must not outlive the settings it was computed with ----

fn state_with_list_doc() -> SatzState {
    let mut state = SatzState::default();
    state.index = satz_core::Index::build(vec![satz_core::parse_document(
        "- a\n- b\n",
        Path::new("a.md"),
    )]);
    state
}

fn format_and_cache(state: &mut SatzState) -> Vec<String> {
    let result = crate::handlers::execute_command::compute_format_changes(state);
    for update in result.cache_updates {
        match update {
            crate::state::CacheUpdate::Unchanged(hash) => state.format_cache.insert_unchanged(hash),
            crate::state::CacheUpdate::Formatted(hash, formatted) => {
                state.format_cache.insert(hash, formatted)
            }
        }
    }
    // What each change makes of the (only) note.
    let source = state
        .index
        .documents()
        .next()
        .map(|doc| doc.line_index.source().to_string())
        .unwrap_or_default();
    result
        .changes
        .iter()
        .map(|c| crate::convert::apply_text_edits(&source, &c.edits))
        .collect()
}

#[test]
fn changing_formatter_settings_changes_what_workspace_format_produces() {
    let v = TempVault::new("fmt-cache");
    let mut state = state_with_list_doc();
    state.set_vault_root(Some(v.0.clone()));

    // Default settings: already clean. The (unchanged) result is cached.
    assert_eq!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::RevertedToDefaults
    );
    assert!(format_and_cache(&mut state).is_empty());
    assert!(!state.format_cache.is_empty());

    // The user switches the list marker: the next run must use the new setting.
    v.write("[formatter.lists]\nmarker = \"*\"\n");
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
    assert_eq!(format_and_cache(&mut state), vec!["* a\n* b\n".to_string()]);

    // And back again.
    v.delete();
    assert_eq!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::RevertedToDefaults
    );
    assert!(format_and_cache(&mut state).is_empty());
}

#[test]
fn a_reload_replaces_the_cache_and_applies_the_new_capacity() {
    let v = TempVault::new("fmt-cap");
    let mut state = SatzState::default();
    state.format_cache.insert(1, "x".to_string());

    v.write("[lsp]\nformat_cache_capacity = 7\n");
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);

    assert!(state.format_cache.is_empty());
    for hash in 0..20u64 {
        state.format_cache.insert(hash, String::new());
    }
    assert_eq!(state.format_cache.len(), 7);
}

#[test]
fn a_failed_reload_keeps_the_settings_and_so_the_cache() {
    let v = TempVault::new("fmt-failed");
    let mut state = SatzState::default();
    state.format_cache.insert(1, "kept".to_string());

    v.write("[formatter\nbroken = \n");
    assert!(matches!(
        reload_config(&mut state, &v.0),
        ReloadOutcome::Failed(_)
    ));

    assert_eq!(state.format_cache.get(1), Some("kept"));
}

// ---- folders: deleting, renaming or moving one changes many notes at once ----

fn write(dir: &Path, rel: &str, text: &str) -> PathBuf {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

/// Indexes every given file the way the watcher would on its first event.
fn index_files(state: &mut SatzState, dir: &Path, rels: &[&str]) {
    for rel in rels {
        apply_fs_change(state, dir, &dir.join(rel));
    }
}

fn ids(state: &SatzState) -> Vec<String> {
    let mut ids: Vec<String> = state
        .index
        .documents()
        .map(|d| d.id.as_str().to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn deleting_a_folder_removes_every_note_in_it_from_the_index() {
    let dir = temp_dir("dir-delete");
    write(&dir, "keep.md", "# keep\n");
    write(&dir, "docs/a.md", "# a\n");
    write(&dir, "docs/deep/b.md", "# b\n");
    let mut state = state_in(&dir);
    index_files(
        &mut state,
        &dir,
        &["keep.md", "docs/a.md", "docs/deep/b.md"],
    );
    assert_eq!(ids(&state), vec!["docs/a.md", "docs/deep/b.md", "keep.md"]);

    std::fs::remove_dir_all(dir.join("docs")).unwrap();
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("docs")),
        FsChange::Removed
    );
    assert_eq!(ids(&state), vec!["keep.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn renaming_a_folder_moves_its_notes_to_the_new_names() {
    let dir = temp_dir("dir-rename");
    write(&dir, "old/x.md", "# x\n");
    write(&dir, "old/sub/y.md", "# y\n");
    let mut state = state_in(&dir);
    index_files(&mut state, &dir, &["old/x.md", "old/sub/y.md"]);

    std::fs::rename(dir.join("old"), dir.join("new")).unwrap();
    // The watcher reports both paths.
    apply_fs_change(&mut state, &dir, &dir.join("old"));
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("new")),
        FsChange::Reindexed
    );
    assert_eq!(ids(&state), vec!["new/sub/y.md", "new/x.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_that_appears_is_read_with_the_gitignore_setting_of_the_config() {
    // No repository here; a `.gitignore` names one note of the folder.
    let dir = temp_dir("dir-gitignore-mode");
    write(
        &dir,
        ".gitignore",
        "secret.md
",
    );
    write(
        &dir,
        "pack/p.md",
        "# p
",
    );
    write(
        &dir,
        "pack/secret.md",
        "# secret
",
    );

    let mut state = state_in(&dir);
    apply_fs_change(&mut state, &dir, &dir.join("pack"));
    assert_eq!(ids(&state), vec!["pack/p.md", "pack/secret.md"]);

    let mut state = state_in(&dir);
    state.config.vault.gitignore = "always".to_string();
    apply_fs_change(&mut state, &dir, &dir.join("pack"));
    assert_eq!(ids(&state), vec!["pack/p.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_change_of_the_gitignore_setting_asks_for_a_restart_and_nothing_else_does() {
    use satz_core::GitignoreMode::{Always, InRepo};
    assert_eq!(gitignore_change_notice(InRepo, InRepo), None);
    assert_eq!(gitignore_change_notice(Always, Always), None);
    for (before, after) in [(InRepo, Always), (Always, InRepo)] {
        let notice = gitignore_change_notice(before, after).expect("a change is announced");
        assert!(notice.contains("vault.gitignore"), "{notice}");
        assert!(notice.contains("restart"), "{notice}");
    }
}

#[test]
fn a_folder_moved_in_from_outside_brings_its_notes() {
    let dir = temp_dir("dir-in");
    let outside = temp_dir("dir-in-outside");
    write(&outside, "pack/p.md", "# p\n[[q]]\n");
    write(&outside, "pack/inner/q.md", "# q\n");
    let mut state = state_in(&dir);

    std::fs::rename(outside.join("pack"), dir.join("pack")).unwrap();
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("pack")),
        FsChange::Reindexed
    );
    assert_eq!(ids(&state), vec!["pack/inner/q.md", "pack/p.md"]);
    assert_eq!(targets(&state, "pack/p.md"), Some(vec!["q".into()]));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn an_open_note_stays_indexed_when_its_folder_disappears() {
    let dir = temp_dir("dir-open");
    write(&dir, "docs/a.md", "# a\n");
    write(&dir, "docs/b.md", "# b\n");
    let mut state = state_in(&dir);
    index_files(&mut state, &dir, &["docs/a.md", "docs/b.md"]);
    state.open_document(
        "file:///a.md",
        "# a\n\n[[buffer]]\n",
        &dir.join("docs/a.md"),
        1,
    );

    std::fs::remove_dir_all(dir.join("docs")).unwrap();
    apply_fs_change(&mut state, &dir, &dir.join("docs"));
    assert_eq!(ids(&state), vec!["docs/a.md"], "only the open buffer stays");
    assert_eq!(targets(&state, "docs/a.md"), Some(vec!["buffer".into()]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_event_matches_whole_path_components_only() {
    let dir = temp_dir("dir-prefix");
    write(&dir, "notes/a.md", "# a\n");
    write(&dir, "notes2/b.md", "# b\n");
    write(&dir, "notes.md", "# n\n");
    write(&dir, "NOTES-old/c.md", "# c\n");
    let mut state = state_in(&dir);
    index_files(
        &mut state,
        &dir,
        &["notes/a.md", "notes2/b.md", "notes.md", "NOTES-old/c.md"],
    );

    std::fs::remove_dir_all(dir.join("notes")).unwrap();
    apply_fs_change(&mut state, &dir, &dir.join("notes"));
    assert_eq!(
        ids(&state),
        vec!["NOTES-old/c.md", "notes.md", "notes2/b.md"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_and_a_note_with_the_same_stem_do_not_get_mixed_up() {
    let dir = temp_dir("dir-stem");
    write(&dir, "a.md", "# note a\n");
    write(&dir, "a/inside.md", "# inside\n");
    let mut state = state_in(&dir);
    index_files(&mut state, &dir, &["a.md", "a/inside.md"]);

    std::fs::remove_dir_all(dir.join("a")).unwrap();
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("a")),
        FsChange::Removed
    );
    assert_eq!(ids(&state), vec!["a.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn events_that_change_nothing_are_skipped() {
    let dir = temp_dir("dir-noop");
    write(&dir, "a.md", "# a\n");
    let picture = write(&dir, "images/pic.png", "not text");
    let mut state = state_in(&dir);
    index_files(&mut state, &dir, &["a.md"]);

    // A file that is not a note, an empty folder, a folder that never held notes.
    assert_eq!(
        apply_fs_change(&mut state, &dir, &picture),
        FsChange::Skipped
    );
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("empty")),
        FsChange::Skipped
    );
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("never-existed")),
        FsChange::Skipped
    );
    assert_eq!(ids(&state), vec!["a.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_scan_of_a_folder_also_drops_notes_that_are_gone_from_it() {
    let dir = temp_dir("dir-rescan");
    write(&dir, "d/keep.md", "# keep\n");
    write(&dir, "d/gone.md", "# gone\n");
    let mut state = state_in(&dir);
    index_files(&mut state, &dir, &["d/keep.md", "d/gone.md"]);

    std::fs::remove_file(dir.join("d/gone.md")).unwrap();
    write(&dir, "d/new.md", "# new\n");
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("d")),
        FsChange::Reindexed
    );
    assert_eq!(ids(&state), vec!["d/keep.md", "d/new.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_markdown_extension_is_a_note_for_the_watcher_too() {
    let dir = temp_dir("markdown-ext");
    let path = write(&dir, "b.markdown", "# b\n");
    let mut state = state_in(&dir);
    assert_eq!(
        apply_fs_change(&mut state, &dir, &path),
        FsChange::Reindexed
    );
    assert_eq!(ids(&state), vec!["b.markdown"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deleting_a_big_folder_is_fast() {
    let dir = temp_dir("dir-big");
    let mut state = state_in(&dir);
    let mut docs: Vec<satz_core::Document> = (0..2000)
        .map(|i| satz_core::parse_document("# n\n", Path::new(&format!("big/n{i}.md"))))
        .collect();
    docs.push(satz_core::parse_document("# k\n", Path::new("keep.md")));
    state.index.replace_docs(docs);
    let start = std::time::Instant::now();
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("big")),
        FsChange::Removed
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(ids(&state), vec!["keep.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_paths_that_can_matter_are_queued() {
    let r = root();
    for (rel, expected) in [
        ("a.md", true),
        ("sub/b.markdown", true),
        (".satz.toml", true),
        ("folder", true),
        ("v1.2", true),
        ("sub/folder", true),
        ("image.png", true), // it might be a folder with a dot in its name: checked later
        (".git/HEAD", false),
        (".git", false),
        ("node_modules/x.md", false),
        ("a/node_modules", false),
        (".obsidian/app.json", false),
        (".hidden/note.md", false),
    ] {
        assert_eq!(is_relevant_path(&r.join(rel), &r), expected, "{rel:?}");
    }
}

// ---- changes that arrive before the first index is complete ----

#[test]
fn a_change_during_the_first_indexing_is_deferred_and_applied_once_it_is_done() {
    let dir = temp_dir("deferred");
    let path = write(&dir, "new.md", "# new\n[[x]]\n");
    let mut state = state_in(&dir);
    state.set_indexing_complete(false);

    assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Deferred);
    assert_eq!(ids(&state), Vec::<String>::new(), "nothing was applied");

    // The file changes again before indexing finishes; the retry reads the file as it is now.
    std::fs::write(&path, "# new\n[[y]]\n").unwrap();
    state.set_indexing_complete(true);
    assert_eq!(
        apply_fs_change(&mut state, &dir, &path),
        FsChange::Reindexed
    );
    assert_eq!(targets(&state, "new.md"), Some(vec!["y".into()]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folder_and_delete_events_are_deferred_too() {
    let dir = temp_dir("deferred-dir");
    write(&dir, "d/a.md", "# a\n");
    let mut state = state_in(&dir);
    state.set_indexing_complete(false);
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("d")),
        FsChange::Deferred
    );
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("gone.md")),
        FsChange::Deferred
    );
    assert_eq!(
        apply_fs_change(&mut state, &dir, &dir.join("gone")),
        FsChange::Deferred
    );
    assert!(ids(&state).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the debounce window ----

#[test]
fn repeated_events_for_one_path_are_one_piece_of_work_after_the_last_one() {
    let mut d = Debouncer::default();
    let t0 = Instant::now();
    let window = Duration::from_millis(200);
    d.push(PathBuf::from("a.md"), t0);
    d.push(PathBuf::from("a.md"), t0 + Duration::from_millis(100));
    d.push(PathBuf::from("a.md"), t0 + Duration::from_millis(150));
    // 200 ms after the FIRST event, but only 50 ms after the last: not ready.
    assert!(
        d.take_ready(t0 + Duration::from_millis(200), window)
            .is_empty()
    );
    assert!(!d.is_empty());
    let ready = d.take_ready(t0 + Duration::from_millis(350), window);
    assert_eq!(ready, vec![PathBuf::from("a.md")]);
    assert!(d.is_empty());
    assert!(d.take_ready(t0 + Duration::from_secs(9), window).is_empty());
}

#[test]
fn paths_are_debounced_independently_and_come_out_in_a_stable_order() {
    let mut d = Debouncer::default();
    let t0 = Instant::now();
    let window = Duration::from_millis(200);
    d.push(PathBuf::from("b.md"), t0);
    d.push(PathBuf::from("a.md"), t0);
    d.push(PathBuf::from("c.md"), t0 + Duration::from_millis(180));
    let ready = d.take_ready(t0 + Duration::from_millis(210), window);
    assert_eq!(ready, vec![PathBuf::from("a.md"), PathBuf::from("b.md")]);
    assert!(!d.is_empty(), "c.md is still waiting");
    // A deferred path is queued again and waits a whole window.
    d.push(PathBuf::from("a.md"), t0 + Duration::from_millis(210));
    let later = d.take_ready(t0 + Duration::from_millis(400), window);
    assert_eq!(later, vec![PathBuf::from("c.md")]);
    assert_eq!(
        d.take_ready(t0 + Duration::from_millis(500), window),
        vec![PathBuf::from("a.md")]
    );
}

#[test]
fn reloading_the_config_applies_the_daily_aliases_to_the_index() {
    let v = TempVault::new("reload-daily");
    let mut state = SatzState::default();
    v.write(
        "[daily_note.aliases]
today = [\"heute\"]
",
    );
    assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
    let (config, _) = state.index.daily().expect("daily set after a reload");
    assert_eq!(config.aliases.today, vec!["heute".to_string()]);
}

#[test]
fn only_changes_that_touched_the_index_refresh_diagnostics() {
    assert!(fs_change_needs_refresh(&FsChange::Reindexed));
    assert!(fs_change_needs_refresh(&FsChange::Removed));
    assert!(!fs_change_needs_refresh(&FsChange::Skipped));
    assert!(!fs_change_needs_refresh(&FsChange::Deferred));
}

#[test]
fn a_skipped_change_leaves_the_index_revision_alone() {
    // The premise of the rule above: a skipped change really does not touch the index.
    let mut state = SatzState::default();
    state.set_indexing_complete(true);
    let before = state.index.revision();
    let change = apply_prepared_batch(
        &mut state,
        vec![(PathBuf::from("/v/x.txt"), PreparedChange::Skip)],
    );
    assert_eq!(change, FsChange::Skipped);
    assert_eq!(state.index.revision(), before);
}

// ---- the watcher can be stopped ----

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_notify_thread_reports_changes_and_ends_when_stopped() {
    let v = TempVault::new("stop");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = WatcherHandle::default();
    let thread = spawn_notify_thread(v.0.clone(), tx, handle.clone());
    std::thread::sleep(Duration::from_millis(300)); // let it start watching

    std::fs::write(v.0.join("first.md"), "# one\n").unwrap();
    let mut seen = Vec::new();
    wait_until("an event for first.md", || {
        while let Ok(path) = rx.try_recv() {
            seen.push(path);
        }
        seen.iter().any(|p| p.ends_with("first.md"))
    });

    handle.stop();
    wait_until("the thread to end", || thread.is_finished());
    thread.join().unwrap();

    std::fs::write(v.0.join("second.md"), "# two\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(path) = rx.try_recv() {
        assert!(
            !path.ends_with("second.md"),
            "an event after the stop: {path:?}"
        );
    }
}

#[test]
fn a_stop_before_or_after_the_thread_ran_is_harmless() {
    let v = TempVault::new("stop-early");
    let (tx, _rx) = mpsc::unbounded_channel();
    let handle = WatcherHandle::default();
    handle.stop();
    handle.stop();
    let thread = spawn_notify_thread(v.0.clone(), tx, handle.clone());
    wait_until("the thread to end", || thread.is_finished());
    assert!(handle.is_stopped());
    handle.stop();
}

#[test]
fn a_missing_vault_root_ends_the_thread_at_once() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let missing = std::env::temp_dir().join("satz_watch_does_not_exist_xyz");
    let thread = spawn_notify_thread(missing, tx, WatcherHandle::default());
    wait_until("the thread to end", || thread.is_finished());
    // The channel is closed with it: nothing is left listening on a dead watcher.
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn saving_one_note_forwards_no_folder_event_that_would_rescan_the_whole_folder() {
    let v = TempVault::new("save-events");
    let sub = v.0.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    for i in 0..3 {
        std::fs::write(sub.join(format!("n{i}.md")), "# n\n").unwrap();
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = WatcherHandle::default();
    let thread = spawn_notify_thread(v.0.clone(), tx, handle.clone());
    std::thread::sleep(Duration::from_millis(400));

    // What an editor's save does: write the file (and here also once more, as some do).
    std::fs::write(sub.join("n1.md"), "# n changed\n").unwrap();
    std::fs::write(sub.join("n1.md"), "# n changed again\n").unwrap();
    std::thread::sleep(Duration::from_millis(1200));

    handle.stop();
    wait_until("the thread to end", || thread.is_finished());
    let mut seen = Vec::new();
    while let Ok(path) = rx.try_recv() {
        seen.push(path);
    }
    assert!(
        seen.iter().any(|p| p.ends_with("n1.md")),
        "the save was seen: {seen:?}"
    );
    // Some platforms report the folder as modified too (Windows does). Whatever is reported, a
    // folder whose notes the index already holds must not be scanned again.
    let mut state = SatzState::with_vault_root(v.0.clone());
    state.index = satz_core::Index::build(
        (0..3)
            .map(|i| satz_core::parse_document("# n\n", Path::new(&format!("sub/n{i}.md"))))
            .collect(),
    );
    for path in seen.iter().filter(|p| p.is_dir()) {
        assert!(
            !folder_event_is_news(&state, path, &v.0),
            "a save reported the folder {path:?}, which would re-read all its notes"
        );
    }
}

#[test]
fn a_folder_event_is_news_only_when_the_index_lacks_notes_of_that_folder() {
    let v = TempVault::new("folder-news");
    let known = v.0.join("known");
    let fresh = v.0.join("fresh");
    std::fs::create_dir_all(&known).unwrap();
    std::fs::create_dir_all(&fresh).unwrap();
    let mut state = SatzState::with_vault_root(v.0.clone());
    state.index = satz_core::Index::build(vec![
        satz_core::parse_document("# a\n", Path::new("known/a.md")),
        satz_core::parse_document("# b\n", Path::new("known/deeper/b.md")),
        satz_core::parse_document("# c\n", Path::new("knownish/c.md")),
    ]);
    assert!(
        !folder_event_is_news(&state, &known, &v.0),
        "notes of it are indexed"
    );
    assert!(
        folder_event_is_news(&state, &fresh, &v.0),
        "nothing of it is indexed"
    );
    // Spelling of the event path does not matter (case, separators).
    let upper = v.0.join("KNOWN");
    assert!(!folder_event_is_news(&state, &upper, &v.0));
    // A folder that only has a similarly named sibling in the index is still new.
    let mut only_sibling = SatzState::with_vault_root(v.0.clone());
    only_sibling.index = satz_core::Index::build(vec![satz_core::parse_document(
        "# c\n",
        Path::new("knownish/c.md"),
    )]);
    assert!(folder_event_is_news(&only_sibling, &known, &v.0));
    // An empty index: every folder is news.
    assert!(folder_event_is_news(
        &SatzState::with_vault_root(v.0.clone()),
        &known,
        &v.0
    ));
}

// ---- which notes a folder holds, and which notes are open (5.1): the same answers, with less work ----

/// `is_inside` as it was: every call folds both sides and builds the prefix to look for.
fn reference_is_inside(id: &str, prefix: &str) -> bool {
    let id = satz_core::fold_key(id);
    let prefix = satz_core::fold_key(prefix.trim_end_matches('/'));
    id == prefix || id.starts_with(&format!("{prefix}/"))
}

/// `folder_event_is_news` as it was.
fn reference_folder_event_is_news(state: &SatzState, folder: &Path, vault_root: &Path) -> bool {
    let rel = SatzState::get_rel_path(folder, Some(vault_root))
        .to_string_lossy()
        .replace('\\', "/");
    !state
        .index
        .documents()
        .any(|doc| reference_is_inside(doc.id.as_str(), &rel))
}

/// The `is_open` of `apply_prepared` as it was: goes through every open document each time.
fn reference_is_open(state: &SatzState, id: &satz_core::DocId) -> bool {
    state.open_docs.values().any(|open| {
        let rel = SatzState::get_rel_path(&open.path, state.vault_root());
        satz_core::fold_key(&rel.to_string_lossy().replace('\\', "/"))
            == satz_core::fold_key(id.as_str())
    })
}

struct T51Rng(u64);

impl T51Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len())]
    }
}

/// Path components that differ in case, in how an accent is written, in length by a letter, and
/// in what folds to what (the dotted capital I).
const T51_PARTS: [&str; 10] = [
    "a", "A", "ab", "aB", "é", "e\u{301}", "É", "İ", "x.md", "sub",
];

fn t51_path(rng: &mut T51Rng) -> String {
    (0..1 + rng.below(4))
        .map(|_| rng.pick(&T51_PARTS))
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn a_folded_folder_test_answers_as_the_test_that_folded_every_time() {
    let mut rng = T51Rng(0x5151_0000_0000_0001);
    let (mut inside, mut outside) = (0, 0);
    for _ in 0..6000 {
        let id = t51_path(&mut rng);
        // Mostly a folder the note may be in (some of its front, spelled another way), else
        // any path; sometimes with the separator at the end.
        let prefix = if rng.below(3) > 0 {
            let parts: Vec<&str> = id.split('/').collect();
            let take = rng.below(parts.len() + 1);
            let mut prefix: Vec<String> = parts[..take]
                .iter()
                .map(|p| {
                    if rng.below(3) == 0 {
                        p.to_uppercase()
                    } else {
                        p.to_string()
                    }
                })
                .collect();
            if rng.below(4) == 0 {
                prefix.push(rng.pick(&T51_PARTS).to_string());
            }
            prefix.join("/")
        } else {
            t51_path(&mut rng)
        };
        let prefix = if rng.below(5) == 0 {
            format!("{prefix}/")
        } else {
            prefix
        };
        let want = reference_is_inside(&id, &prefix);
        let got = is_inside_folded(
            &satz_core::fold_key(&id),
            &folded_prefix(Path::new(&prefix)),
        );
        assert_eq!(got, want, "{id:?} in {prefix:?}");
        if want {
            inside += 1;
        } else {
            outside += 1;
        }
    }
    assert!(
        inside > 800 && outside > 800,
        "{inside} inside, {outside} outside"
    );
    // The folder is the vault itself, or the path has the other separators.
    assert_eq!(
        reference_is_inside("a.md", ""),
        is_inside_folded("a.md", &folded_prefix(Path::new("")))
    );
    assert_eq!(
        folded_prefix(Path::new("Sub\\A")),
        folded_prefix(Path::new("Sub/A"))
    );
}

fn t51_state(rng: &mut T51Rng, root: &Path, open: usize, notes: usize) -> SatzState {
    let mut state = SatzState::default();
    state.set_vault_root(Some(root.to_path_buf()));
    state.set_indexing_complete(true);
    for i in 0..open {
        let rel = t51_path(rng);
        // The editor's spelling of the file: the vault's, or with the other separators.
        let path = if rng.below(2) == 0 {
            root.join(rel.replace('/', "\\"))
        } else {
            root.join(&rel)
        };
        state.open_document(&format!("file:///{i}"), "# Open\n", &path, 1);
    }
    for i in 0..notes {
        let rel = format!("{}.md", t51_path(rng));
        state.index.replace_doc(satz_core::parse_document(
            &format!("# T{i}\n"),
            Path::new(&rel),
        ));
    }
    state
}

#[test]
fn asking_the_set_of_open_paths_answers_as_going_through_the_open_documents_did() {
    let mut rng = T51Rng(0x5151_0000_0000_0002);
    let root = PathBuf::from(if cfg!(windows) { "C:\\vault" } else { "/vault" });
    let (mut open_hits, mut closed) = (0, 0);
    for round in 0..300 {
        let open = rng.below(6);
        let state = t51_state(&mut rng, &root, open, 12);
        let keys = state.open_path_keys();
        for doc in state.index.documents() {
            let want = reference_is_open(&state, &doc.id);
            let got = keys.contains(&satz_core::fold_key(doc.id.as_str()));
            assert_eq!(got, want, "round {round}: {:?}", doc.id);
            open_hits += usize::from(want);
            closed += usize::from(!want);
        }
        // A path as the watcher reports it, in either spelling and case.
        for _ in 0..8 {
            let rel = format!("{}.md", t51_path(&mut rng));
            let path = if rng.below(2) == 0 {
                root.join(rel.replace('/', "\\"))
            } else {
                root.join(&rel)
            };
            let want = state.open_docs.values().any(|open| {
                let key = |p: &Path| {
                    satz_core::fold_key(
                        &SatzState::get_rel_path(p, state.vault_root())
                            .to_string_lossy()
                            .replace('\\', "/"),
                    )
                };
                key(&open.path) == key(&path)
            });
            assert_eq!(
                keys.contains(&state.path_key(&path)),
                want,
                "round {round}: {path:?}"
            );
            assert_eq!(state.is_open_path(&path), want, "round {round}: {path:?}");
        }
    }
    assert!(
        open_hits > 20 && closed > 1000,
        "{open_hits} open, {closed} closed"
    );
}

#[test]
fn folders_are_asked_about_together_as_they_were_asked_about_one_by_one() {
    let mut rng = T51Rng(0x5151_0000_0000_0003);
    let root = PathBuf::from(if cfg!(windows) { "C:\\vault" } else { "/vault" });
    let (mut news, mut known) = (0, 0);
    for round in 0..300 {
        let notes = 1 + rng.below(20);
        let state = t51_state(&mut rng, &root, 0, notes);
        let folders: Vec<PathBuf> = (0..1 + rng.below(6))
            .map(|_| root.join(t51_path(&mut rng)))
            .collect();
        let refs: Vec<&Path> = folders.iter().map(PathBuf::as_path).collect();
        let together = folders_with_news(&state, &refs, &root);
        for (folder, got) in folders.iter().zip(together) {
            let want = reference_folder_event_is_news(&state, folder, &root);
            assert_eq!(got, want, "round {round}: {folder:?}");
            assert_eq!(folder_event_is_news(&state, folder, &root), want);
            news += usize::from(want);
            known += usize::from(!want);
        }
    }
    assert!(news > 200 && known > 200, "{news} news, {known} known");
}

// ---- a batch of prepared changes ----

fn t51_note(text: &str) -> PreparedChange {
    PreparedChange::Doc(Box::new(satz_core::parse_document(text, Path::new("a.md"))))
}

#[test]
fn a_batch_is_not_applied_before_the_first_indexing_is_done() {
    let dir = temp_dir("batch-deferred");
    let mut state = state_in(&dir);
    state.set_indexing_complete(false);
    let change = apply_prepared_batch(&mut state, vec![(dir.join("a.md"), t51_note("# A\n"))]);
    assert_eq!(change, FsChange::Deferred);
    assert!(ids(&state).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn of_several_events_for_one_note_in_a_batch_the_last_one_counts() {
    let dir = temp_dir("batch-last");
    let mut state = state_in(&dir);
    let (first, second) = (dir.join("a.md"), dir.join("A.md"));
    let gone = || PreparedChange::RemoveDoc(satz_core::DocId::new("a.md"));
    let title = |state: &SatzState| {
        state
            .index
            .get_doc(&satz_core::DocId::new("a.md"))
            .map(|d| d.title.clone())
    };

    let change = apply_prepared_batch(
        &mut state,
        vec![
            (first.clone(), t51_note("# First\n")),
            (second.clone(), t51_note("# Second\n")),
        ],
    );
    assert_eq!(change, FsChange::Reindexed);
    assert_eq!(title(&state).as_deref(), Some("Second"));

    // Removed and then written again: it stays. Written and then removed: it goes.
    apply_prepared_batch(
        &mut state,
        vec![
            (first.clone(), gone()),
            (second.clone(), t51_note("# Third\n")),
        ],
    );
    assert_eq!(title(&state).as_deref(), Some("Third"));
    let change = apply_prepared_batch(
        &mut state,
        vec![(second, t51_note("# Fourth\n")), (first, gone())],
    );
    assert_eq!(change, FsChange::Removed);
    assert_eq!(title(&state), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_scan_of_a_folder_in_a_folder_names_it_and_its_notes_with_slashes() {
    let dir = temp_dir("prepare-nested");
    let folder = dir.join("sub").join("deep");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("a.md"),
        "# A
",
    )
    .unwrap();
    let PreparedChange::Subtree { prefix, docs } =
        prepare_fs_change(&folder, &dir, satz_core::GitignoreMode::default())
    else {
        panic!("a folder that exists is scanned as a folder");
    };
    assert_eq!(prefix, "sub/deep");
    let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, vec!["sub/deep/a.md"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_scan_of_a_folder_does_not_replace_an_open_note_and_still_brings_the_others() {
    let dir = temp_dir("batch-subtree-open");
    let mut state = state_in(&dir);
    state.open_document(
        "file:///sub/a.md",
        "# Buffer
",
        &dir.join("sub").join("a.md"),
        1,
    );
    let scanned = |path: &str, text: &str| satz_core::parse_document(text, Path::new(path));
    let change = apply_prepared_batch(
        &mut state,
        vec![(
            dir.join("sub"),
            PreparedChange::Subtree {
                prefix: "sub".to_string(),
                docs: vec![
                    scanned(
                        "sub/a.md", "# Disk
",
                    ),
                    scanned(
                        "sub/b.md", "# B
",
                    ),
                ],
            },
        )],
    );
    assert_eq!(change, FsChange::Reindexed);
    assert_eq!(ids(&state), vec!["sub/a.md", "sub/b.md"]);
    assert_eq!(
        state
            .index
            .get_doc(&satz_core::DocId::new("sub/a.md"))
            .map(|d| d.title.as_str()),
        Some("Buffer")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
