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
    assert!(stdout.contains("— dosya bulunamadı") || stdout.contains("— dosya var, başlık yok"));
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
