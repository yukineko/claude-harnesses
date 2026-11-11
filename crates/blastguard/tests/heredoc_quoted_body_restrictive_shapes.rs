// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Restrictive-shape guard for backlog 1fd5a731.
//!
//! The fix strips a QUOTED-delimiter here-document body from the redirect
//! scans only for a narrow shape (reader cat/tee/`git commit -F -`, no pipe,
//! no `;`/`&` chain, closed body, only blank/comment lines after it). Every
//! shape OUTSIDE that must keep reading the body, so the backtick prose
//! `=> \`Err\`` in it is still judged (and the verdict stays NOT allow).
//!
//! Each test also runs a control: the same shape with a body WITHOUT the
//! `>` prose. Where the control is judged differently from the prose body, the
//! body provably reached the redirect scan (the test is not vacuous).
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.
//!
//! Re-anchored per the user ruling of 2026-11-08 (GitHub #1602): a quoted
//! heredoc body is judged by its target language, and a `.rs` target that is
//! not run in the same command is data (Allow when the later commands are
//! read-only). The shapes this file guards are therefore run against targets
//! the ruling still judges as shell: a `.sh` file and an extension-less file.
//! Shapes that must stay non-Allow for a `.rs` target as well (unquoted
//! delimiter, unterminated body) keep the `.rs` target in addition.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn fresh_project(tag: &str, target: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-heredoc-restrictive-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("crates/overwatch/src")).expect("mkdir project");
    assert!(!dir.join(target).exists(), "target must not exist");
    dir
}

fn verdict(cwd: &std::path::Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

/// A `.rs` target: data under the #1602 ruling unless the shape itself keeps
/// it non-Allow (unquoted delimiter, unterminated body).
const RS_TARGET: &str = "crates/overwatch/src/presence.rs";

/// Targets the #1602 ruling still judges as shell.
const SHELL_TARGETS: [&str; 2] = [
    "crates/overwatch/src/presence.sh",
    "crates/overwatch/src/presence",
];

/// The shell targets plus the `.rs` target, for shapes that must be non-Allow
/// whatever the target language.
const ALL_TARGETS: [&str; 3] = [
    "crates/overwatch/src/presence.sh",
    "crates/overwatch/src/presence",
    RS_TARGET,
];

/// A per-target tag so every target gets its own temp project.
fn target_tag(tag: &str, target: &str) -> String {
    let leaf = target
        .rsplit('/')
        .next()
        .unwrap_or(target)
        .replace('.', "-");
    format!("{tag}-{leaf}")
}

const PROSE_BODY: &str = "//! Presence ledger.\n\
/// Reads the ledger.\n\
/// corrupt => `Err`\n\
pub fn read() {}";

const PLAIN_BODY: &str = "//! Presence ledger.\n\
/// Reads the ledger.\n\
/// corrupt gives `Err`\n\
pub fn read() {}";

fn is_blocking(d: &str) -> bool {
    d == "deny" || d == "ask"
}

/// Assert the prose-body command is NOT allowed. Returns the control verdict
/// so the caller can assert whether the body changed the decision.
fn assert_prose_not_allowed(
    tag: &str,
    target: &str,
    shape: impl Fn(&str) -> String,
) -> ((String, String), (String, String)) {
    let proj = fresh_project(tag, target);
    let prose = verdict(&proj, &shape(PROSE_BODY));
    let control = verdict(&proj, &shape(PLAIN_BODY));
    assert!(
        is_blocking(&prose.0),
        "shape `{tag}` ALLOWED the backtick-prose body: {}: {}\n(control: {}: {})",
        prose.0,
        prose.1,
        control.0,
        control.1
    );
    (prose, control)
}

/// The prose body must have been judged differently from the control, i.e. the
/// body text reached the redirect scan.
fn assert_body_was_read(tag: &str, prose: &(String, String), control: &(String, String)) {
    assert_ne!(
        prose, control,
        "shape `{tag}`: body prose changed nothing versus the plain body, so \
         this test does not show the body being read: {}: {}",
        prose.0, prose.1
    );
}

#[test]
fn unquoted_delimiter_keeps_reading_body() {
    // Unquoted `<<EOF`: the shell expands the body, so it must stay scanned
    // whatever the target language (the #1602 ruling covers quoted bodies).
    for target in ALL_TARGETS {
        let tag = target_tag("unquoted", target);
        let (prose, control) =
            assert_prose_not_allowed(&tag, target, |b| format!("cat > {target} <<EOF\n{b}\nEOF"));
        assert_body_was_read(&tag, &prose, &control);
    }
}

#[test]
fn trailing_command_after_heredoc_keeps_reading_body() {
    for target in SHELL_TARGETS {
        let tag = target_tag("trailing", target);
        let (prose, control) = assert_prose_not_allowed(&tag, target, |b| {
            format!("cat > {target} <<'EOF'\n{b}\nEOF\necho done")
        });
        assert_body_was_read(&tag, &prose, &control);
    }
}

#[test]
fn reader_piped_into_sh_keeps_reading_body() {
    // The body is a program for `sh` here; it is not inert data.
    let (prose, control) = assert_prose_not_allowed("pipe-sh", RS_TARGET, |b| {
        format!("cat <<'EOF' | sh\n{b}\nEOF")
    });
    // Restrictive regardless of the body: piping text into a shell is
    // never allowed, so the control may match; only the NOT-allow part is
    // the claim for this shape.
    let _ = (prose, control);
}

#[test]
fn and_chain_keeps_reading_body() {
    for target in SHELL_TARGETS {
        let tag = target_tag("and-chain", target);
        let (prose, control) = assert_prose_not_allowed(&tag, target, |b| {
            format!("cat > {target} <<'EOF'\n{b}\nEOF\n") + "&& echo done"
        });
        assert_body_was_read(&tag, &prose, &control);
    }
}

#[test]
fn unterminated_body_keeps_reading_body() {
    // An unterminated body is not closed data, so it must stay non-Allow
    // whatever the target language.
    for target in ALL_TARGETS {
        let tag = target_tag("unterminated", target);
        let (prose, control) =
            assert_prose_not_allowed(&tag, target, |b| format!("cat > {target} <<'EOF'\n{b}"));
        assert_body_was_read(&tag, &prose, &control);
    }
}

#[test]
fn python3_reader_keeps_reading_body() {
    // The body is Python source, executed. Restrictive regardless of the body.
    let (prose, control) = assert_prose_not_allowed("python3", RS_TARGET, |b| {
        format!("python3 - <<'EOF'\n{b}\nEOF")
    });
    let _ = (prose, control);
}
