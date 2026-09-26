//! What the CI workflow (`.github/workflows/ci.yml`) promises, checked against the repository: the
//! lock file is enforced on every command that builds, and the oldest Rust the project claims
//! (`rust-version` in `Cargo.toml`) is built on every system. Someone who takes either out gets a
//! failing test, not a green CI that no longer checks it.
//!
//! The workflow is read as text (no YAML dependency): the tests look for the words that matter and
//! assume nothing about their order or indentation beyond "a job starts at two spaces".

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The lines of the job `name` (from its `  name:` line to the next job).
fn job(workflow: &str, name: &str) -> String {
    let lines: Vec<&str> = workflow.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == format!("  {name}:"))
        .unwrap_or_else(|| panic!("no job `{name}` in the workflow"));
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("  ") && !l.starts_with("   ") && !l.trim().starts_with('#'))
        .map_or(lines.len(), |at| start + 1 + at);
    lines[start..end].join("\n")
}

/// The commands the workflow runs (`- run: <command>` lines).
fn commands(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("- run:")
                .or(l.trim().strip_prefix("run:"))
        })
        .map(|c| c.trim().to_string())
        .collect()
}

/// `rust-version` of the workspace, as written in `Cargo.toml`.
fn rust_version() -> String {
    read("Cargo.toml")
        .lines()
        .find_map(|l| l.strip_prefix("rust-version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("a `rust-version = \"…\"` line in the workspace manifest")
        .to_string()
}

#[test]
fn every_cargo_command_that_builds_or_tests_is_run_with_the_lock_file_enforced() {
    let workflow = read(".github/workflows/ci.yml");
    let mut with_a_lock = 0;
    for command in commands(&workflow) {
        let words: Vec<&str> = command.split_whitespace().collect();
        if words.first() != Some(&"cargo") {
            continue;
        }
        // `cargo fmt` reads no dependencies; the rest resolve them.
        let subcommand = words.get(1).copied().unwrap_or("");
        if subcommand == "fmt" {
            continue;
        }
        assert!(
            words.contains(&"--locked"),
            "`{command}` does not enforce the lock file (`--locked`)"
        );
        with_a_lock += 1;
    }
    // clippy, test, and the check of the oldest Rust: at least these three.
    assert!(with_a_lock >= 3, "{with_a_lock} commands with `--locked`");
}

#[test]
fn the_oldest_rust_the_project_claims_is_built_from_the_version_in_cargo_toml_on_every_system() {
    let workflow = read(".github/workflows/ci.yml");
    let msrv = job(&workflow, "msrv");

    // The version is read from the manifest, not written a second time.
    assert!(
        msrv.contains("rust-version") && msrv.contains("Cargo.toml"),
        "the job does not read `rust-version` from Cargo.toml:\n{msrv}"
    );
    let toolchain = msrv
        .lines()
        .find_map(|l| l.trim().strip_prefix("toolchain:"))
        .expect("a `toolchain:` for the job")
        .trim();
    assert!(
        toolchain.contains("steps.") && toolchain.contains(".outputs."),
        "the toolchain is {toolchain:?}, not what the manifest says"
    );
    assert!(
        !toolchain.chars().any(|c| c.is_ascii_digit()),
        "a version written in the workflow: {toolchain:?}"
    );

    // What is built: every package and every target (tests, benchmarks), as the lock file has it.
    let commands = commands(&msrv);
    assert!(
        commands.iter().any(|c| c.starts_with("cargo check")
            && c.contains("--workspace")
            && c.contains("--all-targets")
            && c.contains("--locked")),
        "no `cargo check --workspace --all-targets --locked`: {commands:?}"
    );

    // On every system the ordinary job runs on.
    for os in ["ubuntu-latest", "windows-latest", "macos-latest"] {
        assert!(msrv.contains(os), "the job does not run on {os}");
        assert!(
            job(&workflow, "test").contains(os),
            "`test` does not run on {os}"
        );
    }
}

#[test]
fn the_ordinary_job_runs_the_checks_on_the_stable_toolchain() {
    let workflow = read(".github/workflows/ci.yml");
    let test = job(&workflow, "test");
    assert!(test.contains("dtolnay/rust-toolchain@stable"), "{test}");
    let commands = commands(&test);
    let has = |needle: &[&str]| {
        commands.iter().any(|c| {
            needle
                .iter()
                .all(|word| c.split_whitespace().any(|w| w == *word))
        })
    };
    assert!(has(&["cargo", "fmt", "--check"]), "{commands:?}");
    assert!(
        has(&[
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "-D",
            "warnings"
        ]),
        "{commands:?}"
    );
    assert!(
        has(&["cargo", "test", "--workspace", "--locked"]),
        "{commands:?}"
    );
}

#[test]
fn the_minimum_rust_version_is_a_version_and_the_readme_says_the_same() {
    let version = rust_version();
    let parts: Vec<&str> = version.split('.').collect();
    assert!(
        (2..=3).contains(&parts.len()) && parts.iter().all(|p| p.parse::<u32>().is_ok()),
        "`rust-version` is {version:?}"
    );
    let readme = read("README.md");
    assert!(
        readme.contains(&format!("Rust {version} or newer")),
        "the README does not say `Rust {version} or newer`"
    );
}
