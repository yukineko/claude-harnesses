// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration test for the `condukt editgate` PostToolUse hook subcommand.
//!
//! This is the Fail→Pass oracle for the `editgate-wire` task: before the
//! subcommand exists, piping a *broken-edit* PostToolUse payload produces no
//! `{"decision":"block",...}` line (clap rejects the unknown subcommand and
//! writes nothing to stdout), so the block assertion FAILS (RED). Once the
//! subcommand is wired, the same payload yields a one-line block verdict
//! carrying a non-empty `reason` (GREEN). The RED→GREEN transition therefore
//! hinges purely on the subcommand existing.
//!
//! The hook must be fail-soft everywhere else: a clean-file edit, a non-Rust
//! file, an edit outside any live worktree, and an empty stdin each produce
//! EMPTY stdout and exit 0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

// ── project-key derivation (mirrors harness_core::projkey) ─────────────────
//
// `condukt` is a bin crate with no lib target, so this integration test cannot
// call `harness_core::projkey` directly. We reproduce the SAME key derivation
// here so we can drop the run-state JSON in the exact directory the `editgate`
// subcommand resolves via `Config.state_dir + project_key(repo_root(cwd))`.
// If the upstream scheme ever changes, this test breaks loudly (the block
// assertion fails), which is the intended signal.
fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn project_key(root: &Path) -> String {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let full = canon.to_string_lossy();
    let base = canon
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".into());
    let sani: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{}-{:08x}", sani, fnv1a32(&full))
}

// ── fixture ────────────────────────────────────────────────────────────────

struct Fixture {
    // Held to keep the temp dirs alive for the test's lifetime.
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    broken_wt: PathBuf,
    clean_wt: PathBuf,
}

/// Write a minimal standalone cargo project (its own manifest, no workspace)
/// with the given crate name and `src/lib.rs` body.
fn cargo_project(dir: &Path, name: &str, lib_rs: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join("lib.rs"), lib_rs).unwrap();
}

fn setup() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // A `.git` marker so `repo_root(cwd)` stops walking at `repo` regardless of
    // whatever ancestors the system temp dir happens to sit under — making the
    // project key deterministic.
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();

    let broken_wt = repo.path().join("broken_wt");
    let clean_wt = repo.path().join("clean_wt");

    // Broken crate: `s` is `&str` but the fn returns `i32` → E0308, so
    // `cargo check` fails and prints an `error: could not compile ...` summary.
    cargo_project(
        &broken_wt,
        "broken_fixture",
        "pub fn f() -> i32 {\n    let s: &str = \"nope\";\n    s\n}\n",
    );
    // Clean crate: compiles fine.
    cargo_project(
        &clean_wt,
        "clean_fixture",
        "pub fn f() -> i32 {\n    42\n}\n",
    );

    // Place the run-state JSON where the subcommand looks:
    //   <HOME>/.condukt/state/<project_key(repo)>/<run>.json
    // An OPEN run (both tasks pending) whose task worktrees point at the two
    // fixture crates, so `active_worktree_for_path` resolves edits under them.
    let key = project_key(repo.path());
    let state_dir = home.path().join(".condukt").join("state").join(&key);
    std::fs::create_dir_all(&state_dir).unwrap();
    let run_json = serde_json::json!({
        "run_id": "editgate-test",
        "goal": "editgate integration",
        "tasks": [
            {"id": "a", "status": "pending", "worktree": broken_wt.to_string_lossy()},
            {"id": "b", "status": "pending", "worktree": clean_wt.to_string_lossy()},
        ]
    });
    std::fs::write(
        state_dir.join("editgate-test.json"),
        serde_json::to_string_pretty(&run_json).unwrap(),
    )
    .unwrap();

    Fixture {
        home: home.path().to_path_buf(),
        repo: repo.path().to_path_buf(),
        broken_wt,
        clean_wt,
        _home: home,
        _repo: repo,
    }
}

/// Spawn `condukt editgate` with `cwd == repo`, `HOME == fixture home`, and the
/// given stdin payload. Returns the completed process output.
fn run_editgate(fx: &Fixture, stdin: &str) -> Output {
    let mut child = Command::new(BIN)
        .arg("editgate")
        .current_dir(&fx.repo)
        .env("HOME", &fx.home)
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn condukt editgate");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait for condukt editgate")
}

fn edit_payload(file_path: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": file_path.to_string_lossy() },
    })
    .to_string()
}

// ── the F→P oracle: a broken edit is BLOCKED ───────────────────────────────

#[test]
fn broken_edit_in_worktree_blocks() {
    let fx = setup();
    let file = fx.broken_wt.join("src").join("lib.rs");
    let out = run_editgate(&fx, &edit_payload(&file));

    assert!(
        out.status.success(),
        "hook must exit 0 (never break a turn); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim();
    assert!(
        !line.is_empty(),
        "a broken edit must produce a block verdict, got empty stdout"
    );
    let v: serde_json::Value =
        serde_json::from_str(line).expect("block verdict must be one JSON line");
    assert_eq!(
        v["decision"], "block",
        "broken edit must be blocked; got {line}"
    );
    let reason = v["reason"].as_str().unwrap_or("");
    assert!(
        !reason.is_empty(),
        "block verdict must carry a non-empty reason; got {line}"
    );
}

// ── fail-soft cases: EMPTY stdout, exit 0 ──────────────────────────────────

#[test]
fn clean_edit_in_worktree_allows() {
    let fx = setup();
    let file = fx.clean_wt.join("src").join("lib.rs");
    let out = run_editgate(&fx, &edit_payload(&file));
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "a clean edit must produce no output; got {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn non_rust_file_allows() {
    let fx = setup();
    let file = fx.broken_wt.join("README.md");
    let out = run_editgate(&fx, &edit_payload(&file));
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "a non-Rust edit must produce no output; got {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn edit_outside_any_worktree_allows() {
    let fx = setup();
    // A `.rs` path directly under the repo root — not inside either worktree.
    let file = fx.repo.join("loose.rs");
    let out = run_editgate(&fx, &edit_payload(&file));
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "an out-of-worktree edit must produce no output; got {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn empty_stdin_allows() {
    let fx = setup();
    let out = run_editgate(&fx, "");
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "empty stdin must produce no output; got {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
