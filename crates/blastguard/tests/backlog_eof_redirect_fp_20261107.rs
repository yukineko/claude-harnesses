// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of a blastguard false positive observed 2026-11-07 in a subagent
//! session (32935915): a commit whose message is a QUOTED heredoc,
//!
//! ```text
//! cd /Users/yuki/src/.harness-worktrees/donegate-pass-cache; git add -A && git commit -q -F - <<'EOF'
//! donegate 0.1.83: pass cache ... (multi-line message)
//! ...
//! Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
//! EOF
//! git log --oneline -1
//! ```
//!
//! was refused with (verbatim, PreToolUse:Bash, inside a subagent, blastguard
//! 0.2.303): "'> EOF' is a RELATIVE redirect after an earlier cd/pushd/popd whose
//! target directory blastguard could not statically resolve — it cannot tell
//! whether this lands in a system directory or on a protected gate/config path,
//! and refuses to guess".
//!
//! MEASURED (this file's probes, HEAD 6d230618, 2026-11-07), by line-level
//! delta-debugging of the exact command:
//!  * dropping any single message line keeps the deny EXCEPT the
//!    `Co-Authored-By: ... <noreply@anthropic.com>` line; that line alone (with
//!    the opener and EOF) reproduces it. The trailing `git log` line is not
//!    needed.
//!  * the trigger is a body LINE THAT ENDS IN `>` immediately before the
//!    closing delimiter line: the scanner reads `>` + newline + `EOF` as the
//!    redirect `> EOF`. A body line `x>` is enough; `x >`, `<a@b.com>`,
//!    `anthropic.com>` all reproduce; `noreply@anthropic.com` (no `>`) does not.
//!  * the `cd` target is a literal absolute path (resolvable); the message about
//!    an unresolved cd is a mis-attribution of the bogus `> EOF` redirect.
//!  * it needs a SECOND command between the cd and the heredoc: `cd D; A && cat
//!    <<'EOF'`, `cd D; A; B <<'EOF'` deny; `cd D; cat <<'EOF'` and
//!    `cd D && A && git commit -F - <<'EOF'` are Allow. The README.ja.md scope
//!    for not reading a quoted body is "no `|` `;` `&`", so a chained command
//!    keeps the body scanned (documented, conservative); but the scan must not
//!    take the heredoc's own delimiter line for a redirect target.
//!
//! Payloads carry `agent_id` and `cwd` so blastguard judges as in a subagent
//! (the observed context, where an Ask is hardened to Deny). Assertions are
//! verdicts only, never message text. Written by a disinterested author
//! (CLAUDE.md §2(a)); `src/` untouched. Do NOT weaken a test to make it pass;
//! remove the `#[ignore]` on a RED test when fixed.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The exact command from the subagent transcript (tool_use
/// toolu_01RN5Q4tyMSh5eFL4x6JrzFP), byte for byte.
const EXACT: &str = include_str!("support/eof_redirect_fp_20261107_exact.sh");
/// The literal cd target in EXACT.
const EXACT_CD: &str = "/Users/yuki/src/.harness-worktrees/donegate-pass-cache";

fn fresh_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("blastguard-eofredir-{}-{tag}", std::process::id()));
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

/// TARGET (RED at 6d230618): the exact observed command. The cd target is a
/// literal path, the body is quoted (data); nothing in it writes anywhere.
#[test]
#[ignore = "backlog 0fccf1eb: open defect, remove ignore when fixed"]
fn exact_observed_commit_command_is_allow() {
    let base = fresh_dir("exact");
    let d = decision(&base, EXACT);
    assert_eq!(d, "allow", "quoted heredoc commit message is data");
}

/// TARGET (RED at 6d230618): the minimized shape. A second command after the
/// `cd`, then a quoted heredoc whose last body line ends in `>`.
#[test]
#[ignore = "backlog 0fccf1eb: open defect, remove ignore when fixed"]
fn body_line_ending_in_gt_does_not_make_delimiter_a_redirect_target() {
    let base = fresh_dir("min");
    let wt = base.display();
    for (i, cmd) in [
        format!("cd {wt}; true && cat <<'EOF'\nx>\nEOF"),
        format!("cd {wt}; ls; cat <<'EOF'\nx>\nEOF"),
        format!(
            "cd {wt}; git add -A && git commit -q -F - <<'EOF'\nCo-Authored-By: A <n@a.com>\nEOF"
        ),
    ]
    .iter()
    .enumerate()
    {
        let d = decision(&base, cmd);
        assert_eq!(d, "allow", "[min{i}] {cmd:?}: quoted body is data");
    }
}

// ------------------------------------------------- controls (pass at 6d230618)

/// CONTROL: an unresolvable cd target followed by a relative redirect is not
/// Allow (the shape the block message is about).
#[test]
fn control_unresolvable_cd_then_relative_redirect_is_not_allow() {
    let base = fresh_dir("c-var");
    let d = decision(&base, "cd \"$X\"; cat > f <<'EOF'\nx\nEOF");
    assert_ne!(
        d, "allow",
        "cd \"$X\" is unresolvable; redirect not allowed"
    );
}

/// CONTROL: an UNQUOTED heredoc body is run by the shell, so a command
/// substitution in it is real.
#[test]
fn control_unquoted_heredoc_command_substitution_is_not_allow() {
    let base = fresh_dir("c-unq");
    let wt = base.display();
    let d = decision(&base, &format!("cd {wt}; cat <<EOF\n$(rm -rf /)\nEOF"));
    assert_ne!(d, "allow", "unquoted heredoc body executes $(rm -rf /)");
}

/// CONTROL: a system directory as cd target with a redirect is not Allow, with
/// the same chained shape as the target.
#[test]
fn control_cd_into_system_dir_then_chained_redirect_is_not_allow() {
    let base = fresh_dir("c-etc");
    let d = decision(&base, "cd /etc; true && cat > passwd <<'EOF'\nx\nEOF");
    assert_ne!(d, "allow", "writing /etc/passwd must not be allowed");
}

/// CONTROL: the same commit with a neutral message (no `>` line end) is Allow.
#[test]
fn control_same_commit_neutral_message_is_allow() {
    let base = fresh_dir("c-ok");
    let cmd = EXACT
        .replace(EXACT_CD, &base.display().to_string())
        .replace(
            "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
            "neutral",
        );
    assert!(!cmd.contains("anthropic"), "premise: message made neutral");
    assert_eq!(decision(&base, &cmd), "allow");
}
