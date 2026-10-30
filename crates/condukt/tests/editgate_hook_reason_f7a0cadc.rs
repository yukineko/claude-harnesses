// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog f7a0cadc (hook layer): what the OPERATOR sees from the
//! `condukt editgate` PostToolUse hook, observed on the real binary.
//!
//! `editgate::check_edit` returns a verdict whose `reason` says WHY it
//! rejected (compile errors, toolchain unresolvable, cargo unspawnable). The
//! hook used to print only `verdict.diagnostics`, falling back to "edit left
//! the crate not compiling (see cargo check)" when there were none — so with
//! `cargo` missing from PATH the operator was told the code did not compile,
//! when nothing had compiled anything. These tests pin:
//!
//! - cargo missing from PATH: still blocks; the reason names the environment
//!   (`cargo` / could not be determined) and says neither "not compiling" nor
//!   "compile/type errors";
//! - a real compile error: still blocks; the reason says compile errors and
//!   carries rustc's diagnostic (E0308);
//! - a clean crate: no output, exit 0.

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

struct Fixture {
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    broken_wt: PathBuf,
    clean_wt: PathBuf,
}

fn cargo_project(dir: &Path, name: &str, lib_rs: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [workspace]\n\n[lib]\npath = \"src/lib.rs\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join("lib.rs"), lib_rs).unwrap();
}

fn setup() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    let broken_wt = repo.path().join("broken_wt");
    let clean_wt = repo.path().join("clean_wt");
    cargo_project(
        &broken_wt,
        "f7a0cadc_broken",
        "pub fn f() -> i32 {\n    let s: &str = \"nope\";\n    s\n}\n",
    );
    cargo_project(
        &clean_wt,
        "f7a0cadc_clean",
        "pub fn f() -> i32 {\n    42\n}\n",
    );
    let state_dir = home
        .path()
        .join(".condukt")
        .join("state")
        .join(project_key(repo.path()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let run_json = serde_json::json!({
        "run_id": "f7a0cadc-hook",
        "goal": "editgate hook reason",
        "tasks": [
            {"id": "a", "status": "pending", "worktree": broken_wt.to_string_lossy()},
            {"id": "b", "status": "pending", "worktree": clean_wt.to_string_lossy()},
        ]
    });
    std::fs::write(
        state_dir.join("f7a0cadc-hook.json"),
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

/// The real rustup/cargo homes of THIS test process, so the hook child (whose
/// HOME is the fixture home) still resolves the real toolchain.
fn real_toolchain_env() -> Vec<(&'static str, PathBuf)> {
    let real_home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let rustup = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| real_home.join(".rustup"));
    let cargo = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| real_home.join(".cargo"));
    vec![("RUSTUP_HOME", rustup), ("CARGO_HOME", cargo)]
}

fn run_hook(fx: &Fixture, file: &Path, path_override: Option<&str>) -> Output {
    let mut cmd = Command::new(BIN);
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("editgate")
        .current_dir(&fx.repo)
        .env("HOME", &fx.home)
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CARGO_TARGET_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in real_toolchain_env() {
        cmd.env(k, v);
    }
    if let Some(p) = path_override {
        cmd.env("PATH", p);
    }
    let mut child = cmd.spawn().expect("spawn condukt editgate");
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": file.to_string_lossy() },
    })
    .to_string();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait for condukt editgate")
}

fn block_reason(out: &Output) -> String {
    assert!(
        out.status.success(),
        "hook exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected one JSON block line, got {stdout:?}: {e}"));
    assert_eq!(v["decision"], "block", "{stdout}");
    v["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("block line has no string reason: {stdout}"))
        .to_string()
}

#[test]
fn cargo_missing_from_path_blocks_with_an_environment_reason_not_a_compile_claim() {
    // Precondition, asserted: this PATH has no `cargo` (but keeps git etc.).
    let path = "/usr/bin:/bin";
    for d in path.split(':') {
        assert!(
            !Path::new(d).join("cargo").exists(),
            "precondition: {d} must not contain cargo"
        );
    }
    let fx = setup();
    let out = run_hook(&fx, &fx.clean_wt.join("src").join("lib.rs"), Some(path));
    let reason = block_reason(&out);
    let lower = reason.to_ascii_lowercase();
    assert!(
        lower.contains("cargo"),
        "the reason must name cargo: {reason:?}"
    );
    assert!(
        lower.contains("could not be determined"),
        "the reason must say compile status is undetermined: {reason:?}"
    );
    assert!(
        !lower.contains("not compiling"),
        "cargo never ran; the reason must not claim the crate is not compiling: {reason:?}"
    );
    assert!(
        !lower.contains("compile/type errors"),
        "cargo never ran; the reason must not claim compile errors: {reason:?}"
    );
}

#[test]
fn a_real_compile_error_blocks_with_the_compile_reason_and_rustc_diagnostics() {
    let fx = setup();
    let out = run_hook(&fx, &fx.broken_wt.join("src").join("lib.rs"), None);
    let reason = block_reason(&out);
    assert!(
        reason.contains("compile/type errors"),
        "a real compile error keeps the compile reason: {reason:?}"
    );
    assert!(
        reason.contains("E0308"),
        "rustc's diagnostic must be carried in the block reason: {reason:?}"
    );
}

#[test]
fn a_clean_crate_produces_no_output() {
    let fx = setup();
    let out = run_hook(&fx, &fx.clean_wt.join("src").join("lib.rs"), None);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "a clean edit must produce no output; got {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
