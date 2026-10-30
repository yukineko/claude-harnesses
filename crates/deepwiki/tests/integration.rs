//! End-to-end tests for the `deepwiki` subcommand CLI: spawn the real built
//! binary and assert on stdout + exit code. deepwiki is a plain CLI (not a hook);
//! `--root` makes its read-only subcommands point at an isolated temp dir.

use std::path::Path;
use std::process::Command;

mod support;
use support::git_env::scrub;

/// Run `deepwiki <args...>`. Returns (exit_code, stdout).
fn run(args: &[&str]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_deepwiki");
    let out = scrub(&mut Command::new(bin))
        .args(args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// A unique empty temp dir to serve as an isolated repo root.
fn temp_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("deepwiki-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp root");
    dir
}

#[test]
fn help_lists_real_subcommands() {
    let (code, stdout) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage: deepwiki"), "got: {stdout}");
    assert!(stdout.contains("scan"), "got: {stdout}");
    assert!(stdout.contains("status"), "got: {stdout}");
}

#[test]
fn status_on_dir_without_wiki_reports_no_wiki() {
    // Read-only `status` against a fresh temp root (no .deepwiki/) → the source
    // prints the "no wiki yet" line and exits 0.
    let root = temp_root("status");
    assert!(!Path::new(&root).join(".deepwiki").exists());
    let (code, stdout) = run(&["status", "--root", root.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stdout.contains("no wiki yet"), "got: {stdout}");
}

#[test]
fn scan_json_emits_repo_map() {
    // `scan --json` over an empty temp root → a JSON map echoing the root path.
    let root = temp_root("scan");
    let (code, stdout) = run(&["scan", "--json", "--root", root.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"root\""), "got: {stdout}");
    assert!(stdout.contains("\"total_files\""), "got: {stdout}");
}
