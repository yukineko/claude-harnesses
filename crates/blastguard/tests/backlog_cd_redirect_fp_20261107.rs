// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of a blastguard false positive observed 2026-11-07 in a subagent
//! session (32935915): a heredoc writing a Rust test file,
//!
//! ```text
//! cd /Users/yuki/src/.harness-worktrees/session-32935915 && cat > crates/condukt/tests/pass_cache_20261107.rs <<'EOF'
//! ...Rust test source...
//! EOF
//! . "$HOME/.cargo/env"; cargo test -p condukt ... | grep ... | head -30
//! ```
//!
//! was refused with (verbatim, PreToolUse:Bash, inside a subagent):
//! "'> crates/condukt/tests/pass_cache_20261107.rs' is a RELATIVE redirect after
//! an earlier cd/pushd/popd whose target directory blastguard could not
//! statically resolve — it cannot tell whether this lands in a system directory
//! or on a protected gate/config path, and refuses to guess".
//! The same shape writing `crates/donegate/tests/pass_cache_20261107.rs` a few
//! calls earlier was allowed.
//!
//! MEASURED (this file's probes, HEAD 91531ac1, 2026-11-07), by line-level
//! delta-debugging of the exact command (the quoted body is shell DATA):
//!  * the cd target is a literal absolute path; it is resolvable. Replacing the
//!    heredoc body with `x` (everything else byte-identical) is Allow.
//!  * the verdict flips on the BODY, but only when a shell line FOLLOWS the
//!    closing delimiter: one body line `    for p in
//!    after.iter().filter(|p| !before.contains(p)) {` + a trailing `ls` is
//!    enough; the same body with nothing after `EOF` is Allow.
//!  * the shortest body is a single unbalanced delimiter: `)`, `(` or a lone
//!    backtick, with any trailing line (`cd /tmp/wt && cat > f <<'EOF'\n)\nEOF\nls`).
//!    `}` and `'` alone do not trigger it.
//!  * an unquoted `<<EOF` with the same body is also non-Allow (not asserted:
//!    there the shell really does parse the body).
//!
//! So the defect is attribution: a quoted-heredoc body that is never run by the
//! shell makes the preceding literal `cd <abs path>` count as "could not
//! statically resolve" once a later command line exists (the body is scanned in
//! that case on purpose — README.ja.md "here-document の後のコマンドがあれば
//! 従来通り本文も走査する", pinned by
//! heredoc_quoted_body_restrictive_shapes.rs::trailing_command_after_heredoc_
//! keeps_reading_body for a body containing a real `>` redirect). That scan
//! should still find a real redirect in the body (control below) but must not
//! convert a benign unbalanced `)` into "the cd target is unknown".
//!
//! NOT covered here (deliberately; reported separately): a python stdin
//! heredoc whose body has `>` immediately before a closing quote
//! (`print('a >')`) is Ask/Deny by the documented R2 ruling in
//! interpreter.rs `interpreter_code_verdict` ("may be an operator or part of a
//! string, but blastguard cannot show that it is not a redirect"); Allow there
//! would contradict that ruling, so no RED test is written for it.
//!
//! Payloads carry `agent_id` and `cwd` so blastguard judges as in a subagent
//! (the observed context, where an Ask is hardened to Deny).
//! Assertions are verdicts only, never message text.
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.
//! Do NOT weaken a test to make it pass; remove the `#[ignore]` on a RED test
//! when fixed.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The exact command from the subagent transcript (tool_use
/// toolu_01K7Jt1f6K78AXKJnxLhHkQH), byte for byte.
const EXACT: &str = include_str!("support/cd_redirect_fp_20261107_exact.sh");

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blastguard-cdredir-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Decision (`allow` / `ask` / `deny`) for `command` judged as a subagent Bash
/// call from `cwd`. Empty stdout is Allow.
fn decision(cwd: &Path, command: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "agent_id": "test-subagent",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
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
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string()
}

// ---------------------------------------------------------------- RED targets

/// TARGET (RED at 91531ac1): the exact observed command is judged like the same
/// command whose quoted heredoc body is replaced by `x` (which is Allow).
#[test]
#[ignore = "backlog 547cf2f6: open defect, remove ignore when fixed"]
fn exact_observed_command_is_judged_like_its_neutral_body_variant() {
    let (head, rest) = EXACT.split_once("<<'EOF'\n").expect("opener");
    let (_body, tail) = rest.rsplit_once("\nEOF").expect("terminator");
    let neutral = format!("{head}<<'EOF'\nx\nEOF{tail}");
    let base = fresh_dir("exact");
    let neutral_d = decision(&base, &neutral);
    assert_eq!(
        neutral_d, "allow",
        "premise: the neutral variant must be Allow (literal cd, non-protected target)"
    );
    let d = decision(&base, EXACT);
    assert_eq!(d, "allow", "quoted heredoc body is data; neutral is allow");
}

/// TARGET (RED at 91531ac1): minimal shapes — literal absolute cd into a
/// non-protected directory, a relative non-protected redirect target, a quoted
/// body that is a single unbalanced delimiter, one trailing command line.
#[test]
#[ignore = "backlog 547cf2f6: open defect, remove ignore when fixed"]
fn unbalanced_delimiter_in_quoted_body_does_not_unresolve_a_literal_cd() {
    let base = fresh_dir("min");
    let wt = base.display();
    for (i, body) in [
        ")",
        "(",
        "x)",
        "    for p in after.iter().filter(|p| !before.contains(p)) {",
    ]
    .iter()
    .enumerate()
    {
        let cmd = format!("cd {wt} && cat > crates/condukt/tests/x.rs <<'EOF'\n{body}\nEOF\nls");
        let d = decision(&base, &cmd);
        assert_eq!(d, "allow", "[min{i}] body {body:?}: quoted body is data");
    }
}

/// TARGET (RED at 91531ac1, separate defect, user ruling 2026-11-07): a lone
/// backtick as the whole quoted body, one trailing line. The Ask here comes
/// from the documented body-as-code scan (README.ja.md: a command after the
/// heredoc keeps the body scanned), not from cd resolution; fixing it needs a
/// ruling that changes that documented scan.
#[test]
#[ignore = "backlog PLACEHOLDER_BT: open defect, remove ignore when fixed"]
fn lone_backtick_in_quoted_body_with_trailing_line_is_allow() {
    let base = fresh_dir("bt");
    let wt = base.display();
    let cmd = format!("cd {wt} && cat > crates/condukt/tests/x.rs <<'EOF'\n`\nEOF\nls");
    assert_eq!(decision(&base, &cmd), "allow", "quoted body is data");
}

// ------------------------------------------------- controls (pass at 91531ac1)

/// CONTROL: an unresolvable cd target followed by a relative redirect is still
/// not Allow (quoted body or not).
#[test]
fn control_unresolvable_cd_then_relative_redirect_is_not_allow() {
    let base = fresh_dir("c-var");
    let d = decision(&base, "cd \"$X\" && cat > foo <<'EOF'\nx\nEOF");
    assert_ne!(
        d, "allow",
        "cd \"$X\" is unresolvable; the redirect must not be allowed"
    );
    let d2 = decision(&base, "cd \"$X\" && cat > foo <<'EOF'\n)\nEOF\nls");
    assert_ne!(
        d2, "allow",
        "cd \"$X\" with a trailing line must stay non-Allow"
    );
}

/// CONTROL: a system directory as cd target is not Allow.
#[test]
fn control_cd_into_system_dir_then_redirect_is_not_allow() {
    let base = fresh_dir("c-etc");
    let d = decision(&base, "cd /etc && cat > passwd <<'EOF'\nx\nEOF");
    assert_ne!(d, "allow", "writing /etc/passwd must not be allowed");
    let d2 = decision(&base, "cd /etc && cat > passwd <<'EOF'\n)\nEOF\nls");
    assert_ne!(
        d2, "allow",
        "writing /etc/passwd with a trailing line must not be allowed"
    );
}

/// CONTROL: the neutral-body variant of the observed shape (literal cd into a
/// non-protected dir) is Allow — the allowed donegate sibling's shape.
#[test]
fn control_literal_cd_neutral_body_with_trailing_line_is_allow() {
    let base = fresh_dir("c-ok");
    let wt = base.display();
    let cmd = format!("cd {wt} && cat > crates/donegate/tests/x.rs <<'EOF'\nx\nEOF\nls");
    assert_eq!(decision(&base, &cmd), "allow");
}

/// CONTROL: a real redirect inside a quoted body that is followed by a command
/// line is still scanned (README.ja.md; heredoc_quoted_body_restrictive_shapes)
/// — it targets /etc/passwd here, so it must not be Allow.
#[test]
fn control_real_redirect_in_body_with_trailing_line_is_not_allow() {
    let base = fresh_dir("c-body");
    let wt = base.display();
    let cmd = format!("cd {wt} && cat > f <<'EOF'\n) > /etc/passwd\nEOF\nls");
    assert_ne!(decision(&base, &cmd), "allow");
}
