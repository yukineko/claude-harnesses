#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-19 on blastguard 0.2.738 built at
//! main 5a27f8ecd, hermetic HOME, PreToolUse Bash JSON, empty stdout = Allow):
//!
//! ```text
//! for f in /etc; do ln -st /tmp/L15 $f; done; echo x > /tmp/L15/etc/passwd   -> allow
//! ```
//!
//! `-st` is the combined short-option form of `-s -t DIR` (GNU and BSD `ln`):
//! it creates `/tmp/L15/etc -> /etc`, so the redirect writes `/etc/passwd`.
//! Same probe table (also allow): `ln -sft DIR etc`. (`ln -ft`, a hard link, is
//! not asserted: a hard link to a directory is not a symlink-to-/etc harm.)
//! Controls observed in the same table:
//!   `ln -s --target-directory=/tmp/L16 $f`          -> ask
//!   `ln --target-directory /tmp/L17 -s $f`          -> ask
//!   `for f in /etc/hosts; do echo x > $f; done`     -> deny
//! so the same link spelled with `--target-directory` is refused and only the
//! combined short form slips through.
//!
//! Two readings: (a) fail-open: `-t` inside a combined short option (`-st`,
//! `-sft`) is not recognised, so the relink target directory is not seen
//! and the later write through it is judged as an ordinary /tmp write; (b)
//! intended: `-st` is not a target-directory form. Tell apart: GNU `ln(1)` and
//! BSD `ln(1)` both parse getopt clusters, so `-st DIR` == `-s -t DIR`, and the
//! long-form control that is refused creates the identical link. (b) would need
//! `-st` to not create `DIR/etc`, which it does. -> reading (a).
//!
//! Suspected root cause (read, not changed): `target_dir` in
//! crates/blastguard/src/detect/relink.rs and `positional_operands` in
//! crates/blastguard/src/detect/fileops.rs compare `-t` by exact match.
//!
//! User ruling: system harm must be Deny through any wrapper, so the asserted
//! verdict is `deny` (not merely != allow).
//!
//! DOES NOT PROVE: that no other combined-short-option cluster (`-sT`, `-snt`,
//! `cp -at`, `mv -ft`, `install -t`) has the same hole (not probed); that the
//! deny reason will name the symlink; that the non-loop shape is also Allow
//! unless its test below fails when run with `--ignored` (see its doc).
//!
//! RED written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-lnt-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "ln-combined-t-20261119",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn assert_deny(command: &str, tag: &str) {
    let home = unique_home(tag);
    let (d, r) = run(command, &home);
    assert_eq!(
        d, "deny",
        "a symlink to /etc followed by a write through it is system harm and must be Deny; command={command} reason={r}"
    );
}

#[test]
fn for_loop_ln_st_then_write_through_link_is_deny() {
    assert_deny(
        "for f in /etc; do ln -st /tmp/L15 $f; done; echo x > /tmp/L15/etc/passwd",
        "st-loop",
    );
}

#[test]
fn for_loop_ln_sft_then_write_through_link_is_deny() {
    assert_deny(
        "for f in /etc; do ln -sft /tmp/L1 $f; done; echo x > /tmp/L1/etc/passwd",
        "sft-loop",
    );
}

#[test]
fn plain_ln_st_then_write_through_link_is_deny() {
    assert_deny(
        "ln -st /tmp/L40 /etc; echo x > /tmp/L40/etc/passwd",
        "st-plain",
    );
}

/// Controls: pass today. Long-form `--target-directory` is already refused.
#[test]
fn control_long_target_directory_equals_form_is_not_allowed() {
    let home = unique_home("long-eq");
    let (d, r) = run(
        "for f in /etc; do ln -s --target-directory=/tmp/L16 $f; done; echo x > /tmp/L16/etc/passwd",
        &home,
    );
    assert_ne!(d, "allow", "long-form must stay refused; reason={r}");
}

#[test]
fn control_long_target_directory_separate_arg_is_not_allowed() {
    let home = unique_home("long-sep");
    let (d, r) = run(
        "for f in /etc; do ln --target-directory /tmp/L17 -s $f; done; echo x > /tmp/L17/etc/passwd",
        &home,
    );
    assert_ne!(d, "allow", "long-form must stay refused; reason={r}");
}

#[test]
fn control_direct_loop_write_into_etc_is_deny() {
    let home = unique_home("direct");
    let (d, r) = run("for f in /etc/hosts; do echo x > $f; done", &home);
    assert_eq!(
        d, "deny",
        "direct write to /etc/hosts must be Deny; reason={r}"
    );
}
