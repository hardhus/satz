use std::path::Path;

#[test]
fn test_satz_index_command() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("satz-core/tests/fixtures");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["index", fixtures.to_str().unwrap()])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("documents indexed"));
    assert!(stdout.contains("Indexing vault"));
}

#[test]
fn test_satz_stats_json_command() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("satz-core/tests/fixtures");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["stats", "--vault", fixtures.to_str().unwrap(), "--json"])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output should be valid JSON");
    assert!(json["doc_count"].as_u64().unwrap() >= 4);
    assert!(json["unique_tags"].as_u64().unwrap() > 0);
}

#[test]
fn test_satz_list_tag_command() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("satz-core/tests/fixtures");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args([
            "list",
            "--vault",
            fixtures.to_str().unwrap(),
            "--tag",
            "felsefe",
        ])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("book_note.md"));
}

#[test]
fn test_satz_resolve_command() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("satz-core/tests/fixtures");

    // Existing note resolution via alias
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["resolve", "--vault", fixtures.to_str().unwrap(), "[[TLP]]"])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("book_note.md"));

    // Non-existent note resolution should fail with status code 1
    let fail_output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args([
            "resolve",
            "--vault",
            fixtures.to_str().unwrap(),
            "[[nonexistent-note-xyz]]",
        ])
        .output()
        .expect("satz binary should execute");

    assert!(!fail_output.status.success());
    let stderr = String::from_utf8_lossy(&fail_output.stderr);
    assert!(stderr.contains("not found"));
}

#[test]
fn test_satz_daily_command() {
    let temp_dir = std::env::temp_dir().join(format!("satz_daily_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["daily", temp_dir.to_str().unwrap()])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = std::path::PathBuf::from(stdout.trim());
    assert!(path.exists());
    assert!(path.to_string_lossy().ends_with(".md"));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("---"));

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_satz_list_broken_command() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("satz-core/tests/fixtures");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["list", "--vault", fixtures.to_str().unwrap(), "--broken"])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Fixtures contain broken links to non-existent notes
    assert!(
        stdout.contains("— file not found") || stdout.contains("— file exists, heading not found")
    );
}

#[test]
fn test_satz_graph_command() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("satz-core/tests/fixtures");

    // 1. JSON format
    let output_json = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args([
            "graph",
            "--vault",
            fixtures.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("satz binary should execute");

    assert!(output_json.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output_json.stdout).expect("output should be valid JSON");
    assert!(json["nodes"].as_array().unwrap().len() >= 4);
    assert!(json["edges"].as_array().is_some());

    // 2. DOT format
    let output_dot = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args([
            "graph",
            "--vault",
            fixtures.to_str().unwrap(),
            "--format",
            "dot",
        ])
        .output()
        .expect("satz binary should execute");

    assert!(output_dot.status.success());
    let dot = String::from_utf8_lossy(&output_dot.stdout);
    assert!(dot.starts_with("digraph \"satz\" {"));
    assert!(dot.contains("->"));
}

#[test]
fn test_satz_fmt_check_exits_1_on_dirty_file() {
    let temp_dir = std::env::temp_dir().join(format!("satz_fmt_check_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    std::fs::write(
        temp_dir.join("note.md"),
        "# Title\n\nContent with   trailing spaces \t \nand _underscore italic_.\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["fmt", temp_dir.to_str().unwrap(), "--check"])
        .output()
        .expect("satz binary should execute");

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("note.md"));

    // --check must never write anything.
    let content = std::fs::read_to_string(temp_dir.join("note.md")).unwrap();
    assert!(content.contains("_underscore italic_"));

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_satz_fmt_write_actually_rewrites_dirty_file() {
    let temp_dir = std::env::temp_dir().join(format!("satz_fmt_write_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let file_path = temp_dir.join("note.md");
    std::fs::write(
        &file_path,
        "# Title\n\nContent with   trailing spaces \t \nand _underscore italic_.\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["fmt", temp_dir.to_str().unwrap(), "--write"])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());

    let content = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(
        content,
        "# Title\n\nContent with   trailing spaces\nand *underscore italic*.\n"
    );

    // Running --check again on the now-clean file must succeed.
    let recheck = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["fmt", temp_dir.to_str().unwrap(), "--check"])
        .output()
        .expect("satz binary should execute");
    assert!(recheck.status.success());

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_satz_fmt_write_skips_io_for_already_clean_file() {
    let temp_dir = std::env::temp_dir().join(format!("satz_fmt_clean_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let file_path = temp_dir.join("note.md");
    std::fs::write(&file_path, "# Title\n\nAlready clean content.\n").unwrap();

    let mtime_before = std::fs::metadata(&file_path).unwrap().modified().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["fmt", temp_dir.to_str().unwrap(), "--write"])
        .output()
        .expect("satz binary should execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0 file(s) formatted"));
    assert!(stdout.contains("1 file(s) already clean"));

    let mtime_after = std::fs::metadata(&file_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "already-clean file must not be rewritten (mtime changed)"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// ---------------------------------------------------------------------------------------------
// Config errors, path validation and write failures (`fmt` / `daily`).
//
// These run the real binary and compare file CONTENTS byte for byte: a command that fails must
// not have changed anything, and a command that succeeds must have really applied the config.
// ---------------------------------------------------------------------------------------------

mod support {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::process::Output;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A unique temp directory that is removed (read-only files included) when dropped.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "satz_cli_{tag}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
        pub fn str(&self) -> &str {
            self.0.to_str().unwrap()
        }
        pub fn write(&self, rel: &str, content: &str) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
            p
        }
        pub fn read(&self, rel: &str) -> Vec<u8> {
            std::fs::read(self.0.join(rel)).unwrap()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fn unlock(dir: &Path) {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    return;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        unlock(&p);
                    } else if let Ok(meta) = std::fs::metadata(&p) {
                        let mut perms = meta.permissions();
                        // Clearing the read-only flag is the point: it lets the test clean up its files.
                        #[allow(clippy::permissions_set_readonly_false)]
                        perms.set_readonly(false);
                        let _ = std::fs::set_permissions(&p, perms);
                    }
                }
            }
            unlock(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Every file (relative path -> bytes) and directory (relative path + "/" -> empty) below
    /// `dir`, so "nothing was created or changed" can be asserted with one comparison.
    pub fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if p.is_dir() {
                    out.insert(format!("{rel}/"), Vec::new());
                    walk(root, &p, out);
                } else {
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    pub fn satz(args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
            .args(args)
            .output()
            .expect("satz binary should execute")
    }

    pub fn out(o: &Output) -> String {
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    pub fn err(o: &Output) -> String {
        String::from_utf8_lossy(&o.stderr).into_owned()
    }

    pub const DIRTY: &str =
        "# Title\n\nContent with   trailing spaces \t \nand _underscore italic_.\n";
    pub const DIRTY_FORMATTED: &str =
        "# Title\n\nContent with   trailing spaces\nand *underscore italic*.\n";
}

use support::{DIRTY, DIRTY_FORMATTED, TempDir, err, out, satz, snapshot};

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// ---- fmt: configuration errors must be loud and must not touch any file ----

#[test]
fn fmt_invalid_toml_fails_and_leaves_files_untouched() {
    let v = TempDir::new("fmt_badtoml");
    v.write(".satz.toml", "[formatter\nline_width = 1\n");
    v.write("note.md", DIRTY);
    let before = snapshot(v.path());

    let o = satz(&["fmt", v.str()]);

    assert_eq!(o.status.code(), Some(1), "stderr: {}", err(&o));
    let e = err(&o);
    assert!(e.contains(".satz.toml"), "should name the config file: {e}");
    assert!(e.contains("line 1"), "should say where the error is: {e}");
    assert_eq!(snapshot(v.path()), before, "no file may change");
}

#[test]
fn fmt_unknown_config_key_warns_and_still_formats_with_the_rest_of_the_config() {
    let v = TempDir::new("fmt_unknownkey");
    // A typo (`enabled` instead of `enable`): named in a warning, the rest of the file applies.
    v.write(
        ".satz.toml",
        "[formatter.wrap]\nenabled = true\n[formatter.lists]\nmarker = \"*\"\n",
    );
    v.write("note.md", "- one\n- two\n");
    let before = snapshot(v.path());

    let o = satz(&["fmt", v.str()]);

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let e = err(&o);
    assert!(e.contains("warning"), "{e}");
    assert!(
        e.contains("formatter.wrap.enabled"),
        "should name the unknown key: {e}"
    );
    assert_ne!(snapshot(v.path()), before, "the file was formatted");
    assert_eq!(
        v.read("note.md"),
        b"* one\n* two\n",
        "the valid marker setting applied"
    );
}

#[test]
fn fmt_check_reports_only_formatting_differences_even_with_config_warnings() {
    let v = TempDir::new("fmt_check_warn");
    v.write(".satz.toml", "[nonsense]\nx = 1\n");
    v.write("clean.md", "# Clean\n\nText.\n");
    let o = satz(&["fmt", "--check", v.str()]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("nonsense"), "{}", err(&o));
    v.write("dirty.md", DIRTY);
    let o = satz(&["fmt", "--check", v.str()]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
}

#[test]
fn a_config_without_mistakes_prints_no_warning() {
    let v = TempDir::new("fmt_no_warning");
    v.write(".satz.toml", "[formatter]\nline_width = 100\n");
    v.write("note.md", "# T\n\nText.\n");
    let o = satz(&["fmt", v.str()]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(!err(&o).contains("warning"), "{}", err(&o));
}

#[test]
fn fmt_wrong_value_type_fails_and_leaves_files_untouched() {
    let v = TempDir::new("fmt_wrongtype");
    v.write(".satz.toml", "[formatter]\nline_width = \"wide\"\n");
    v.write("note.md", DIRTY);
    let before = snapshot(v.path());

    let o = satz(&["fmt", v.str()]);

    assert_eq!(o.status.code(), Some(1));
    assert_eq!(snapshot(v.path()), before);
}

#[test]
fn fmt_check_with_invalid_config_never_claims_everything_is_formatted() {
    let v = TempDir::new("fmt_check_bad");
    v.write(".satz.toml", "not = [valid\n");
    v.write("note.md", DIRTY_FORMATTED); // already clean under the default config
    let before = snapshot(v.path());

    let o = satz(&["fmt", v.str(), "--check"]);

    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains(".satz.toml"));
    assert!(
        !out(&o).contains("already formatted"),
        "a broken config must not be reported as a clean check: {}",
        out(&o)
    );
    assert_eq!(snapshot(v.path()), before);
}

#[test]
fn fmt_applies_a_valid_config() {
    let with_cfg = TempDir::new("fmt_cfg");
    with_cfg.write(".satz.toml", "[formatter.lists]\nmarker = \"*\"\n");
    with_cfg.write("note.md", "# T\n\n- a\n- b\n");
    let without_cfg = TempDir::new("fmt_nocfg");
    without_cfg.write("note.md", "# T\n\n- a\n- b\n");

    assert!(satz(&["fmt", with_cfg.str()]).status.success());
    assert!(satz(&["fmt", without_cfg.str()]).status.success());

    // The config really changed the result (a config that is read but ignored would leave "-").
    assert_eq!(with_cfg.read("note.md"), b"# T\n\n* a\n* b\n");
    assert_eq!(without_cfg.read("note.md"), b"# T\n\n- a\n- b\n");
}

#[test]
fn fmt_ignores_config_files_in_subfolders() {
    let v = TempDir::new("fmt_subcfg");
    v.write("sub/.satz.toml", "[formatter.lists]\nmarker = \"*\"\n");
    v.write("note.md", "# T\n\n- a\n- b\n");

    assert!(satz(&["fmt", v.str()]).status.success());

    assert_eq!(v.read("note.md"), b"# T\n\n- a\n- b\n");
}

#[test]
fn fmt_disabled_config_is_a_successful_noop() {
    let v = TempDir::new("fmt_disabled");
    v.write(".satz.toml", "[formatter]\nenabled = false\n");
    v.write("note.md", DIRTY);
    let before = snapshot(v.path());

    let o = satz(&["fmt", v.str()]);

    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("disabled"));
    assert_eq!(snapshot(v.path()), before);
}

// ---- fmt / daily: the path must be an existing directory ----

#[test]
fn fmt_path_that_is_a_file_errors_and_leaves_it_untouched() {
    let v = TempDir::new("fmt_isfile");
    // A config next to the file must NOT be used to format the file behind the user's back.
    v.write(".satz.toml", "[formatter.lists]\nmarker = \"*\"\n");
    let file = v.write("note.md", "# T\n\n- a\n");
    let before = snapshot(v.path());

    let o = satz(&["fmt", file.to_str().unwrap()]);

    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("directory"), "{}", err(&o));
    assert_eq!(snapshot(v.path()), before);
}

#[test]
fn fmt_missing_path_errors_and_creates_nothing() {
    let v = TempDir::new("fmt_missing");
    let missing = v.path().join("does-not-exist");

    let o = satz(&["fmt", missing.to_str().unwrap()]);

    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("does not exist"), "{}", err(&o));
    assert!(!missing.exists(), "the command must not create the path");
}

// ---- fmt: exit codes and write failures ----

#[test]
fn fmt_check_exit_codes_and_no_writes() {
    let clean = TempDir::new("fmt_check_clean");
    clean.write("a.md", DIRTY_FORMATTED);
    let o = satz(&["fmt", clean.str(), "--check"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("already formatted"));

    let dirty = TempDir::new("fmt_check_dirty");
    dirty.write("a.md", DIRTY);
    dirty.write("sub/b.md", DIRTY_FORMATTED);
    let before = snapshot(dirty.path());
    let o = satz(&["fmt", dirty.str(), "--check"]);
    assert_eq!(o.status.code(), Some(1));
    let stdout = out(&o);
    assert!(
        stdout.contains("a.md"),
        "dirty file should be listed: {stdout}"
    );
    assert!(
        !stdout.contains("b.md"),
        "clean file must not be listed: {stdout}"
    );
    assert_eq!(snapshot(dirty.path()), before, "--check must never write");
}

#[test]
fn fmt_write_failure_exits_nonzero_reports_the_path_and_keeps_going() {
    let v = TempDir::new("fmt_readonly");
    v.write("a.md", DIRTY);
    let locked = v.write("b.md", DIRTY);
    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&locked, perms).unwrap();

    let o = satz(&["fmt", v.str()]);

    assert_eq!(o.status.code(), Some(1), "stdout: {}", out(&o));
    let e = err(&o);
    assert!(e.contains("b.md"), "should name the file that failed: {e}");
    assert!(e.contains("1 file(s) could not be written"), "{e}");
    // The count only includes files that were really written.
    assert!(out(&o).contains("1 file(s) formatted"), "{}", out(&o));
    // The writable file was still formatted; the locked one is byte-for-byte unchanged.
    assert_eq!(v.read("a.md"), DIRTY_FORMATTED.as_bytes());
    assert_eq!(v.read("b.md"), DIRTY.as_bytes());
}

// ---- daily ----

#[test]
fn daily_creates_todays_note_by_default_and_with_explicit_true() {
    for extra in [&[][..], &["--create", "true"][..], &["-c", "true"][..]] {
        let v = TempDir::new("daily_create");
        let mut args = vec!["daily", v.str()];
        args.extend_from_slice(extra);

        let o = satz(&args);

        assert!(o.status.success(), "{extra:?}: {}", err(&o));
        let note = v.path().join("daily").join(format!("{}.md", today()));
        assert!(
            note.exists(),
            "{extra:?}: note should be created at {note:?}"
        );
        assert!(out(&o).trim().ends_with(&format!("{}.md", today())));
    }
}

#[test]
fn daily_create_false_prints_the_path_and_creates_nothing() {
    for flag in [&["--create", "false"][..], &["-c", "false"][..]] {
        let v = TempDir::new("daily_nocreate");
        let before = snapshot(v.path());
        let mut args = vec!["daily", v.str()];
        args.extend_from_slice(flag);

        let o = satz(&args);

        assert!(o.status.success(), "{flag:?}: {}", err(&o));
        assert!(
            out(&o).trim().ends_with(&format!("{}.md", today())),
            "{flag:?}: {}",
            out(&o)
        );
        assert_eq!(
            snapshot(v.path()),
            before,
            "{flag:?}: nothing may be created"
        );
    }
}

#[test]
fn daily_create_as_a_bare_flag_means_true_as_it_always_did() {
    for flag in ["--create", "-c"] {
        let v = TempDir::new("daily_bareflag");

        let o = satz(&["daily", v.str(), flag]);

        assert!(o.status.success(), "{flag}: {}", err(&o));
        let printed = std::path::PathBuf::from(out(&o).trim());
        assert!(
            printed.exists(),
            "{flag}: the note was created at {printed:?}"
        );
    }
    // A value still works next to it, and the bare flag can come last or first.
    let v = TempDir::new("daily_bareflag_false");
    let o = satz(&["daily", v.str(), "--create", "false"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(!std::path::PathBuf::from(out(&o).trim()).exists());
}

#[test]
fn daily_create_with_a_non_boolean_value_is_a_usage_error() {
    let v = TempDir::new("daily_badvalue");
    let before = snapshot(v.path());

    let o = satz(&["daily", v.str(), "--create", "maybe"]);

    assert_eq!(o.status.code(), Some(2));
    assert_eq!(snapshot(v.path()), before);
}

#[test]
fn daily_never_overwrites_an_existing_note() {
    let v = TempDir::new("daily_existing");
    let rel = format!("daily/{}.md", today());
    v.write(&rel, "MY OWN CONTENT, keep it\n");

    let o = satz(&["daily", v.str()]);

    assert!(o.status.success());
    assert_eq!(v.read(&rel), b"MY OWN CONTENT, keep it\n");
}

#[test]
fn daily_invalid_config_errors_and_creates_nothing() {
    let v = TempDir::new("daily_badcfg");
    v.write(".satz.toml", "[daily_note]\nfolder = \n");
    let before = snapshot(v.path());

    let o = satz(&["daily", v.str()]);

    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains(".satz.toml"), "{}", err(&o));
    assert_eq!(snapshot(v.path()), before);
}

#[test]
fn daily_unknown_config_key_warns_and_uses_the_rest_of_the_config() {
    let v = TempDir::new("daily_unknownkey");
    v.write(
        ".satz.toml",
        "[daily_note]\nfolders = \"journal\"\nfolder = \"mine\"\n",
    );

    let o = satz(&["daily", v.str()]);

    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("daily_note.folders"), "{}", err(&o));
    let printed = std::path::PathBuf::from(out(&o).trim());
    assert!(
        printed.to_string_lossy().contains("mine"),
        "the valid folder applied: {printed:?}"
    );
    assert!(printed.exists());
}

#[test]
fn daily_invalid_date_format_warns_and_uses_the_default_format_never_panics() {
    for bad in ["%Q", "%", "%Y%"] {
        let v = TempDir::new("daily_badfmt");
        v.write(".satz.toml", &format!("[daily_note]\nformat = \"{bad}\"\n"));

        let o = satz(&["daily", v.str()]);

        // A panic would exit with 101.
        assert_eq!(o.status.code(), Some(0), "{bad:?}: {}", err(&o));
        assert!(!err(&o).contains("panicked"), "{bad:?}: {}", err(&o));
        assert!(
            err(&o).contains("daily_note.format"),
            "{bad:?}: {}",
            err(&o)
        );
        let printed = std::path::PathBuf::from(out(&o).trim());
        let name = printed.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.len() == "2026-01-01.md".len() && name.ends_with(".md"),
            "the default %Y-%m-%d: {name}"
        );
    }
}

#[test]
fn daily_custom_folder_and_format_are_applied() {
    let v = TempDir::new("daily_custom");
    v.write(
        ".satz.toml",
        "[daily_note]\nfolder = \"journal\"\nformat = \"%Y/%m/%d\"\n",
    );

    let o = satz(&["daily", v.str()]);

    assert!(o.status.success(), "{}", err(&o));
    let printed = std::path::PathBuf::from(out(&o).trim());
    let parts: Vec<String> = printed
        .components()
        .rev()
        .take(4)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    // .../journal/<yyyy>/<mm>/<dd>.md (components collected in reverse)
    assert!(
        parts[0].ends_with(".md") && parts[0].len() == 5,
        "{parts:?}"
    );
    assert_eq!(parts[1].len(), 2, "{parts:?}");
    assert_eq!(parts[2].len(), 4, "{parts:?}");
    assert_eq!(parts[3], "journal", "{parts:?}");
    assert!(printed.exists());
}

#[test]
fn daily_path_that_is_a_file_or_missing_errors_and_creates_nothing() {
    let v = TempDir::new("daily_badpath");
    let file = v.write("note.md", "# T\n");
    let missing = v.path().join("nope");
    let before = snapshot(v.path());

    let o = satz(&["daily", file.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("directory"), "{}", err(&o));

    let o = satz(&["daily", missing.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("does not exist"), "{}", err(&o));

    assert!(!missing.exists(), "must not create the missing vault");
    assert_eq!(snapshot(v.path()), before);
}

#[test]
fn daily_output_path_has_no_windows_verbatim_prefix() {
    let v = TempDir::new("daily_prefix");

    let o = satz(&["daily", v.str(), "--create", "false"]);

    assert!(o.status.success());
    let printed = out(&o);
    assert!(
        !printed.trim_start().starts_with(r"\\?\"),
        "printed path should be usable as-is: {printed}"
    );
}

#[test]
fn daily_settings_cannot_escape_the_vault() {
    for (setting, value) in [
        ("folder", "../outside"),
        ("folder", "a/../../outside"),
        ("format", "../outside/%Y-%m-%d"),
        // Backslash separators (written as a TOML `\\` escape).
        ("format", "..\\\\outside\\\\%Y-%m-%d"),
    ] {
        let parent = TempDir::new("daily_escape");
        let vault = parent.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join(".satz.toml"),
            format!("[daily_note]\n{setting} = \"{value}\"\n"),
        )
        .unwrap();
        let before = snapshot(parent.path());

        let o = satz(&["daily", vault.to_str().unwrap()]);

        assert_eq!(o.status.code(), Some(1), "{setting}={value}: {}", err(&o));
        assert!(
            err(&o).contains("inside the vault"),
            "{setting}={value}: {}",
            err(&o)
        );
        // Nothing was written next to, above or inside the vault.
        assert_eq!(snapshot(parent.path()), before, "{setting}={value}");
        assert!(!parent.path().join("outside").exists());
    }
}

#[test]
fn daily_absolute_looking_folder_is_kept_inside_the_vault() {
    let v = TempDir::new("daily_abs");
    v.write(".satz.toml", "[daily_note]\nfolder = \"/journal\"\n");

    let o = satz(&["daily", v.str()]);

    assert!(o.status.success(), "{}", err(&o));
    assert!(
        v.path()
            .join("journal")
            .join(format!("{}.md", today()))
            .exists()
    );
}

#[test]
fn fmt_write_keeps_the_byte_order_mark() {
    let temp_dir = std::env::temp_dir().join(format!("satz_fmt_bom_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let file_path = temp_dir.join("note.md");
    let mut before = vec![0xEF, 0xBB, 0xBF];
    before.extend_from_slice(b"---\ntitle: T\n---\n\n\n# Title  \n\ntext  \n");
    std::fs::write(&file_path, &before).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(["fmt", temp_dir.to_str().unwrap(), "--write"])
        .output()
        .expect("satz binary should execute");
    assert!(output.status.success());

    let after = std::fs::read(&file_path).unwrap();
    let mut expected = vec![0xEF, 0xBB, 0xBF];
    expected.extend_from_slice(b"---\ntitle: T\n---\n\n# Title\n\ntext\n");
    assert_eq!(after, expected);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

// ---- `satz list`: filters combine with `--broken`, reasons are English ----

fn sorted_lines(output: &std::process::Output) -> Vec<String> {
    let mut lines: Vec<String> = out(output).lines().map(str::to_string).collect();
    lines.sort();
    lines
}

fn broken_vault(tag: &str) -> TempDir {
    let v = TempDir::new(tag);
    v.write("a.md", "# A\n\n#x [[missing]] [[b#Nope]]\n");
    v.write("b.md", "# B\n\n[[missing2]]\n");
    v.write("c.md", "# C\n\n#x #y [[a]]\n");
    v
}

#[test]
fn list_broken_names_each_problem_in_english() {
    let v = broken_vault("broken_en");
    let o = satz(&["list", "--vault", v.str(), "--broken"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        sorted_lines(&o),
        vec![
            "a.md:3\t[[b#Nope]]\t— file exists, heading not found".to_string(),
            "a.md:3\t[[missing]]\t— file not found".to_string(),
            "b.md:3\t[[missing2]]\t— file not found".to_string(),
        ]
    );
}

#[test]
fn list_broken_honours_tag_and_orphan_filters() {
    let v = broken_vault("broken_filters");

    // Only notes tagged x: a.md (c.md has no broken links).
    let tagged = sorted_lines(&satz(&["list", "-v", v.str(), "--broken", "--tag", "x"]));
    assert_eq!(tagged.len(), 2, "{tagged:?}");
    assert!(tagged.iter().all(|l| l.starts_with("a.md:")), "{tagged:?}");

    // Intersection of tags: only c.md has both x and y, and it has nothing broken.
    let both = satz(&[
        "list",
        "-v",
        v.str(),
        "--broken",
        "--tag",
        "x",
        "--tag",
        "y",
    ]);
    assert!(both.status.success());
    assert!(sorted_lines(&both).is_empty());

    // Orphans: only c.md (nothing links to it) -- again nothing broken there.
    let orphans = satz(&["list", "-v", v.str(), "--broken", "--orphans"]);
    assert!(orphans.status.success());
    assert!(sorted_lines(&orphans).is_empty());

    // A tag nobody has.
    let none = satz(&["list", "-v", v.str(), "--broken", "--tag", "nope"]);
    assert!(none.status.success());
    assert!(sorted_lines(&none).is_empty());
}

// ---- every vault command reports a bad vault path the same way ----

#[test]
fn every_vault_command_rejects_a_missing_or_non_directory_vault_alike() {
    let dir = TempDir::new("badvault");
    dir.write("plain.md", "# x\n");
    let missing = dir.path().join("does-not-exist");
    let missing = missing.to_str().unwrap();
    let file = dir.path().join("plain.md");
    let file = file.to_str().unwrap();

    let commands: Vec<Vec<&str>> = vec![
        vec!["index", "PATH"],
        vec!["stats", "-v", "PATH"],
        vec!["list", "-v", "PATH"],
        vec!["resolve", "-v", "PATH", "x"],
        vec!["graph", "-v", "PATH"],
        vec!["fmt", "PATH", "--check"],
    ];
    for template in &commands {
        for (bad, expected) in [
            (missing, "vault path does not exist"),
            (file, "is not a directory"),
        ] {
            let args: Vec<&str> = template
                .iter()
                .map(|a| if *a == "PATH" { bad } else { *a })
                .collect();
            let o = satz(&args);
            assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
            assert!(err(&o).contains(expected), "{args:?}: {}", err(&o));
        }
    }
}

// ---- edge cases of the query commands ----

fn small_vault(tag: &str) -> TempDir {
    let v = TempDir::new(tag);
    v.write("a.md", "# A\n\n#x #y [[b]]\n");
    v.write("b.md", "# B\n\n#x text\n");
    v.write("c.md", "# C\n\nlonely note\n");
    v
}

#[test]
fn list_tag_filters_intersect_and_unknown_tags_give_nothing() {
    let v = small_vault("list_tags");
    assert_eq!(
        sorted_lines(&satz(&["list", "-v", v.str(), "--tag", "x"])),
        vec!["a.md", "b.md"]
    );
    assert_eq!(
        sorted_lines(&satz(&["list", "-v", v.str(), "--tag", "x", "--tag", "y"])),
        vec!["a.md"]
    );
    let none = satz(&["list", "-v", v.str(), "--tag", "nope"]);
    assert!(none.status.success());
    assert!(sorted_lines(&none).is_empty());
}

#[test]
fn list_orphans_shows_notes_nobody_links_to() {
    let v = small_vault("list_orphans");
    // a.md links to b.md; nothing links to a.md or c.md.
    assert_eq!(
        sorted_lines(&satz(&["list", "-v", v.str(), "--orphans"])),
        vec!["a.md", "c.md"]
    );
    assert_eq!(
        sorted_lines(&satz(&["list", "-v", v.str(), "--orphans", "--tag", "x"])),
        vec!["a.md"]
    );
}

#[test]
fn graph_writes_dot_to_a_file_and_json_to_stdout() {
    let v = small_vault("graph_out");
    let target = v.path().join("graph.dot");
    let o = satz(&[
        "graph",
        "-v",
        v.str(),
        "-f",
        "dot",
        "-o",
        target.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).is_empty(),
        "nothing on stdout when writing to a file"
    );
    assert!(err(&o).contains("3 nodes"), "{}", err(&o));
    let dot = std::fs::read_to_string(&target).unwrap();
    assert!(dot.starts_with("digraph"), "{dot}");
    assert!(dot.contains("->"), "{dot}");

    let json = satz(&["graph", "-v", v.str(), "-f", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&out(&json)).unwrap();
    assert_eq!(parsed["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["edges"].as_array().unwrap().len(), 1);
}

#[test]
fn graph_rejects_an_unknown_format() {
    let v = small_vault("graph_bad_format");
    let o = satz(&["graph", "-v", v.str(), "-f", "svg"]);
    assert_eq!(o.status.code(), Some(2), "clap usage errors exit with 2");
    assert!(err(&o).contains("svg"), "{}", err(&o));
}

#[test]
fn stats_text_output_lists_the_counts() {
    let v = small_vault("stats_text");
    let o = satz(&["stats", "-v", v.str()]);
    assert!(o.status.success());
    let text = out(&o);
    for line in [
        "  Documents:    3",
        "  Total links:  1",
        "  Broken links: 0",
        "  Unique tags:  2",
        "  Orphan docs:  2",
    ] {
        assert!(text.contains(line), "missing {line:?} in:\n{text}");
    }
}

#[test]
fn resolve_finds_a_note_and_fails_cleanly_for_a_missing_one() {
    let v = small_vault("resolve_edge");
    let found = satz(&["resolve", "-v", v.str(), "[[b]]"]);
    assert!(found.status.success());
    assert!(out(&found).trim().ends_with("b.md"), "{}", out(&found));
    let bare = satz(&["resolve", "-v", v.str(), "b"]);
    assert_eq!(out(&bare), out(&found));
    let missing = satz(&["resolve", "-v", v.str(), "[[nothing-here]]"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(err(&missing).contains("not found"), "{}", err(&missing));
}

#[test]
fn fmt_follows_the_vault_configuration() {
    let v = TempDir::new("fmt_config_driven");
    v.write(".satz.toml", "[formatter.lists]\nmarker = \"*\"\n");
    v.write("n.md", "- a\n- b\n");
    let o = satz(&["fmt", v.str()]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        std::fs::read_to_string(v.path().join("n.md")).unwrap(),
        "* a\n* b\n"
    );
    // And with the setting turned off nothing is rewritten.
    v.write(".satz.toml", "[formatter]\nenabled = false\n");
    v.write("n.md", "- a\n-   b  \n");
    let before = snapshot(v.path());
    let off = satz(&["fmt", v.str()]);
    assert!(off.status.success());
    assert_eq!(snapshot(v.path()), before);
}

// ---- a reader that goes away (`satz list | head`) ----

/// Runs `satz` with its stdout piped and the reading end closed at once: whatever the command
/// writes fails with a broken pipe (it indexes the vault first, which takes longer than closing).
fn run_with_a_closed_stdout(args: &[&str]) -> std::process::Output {
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_satz"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("satz binary should execute");
    drop(child.stdout.take());
    child.wait_with_output().expect("satz finishes")
}

#[test]
fn commands_end_quietly_when_the_reader_of_stdout_has_gone_away() {
    // (label, args with VAULT, exit code the command has on its own, vault with a note to format)
    let cases: &[(&str, &[&str], i32, bool)] = &[
        ("list", &["list", "-v", "VAULT"], 0, false),
        (
            "list --orphans",
            &["list", "-v", "VAULT", "--orphans"],
            0,
            false,
        ),
        (
            "list --tag",
            &["list", "-v", "VAULT", "--tag", "x"],
            0,
            false,
        ),
        (
            "list --broken",
            &["list", "-v", "VAULT", "--broken"],
            0,
            false,
        ),
        ("stats", &["stats", "-v", "VAULT"], 0, false),
        (
            "stats --json",
            &["stats", "-v", "VAULT", "--json"],
            0,
            false,
        ),
        ("index", &["index", "VAULT"], 0, false),
        ("graph json", &["graph", "-v", "VAULT"], 0, false),
        (
            "graph dot",
            &["graph", "-v", "VAULT", "-f", "dot"],
            0,
            false,
        ),
        ("resolve", &["resolve", "-v", "VAULT", "b"], 0, false),
        ("daily", &["daily", "VAULT"], 0, false),
        // The exit code is the command's own result, not the pipe's: a script that checks
        // `fmt --check | head` with pipefail must still see "files need formatting".
        (
            "fmt --check, one file to format",
            &["fmt", "VAULT", "--check"],
            1,
            true,
        ),
        (
            "fmt --check, all clean",
            &["fmt", "VAULT", "--check"],
            0,
            false,
        ),
        ("fmt", &["fmt", "VAULT"], 0, true),
    ];
    for (label, template, expected_code, with_dirty_note) in cases {
        let v = small_vault("closed_pipe");
        v.write("broken.md", "# Broken\n\n[[nothing-here]]\n");
        if *with_dirty_note {
            v.write("dirty.md", DIRTY);
        }
        let args: Vec<&str> = template
            .iter()
            .map(|a| if *a == "VAULT" { v.str() } else { *a })
            .collect();

        let o = run_with_a_closed_stdout(&args);

        assert_eq!(
            o.status.code(),
            Some(*expected_code),
            "{label}: stderr: {}",
            err(&o)
        );
        assert!(
            !err(&o).contains("panicked") && !err(&o).contains("failed printing"),
            "{label}: stderr: {}",
            err(&o)
        );
        // The work is done, only the report had no reader.
        match *label {
            "daily" => assert!(
                v.path()
                    .join("daily")
                    .join(format!("{}.md", today()))
                    .exists(),
                "the daily note was still created"
            ),
            "fmt" => assert_eq!(
                v.read("dirty.md"),
                DIRTY_FORMATTED.as_bytes(),
                "the note was still formatted"
            ),
            _ => {}
        }
    }
}

// ---- the commands as a library: the output goes to the writer that is given ----

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("the output is UTF-8")
}

#[test]
fn list_writes_to_the_given_writer() {
    use satz_cli::commands::list_cmd::{ListArgs, run_with_output};
    let v = small_vault("lib_list");
    v.write("broken.md", "# Broken\n\nsee [[nothing-here]]\n");
    let args = |tag: &[&str], orphans, broken| ListArgs {
        vault: v.path().to_path_buf(),
        tag: tag.iter().map(|t| t.to_string()).collect(),
        orphans,
        broken,
    };

    let mut plain = Vec::new();
    run_with_output(args(&[], false, false), &mut plain).unwrap();
    assert_eq!(text(plain), "a.md\nb.md\nbroken.md\nc.md\n");

    let mut tagged = Vec::new();
    run_with_output(args(&["x"], false, false), &mut tagged).unwrap();
    assert_eq!(text(tagged), "a.md\nb.md\n");

    let mut broken = Vec::new();
    run_with_output(args(&[], false, true), &mut broken).unwrap();
    assert_eq!(
        text(broken),
        "broken.md:3\t[[nothing-here]]\t— file not found\n"
    );
}

#[test]
fn stats_index_graph_and_resolve_write_to_the_given_writer() {
    use satz_cli::commands::graph_cmd::{GraphArgs, GraphFormat};
    use satz_cli::commands::index_cmd::IndexArgs;
    use satz_cli::commands::resolve_cmd::ResolveArgs;
    use satz_cli::commands::stats_cmd::StatsArgs;
    let v = small_vault("lib_others");

    let mut stats = Vec::new();
    satz_cli::commands::stats_cmd::run_with_output(
        StatsArgs {
            vault: v.path().to_path_buf(),
            json: true,
        },
        &mut stats,
    )
    .unwrap();
    let stats: serde_json::Value = serde_json::from_str(&text(stats)).expect("JSON");
    assert_eq!(stats["doc_count"], 3);

    let mut index = Vec::new();
    satz_cli::commands::index_cmd::run_with_output(
        IndexArgs {
            path: v.path().to_path_buf(),
        },
        &mut index,
    )
    .unwrap();
    let index = text(index);
    assert!(index.starts_with("Indexing vault: "), "{index}");
    assert!(index.contains("3 documents indexed"), "{index}");

    for (format, marker) in [
        (GraphFormat::Json, "\"nodes\""),
        (GraphFormat::Dot, "digraph"),
    ] {
        let mut graph = Vec::new();
        satz_cli::commands::graph_cmd::run_with_output(
            GraphArgs {
                vault: v.path().to_path_buf(),
                format,
                output: None,
            },
            &mut graph,
        )
        .unwrap();
        let graph = text(graph);
        assert!(graph.contains(marker), "{format:?}: {graph}");
        assert!(graph.ends_with('\n'));
    }

    let mut resolved = Vec::new();
    satz_cli::commands::resolve_cmd::run_with_output(
        ResolveArgs {
            vault: v.path().to_path_buf(),
            target: "[[b]]".to_string(),
        },
        &mut resolved,
    )
    .unwrap();
    assert!(text(resolved).trim_end().ends_with("b.md"));
}

#[test]
fn fmt_check_writes_the_files_that_need_formatting_to_the_given_writer() {
    use satz_cli::commands::fmt_cmd::{FmtArgs, Outcome, run_with_output};
    let v = TempDir::new("lib_fmt");
    v.write("dirty.md", DIRTY);
    v.write("clean.md", DIRTY_FORMATTED);
    let before = snapshot(v.path());

    let mut listed = Vec::new();
    let outcome = run_with_output(
        FmtArgs {
            path: v.path().to_path_buf(),
            check: true,
            write: false,
        },
        &mut listed,
    )
    .unwrap();

    assert_eq!(outcome, Outcome::NeedsFormatting);
    assert_eq!(text(listed), "dirty.md\n");
    assert_eq!(snapshot(v.path()), before, "--check writes nothing");
}

// ---- resolve: what it prints, what it exits with, and that a library call comes back ----

#[test]
fn resolve_returns_to_the_embedding_program_when_the_target_is_unknown() {
    use satz_cli::commands::resolve_cmd::{ResolveArgs, run_with_output};

    // `process::exit` inside the call would end this test run, so the call is made by a child
    // process: this same test, started again with the vault named in the environment.
    if let Ok(vault) = std::env::var("SATZ_TEST_RESOLVE_CHILD") {
        let mut sink = Vec::new();
        let _ = run_with_output(
            ResolveArgs {
                vault: vault.into(),
                target: "[[nothing-here]]".to_string(),
            },
            &mut sink,
        );
        println!("RETURNED");
        return;
    }

    let v = small_vault("resolve_embedded");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "resolve_returns_to_the_embedding_program_when_the_target_is_unknown",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("SATZ_TEST_RESOLVE_CHILD", v.path())
        .output()
        .expect("the test binary can be started again");
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(
        stdout.contains("RETURNED"),
        "the call did not come back (exit {:?}): {stdout}{}",
        child.status.code(),
        String::from_utf8_lossy(&child.stderr)
    );
}

fn resolve_vault(tag: &str) -> TempDir {
    let v = small_vault(tag);
    v.write("h.md", "# H\n\n## Heading\n\ntext\n");
    v
}

#[test]
fn resolve_prints_the_path_of_what_it_found_and_nothing_else() {
    let v = resolve_vault("resolve_found");
    let b = format!("{}\n", v.path().join("b.md").display());
    for target in ["b", "[[b]]", "[[ b ]]", "  [[b]]  "] {
        let o = satz(&["resolve", "-v", v.str(), target]);
        assert_eq!(o.status.code(), Some(0), "{target:?}: {}", err(&o));
        assert_eq!(out(&o), b, "{target:?}");
        assert_eq!(err(&o), "", "{target:?}");
    }
}

#[test]
fn resolve_adds_the_line_of_a_heading_it_finds_and_falls_back_to_the_path() {
    let v = resolve_vault("resolve_heading");
    let path = v.path().join("h.md");
    let with_line = format!("{}:3\n", path.display());
    let path_only = format!("{}\n", path.display());

    for target in ["[[h#Heading]]", "h#Heading", "[[h# Heading ]]"] {
        let o = satz(&["resolve", "-v", v.str(), target]);
        assert_eq!(o.status.code(), Some(0), "{target:?}: {}", err(&o));
        assert_eq!(out(&o), with_line, "{target:?}");
    }
    // A heading the note does not have: the note is still where the link points.
    let o = satz(&["resolve", "-v", v.str(), "[[h#no-such-heading]]"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), path_only);
    assert_eq!(err(&o), "");
}

#[test]
fn resolve_of_an_unknown_target_exits_1_with_the_target_on_stderr_and_no_stdout() {
    let v = resolve_vault("resolve_unknown");
    // (target as given, the name reported: brackets and a `#heading` suffix are stripped)
    for (target, reported) in [
        ("[[nothing-here]]", "nothing-here"),
        ("nothing-here", "nothing-here"),
        ("[[nothing#h]]", "nothing"),
        // Nothing to look up at all: not a match for some note, not a crash.
        ("", ""),
        ("   ", ""),
        ("[[]]", ""),
        ("#h", ""),
        ("[[#h]]", ""),
    ] {
        let o = satz(&["resolve", "-v", v.str(), target]);
        assert_eq!(o.status.code(), Some(1), "{target:?}: {}", err(&o));
        assert_eq!(out(&o), "", "{target:?}: nothing on stdout");
        assert_eq!(err(&o), format!("not found: {reported}\n"), "{target:?}");
    }
}

#[test]
fn resolve_reports_found_and_not_found_as_an_outcome_and_writes_only_what_it_found() {
    use satz_cli::commands::resolve_cmd::{Outcome, ResolveArgs, run_with_output};
    let v = resolve_vault("resolve_outcome");
    let run = |target: &str| {
        let mut written = Vec::new();
        let outcome = run_with_output(
            ResolveArgs {
                vault: v.path().to_path_buf(),
                target: target.to_string(),
            },
            &mut written,
        )
        .expect("a target that is not found is not an error");
        (outcome, text(written))
    };

    let (outcome, written) = run("[[h#Heading]]");
    assert_eq!(outcome, Outcome::Found);
    assert_eq!(written, format!("{}:3\n", v.path().join("h.md").display()));

    for target in ["[[nothing-here]]", "", "[[]]", "#h"] {
        let (outcome, written) = run(target);
        assert_eq!(outcome, Outcome::NotFound, "{target:?}");
        assert_eq!(
            written, "",
            "{target:?}: nothing was found, nothing is written"
        );
    }

    // A vault that cannot be read is still an error, not a "not found".
    let missing = v.path().join("does-not-exist");
    let e = run_with_output(
        ResolveArgs {
            vault: missing,
            target: "b".to_string(),
        },
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("does not exist"), "{e}");
}

// ---- graph: the same vault gives the same output every run ----

#[test]
fn graph_output_is_the_same_in_every_run_and_in_note_id_order() {
    let v = TempDir::new("graph_stable");
    for i in 0..36 {
        let folder = ["", "sub/", "Zeta/", "ş/"][i % 4];
        v.write(
            &format!("{folder}n{i:02}.md"),
            &format!(
                "# Note {i}\n\n[[n{:02}]] ![[n{:02}]] [[n{:02}]]\n",
                (i + 1) % 36,
                (i + 5) % 36,
                (i + 1) % 36
            ),
        );
    }
    let run = |format: &str| {
        let o = satz(&["graph", "-v", v.str(), "-f", format]);
        assert!(o.status.success(), "{}", err(&o));
        out(&o)
    };

    let json = run("json");
    let dot = run("dot");
    for round in 0..4 {
        // Every run is a new process with its own hash seeds.
        assert_eq!(run("json"), json, "JSON differs in run {round}");
        assert_eq!(run("dot"), dot, "DOT differs in run {round}");
    }

    let data: serde_json::Value = serde_json::from_str(&json).expect("JSON");
    let ids: Vec<&str> = data["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids.len(), 36);
    assert_eq!(ids, sorted, "nodes come in note id order");
}

// ---- daily: the note is created in one step, never over something that is already there ----

#[test]
fn a_note_that_appears_while_daily_runs_is_never_overwritten() {
    use satz_cli::commands::daily_cmd::{DailyArgs, run_with_output};
    use std::io::Write as _;
    use std::sync::Barrier;

    // `daily` against a "user" who creates the very same note at about the same moment (an
    // editor, a second `satz daily`, a sync tool). Whoever creates it first owns it: the other
    // one must leave it alone. The user's wait before creating differs from round to round, so
    // the moment sweeps across the whole time `daily` needs.
    let (mut user_first, mut daily_first) = (0, 0);
    for round in 0..300u32 {
        let v = TempDir::new("daily_race");
        let note = v.path().join("daily").join(format!("{}.md", today()));
        let barrier = Barrier::new(2);

        let (daily_result, user_created) = std::thread::scope(|s| {
            let daily = s.spawn(|| {
                barrier.wait();
                run_with_output(
                    DailyArgs {
                        path: v.path().to_path_buf(),
                        create: true,
                    },
                    &mut Vec::new(),
                )
            });
            let user = s.spawn(|| {
                barrier.wait();
                for _ in 0..((round * 37 % 900) * (1 + round % 60)) {
                    std::hint::spin_loop();
                }
                std::fs::create_dir_all(note.parent().unwrap()).unwrap();
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&note)
                {
                    Ok(mut file) => {
                        file.write_all(b"USER CONTENT\n").unwrap();
                        true
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
                    Err(e) => panic!("round {round}: the user could not create the note: {e}"),
                }
            });
            (daily.join().unwrap(), user.join().unwrap())
        });

        let content = std::fs::read_to_string(&note).unwrap();
        assert!(
            daily_result.is_ok(),
            "round {round}: `daily` failed although the note simply appeared: {daily_result:?}"
        );
        if user_created {
            user_first += 1;
            assert_eq!(
                content, "USER CONTENT\n",
                "round {round}: the note the user created was overwritten"
            );
        } else {
            daily_first += 1;
            assert!(
                content.contains(&today()) && !content.contains("USER CONTENT"),
                "round {round}: `daily` created it first, the file is not its template: {content:?}"
            );
        }
    }
    eprintln!("daily race: the user was first in {user_first} rounds, `daily` in {daily_first}");
}

/// A symbolic link from `link` to `target`; `false` when this machine does not allow one.
fn make_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, link);
    made.is_ok()
}

#[test]
fn a_dangling_link_at_the_note_path_is_left_alone() {
    // `Path::exists` follows a link, so a link to nothing counted as "no note yet" and the write
    // went THROUGH it, creating a file wherever the link pointed -- outside the vault.
    let v = TempDir::new("daily_dangling");
    let outside = TempDir::new("daily_dangling_outside");
    let target = outside.path().join("created-through-the-link.md");
    std::fs::create_dir_all(v.path().join("daily")).unwrap();
    let link = v.path().join("daily").join(format!("{}.md", today()));
    if !make_symlink(&target, &link) {
        println!("skipped: this machine does not allow creating symbolic links");
        return;
    }

    let o = satz(&["daily", v.str()]);

    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).trim().ends_with(&format!("{}.md", today())),
        "{}",
        out(&o)
    );
    assert!(
        !target.exists(),
        "a file was created outside the vault, through the link"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link itself is left as it was"
    );
}

#[test]
fn daily_leaves_whatever_is_at_the_note_path_alone() {
    // A folder with the note's name, and a read-only note: both are "already there".
    let v = TempDir::new("daily_occupied");
    let as_folder = v.path().join("daily").join(format!("{}.md", today()));
    std::fs::create_dir_all(&as_folder).unwrap();
    let before = snapshot(v.path());
    let o = satz(&["daily", v.str()]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).trim().ends_with(&format!("{}.md", today())));
    assert_eq!(snapshot(v.path()), before, "the folder is left as it was");

    let v = TempDir::new("daily_readonly");
    let rel = format!("daily/{}.md", today());
    let path = v.write(&rel, "READ ONLY, keep it\n");
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    let o = satz(&["daily", v.str()]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(v.read(&rel), b"READ ONLY, keep it\n");
}

// ---- `[vault] gitignore`: which notes the commands see ----

/// A folder that is no git repository, with a `.gitignore` that names one of two notes and an
/// unformatted note the `.gitignore` also names.
fn vault_with_a_gitignore(tag: &str) -> TempDir {
    let v = TempDir::new(tag);
    v.write("keep.md", "# Keep\n");
    v.write("secret.md", "# Secret\n");
    v.write("ignored-and-dirty.md", DIRTY);
    v.write(".gitignore", "secret.md\nignored-and-dirty.md\n");
    v
}

#[test]
fn a_gitignore_outside_a_repository_counts_only_when_the_vault_says_so() {
    let v = vault_with_a_gitignore("gitignore_setting");
    let listed = |v: &TempDir| sorted_lines(&satz(&["list", "-v", v.str()]));
    let all = vec!["ignored-and-dirty.md", "keep.md", "secret.md"];

    // Not a repository: the default reads every note, as it always did.
    assert_eq!(listed(&v), all);
    v.write(".satz.toml", "[vault]\ngitignore = \"in-repo\"\n");
    assert_eq!(listed(&v), all, "\"in-repo\" is the default spelled out");

    v.write(".satz.toml", "[vault]\ngitignore = \"always\"\n");
    assert_eq!(listed(&v), vec!["keep.md"]);

    // Every command that reads the vault sees the same notes.
    let stats = satz(&["stats", "-v", v.str(), "--json"]);
    let stats: serde_json::Value = serde_json::from_str(&out(&stats)).unwrap();
    assert_eq!(stats["doc_count"], 1);
    let index = satz(&["index", v.str()]);
    assert!(
        out(&index).contains("1 documents indexed"),
        "{}",
        out(&index)
    );
    let graph = satz(&["graph", "-v", v.str()]);
    let graph: serde_json::Value = serde_json::from_str(&out(&graph)).unwrap();
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 1);
    assert!(!satz(&["resolve", "-v", v.str(), "secret"]).status.success());
    assert!(satz(&["resolve", "-v", v.str(), "keep"]).status.success());

    // `fmt` leaves the ignored, unformatted note alone -- and says nothing about it.
    let fmt = satz(&["fmt", v.str(), "--check"]);
    assert_eq!(fmt.status.code(), Some(0), "{}", err(&fmt));
    assert_eq!(v.read("ignored-and-dirty.md"), DIRTY.as_bytes());

    // Back to the default: the note is a note again.
    v.write(".satz.toml", "");
    assert_eq!(listed(&v), all);
    assert_eq!(
        satz(&["fmt", v.str(), "--check"]).status.code(),
        Some(1),
        "the dirty note is seen (and reported) again"
    );
}

#[test]
fn a_mistake_in_the_vault_section_is_a_warning_and_the_default_applies() {
    for (config, named) in [
        ("[vault]\ngitgnore = \"always\"\n", "vault.gitgnore"),
        ("[vault]\ngitignore = \"sometimes\"\n", "vault.gitignore"),
    ] {
        let v = vault_with_a_gitignore("gitignore_mistake");
        v.write(".satz.toml", config);

        let o = satz(&["list", "-v", v.str()]);

        assert_eq!(o.status.code(), Some(0), "{config:?}: {}", err(&o));
        assert!(err(&o).contains("warning:"), "{config:?}: {}", err(&o));
        assert!(err(&o).contains(named), "{config:?}: {}", err(&o));
        assert_eq!(
            sorted_lines(&o),
            vec!["ignored-and-dirty.md", "keep.md", "secret.md"],
            "{config:?}: the mistake changed nothing"
        );
    }
}

#[test]
fn a_broken_satz_toml_is_an_error_for_the_commands_that_only_read_too() {
    // The file decides which notes they see, so they do not carry on with a guess.
    let v = vault_with_a_gitignore("gitignore_broken");
    v.write(".satz.toml", "[vault\ngitignore = \n");
    for args in [
        vec!["index", v.str()],
        vec!["stats", "-v", v.str()],
        vec!["list", "-v", v.str()],
        vec!["resolve", "-v", v.str(), "keep"],
        vec!["graph", "-v", v.str()],
    ] {
        let o = satz(&args);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
        assert!(err(&o).contains(".satz.toml"), "{args:?}: {}", err(&o));
        assert_eq!(out(&o), "", "{args:?}: nothing is printed for a guess");
    }
}
