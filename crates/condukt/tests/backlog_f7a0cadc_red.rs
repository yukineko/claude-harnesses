// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog f7a0cadc — deterministic RED/GREEN regression on the real binary.
//!
//! The defect: when the `cargo` that `condukt editgate` spawns could not
//! resolve a rustup toolchain (in the original report: because the child's
//! `HOME` pointed at a sandbox with no `~/.rustup`), rustup's own
//! `error: rustup could not choose a version of cargo to run ...` line was
//! treated as a compile diagnostic, and the operator was told the edit broke
//! the crate. Nothing had compiled anything.
//!
//! What this test can and cannot observe (stated, not glossed):
//! - It CAN observe, deterministically, what the PostToolUse hook tells the
//!   operator when the toolchain is unresolvable: the child gets an empty
//!   `HOME` and no `RUSTUP_HOME`/`CARGO_HOME`/`RUSTUP_TOOLCHAIN`, so the rustup
//!   proxy fails on every run, not only under a race.
//! - It CANNOT observe the in-process HOME race (a sibling test swapping the
//!   process-global `HOME` while `check_edit` spawns cargo). In a fresh
//!   one-shot hook process nothing mutates `HOME`, so the fix's env snapshot
//!   equals what the child inherits; that part stays covered only by the
//!   in-crate `editgate::backlog_f7a0cadc` test, which is race-dependent.
//!
//! Contract pinned: the edit is still BLOCKED (undetermined resolves to the
//! restrictive side), and the block reason says compile status could not be
//! determined because of the toolchain — it never claims compile errors.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

// Mirrors harness_core::projkey (condukt is a bin crate; same derivation as
// tests/edit_gate_hook.rs).
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

/// The directory holding the real rustup `cargo` proxy: `$CARGO_HOME/bin`, or
/// `$HOME/.cargo/bin`, of THIS test process.
fn rustup_proxy_bin_dir() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cargo"))
        .join("bin")
}

#[test]
fn unresolvable_toolchain_blocks_with_an_undetermined_reason_not_a_compile_claim() {
    // Fixture: an empty sandbox HOME (no ~/.rustup, no ~/.cargo) that holds
    // only condukt's run state, and a clean crate registered as a live task
    // worktree under a fake repo root.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    let wt = repo.path().join("clean_wt");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(
        wt.join("Cargo.toml"),
        "[package]\nname = \"f7a0cadc_red\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [workspace]\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    std::fs::write(
        wt.join("src").join("lib.rs"),
        "pub fn f() -> i32 {\n    42\n}\n",
    )
    .unwrap();
    let state_dir = home
        .path()
        .join(".condukt")
        .join("state")
        .join(project_key(repo.path()));
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(
        state_dir.join("f7a0cadc-red.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "run_id": "f7a0cadc-red",
            "goal": "editgate toolchain-unresolvable reason",
            "tasks": [
                {"id": "a", "status": "pending", "worktree": wt.to_string_lossy()},
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    // Precondition, asserted: the cargo the child will find is a rustup proxy.
    let proxy_dir = rustup_proxy_bin_dir();
    let proxy = proxy_dir.join("cargo");
    let target = std::fs::read_link(&proxy).unwrap_or_else(|e| {
        panic!(
            "precondition: {} must be a symlink to rustup ({e})",
            proxy.display()
        )
    });
    assert_eq!(
        target.file_name().and_then(|s| s.to_str()),
        Some("rustup"),
        "precondition: {} must be a rustup proxy, got {}",
        proxy.display(),
        target.display()
    );

    let mut cmd = Command::new(BIN);
    cmd.arg("editgate")
        .current_dir(repo.path())
        .env("HOME", home.path())
        // PATH: the rustup proxy dir first, then system dirs (git etc.).
        .env("PATH", format!("{}:/usr/bin:/bin", proxy_dir.display()))
        // No toolchain hints at all: the proxy can only consult $HOME/.rustup,
        // which does not exist, so it fails on every run.
        .env_remove("RUSTUP_HOME")
        .env_remove("CARGO_HOME")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn condukt editgate");
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": wt.join("src").join("lib.rs").to_string_lossy() },
    })
    .to_string();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out: Output = child.wait_with_output().expect("wait for condukt editgate");

    assert!(
        out.status.success(),
        "hook exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("the toolchain-unresolvable edit must still be blocked (one JSON line), got {stdout:?}: {e}")
    });
    assert_eq!(v["decision"], "block", "{stdout}");
    let reason = v["reason"].as_str().unwrap_or_default().to_string();
    let lower = reason.to_ascii_lowercase();

    // Precondition, asserted from the observation itself: rustup DID fail.
    assert!(
        lower.contains("rustup"),
        "precondition: the sandbox HOME must make the rustup proxy fail: {reason:?}"
    );

    // The defect: the operator must be told compile status is undetermined
    // because of the toolchain, not that the edited code fails to compile.
    assert!(
        !lower.contains("compile/type errors") && !lower.contains("not compiling"),
        "cargo never ran; the reason must not claim compile errors: {reason:?}"
    );
    assert!(
        lower.contains("toolchain") && lower.contains("could not be determined"),
        "the reason must say the toolchain could not be resolved so compile status \
         could not be determined: {reason:?}"
    );
}
