// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/fetchguard` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host. Mirrors
//! the launcher tests the other plugin crates carry.
//!
//! `scan` is PostToolUse with no `permissionDecision` channel, so the
//! fail-closed signal here is a non-silent `additionalContext` warning
//! (never a bare `exit 0` with empty stdout, which would read as "checked,
//! nothing found").

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
}

/// Run the real launcher from a directory that provably contains no
/// `fetchguard-<os>-<arch>` sibling, so the missing-binary path is taken on
/// every platform (the repo's own `bin/` may hold a built binary once this
/// task's rollout has run).
fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("fetchguard");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("fetchguard"),
        &launcher,
    )
    .expect("copy launcher");

    let mut child = Command::new("sh")
        .arg(&launcher)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn scan_missing_binary_warns_instead_of_silently_allowing() {
    let r = run(&["scan"], r#"{"tool_name":"WebFetch"}"#);
    assert_eq!(r.code, 0, "PostToolUse hooks must never break the turn");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence IS the allow — the launcher must emit additionalContext: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "{v}"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(
        ctx.contains("fetchguard") && ctx.contains("UNTRUSTED DATA"),
        "the warning is the only steering channel; it must name the cause \
         and still say untrusted: {ctx}",
    );
}

#[test]
fn unknown_subcommand_exits_nonzero_rather_than_printing_empty_data() {
    let r = run(&["not-a-real-subcommand"], "");
    assert_ne!(
        r.code, 0,
        "an unrecognised CLI call must not look like clean empty output"
    );
}
