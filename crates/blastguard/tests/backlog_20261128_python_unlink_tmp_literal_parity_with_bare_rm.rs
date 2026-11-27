// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (salvaged 2026-11-28 from the unmerged worktree bg-walk-clean-6bbf,
//! where it was written 2026-11-08 as `parity_os_unlink_tmp_literal_matches_bare_rm`
//! in inert_literal_mention_controls_20261107.rs; measured here on main
//! a4c078f2d, blastguard 0.2.976).
//!
//! docs/specs/blastguard.md (filesystem 破壊): `os.unlink(p)` is judged as
//! `rm p`. A bare `rm /tmp/x.tmp` is Allow, but the python stdin-heredoc
//! `os.unlink('/tmp/x.tmp')` is Deny ("... it runs `unlink(`, which deletes
//! files or directories outright"), so the two spellings of the same effect
//! get different verdicts.
//!
//! Readings: (a) defect/FP — the literal-path call must get the bare command's
//! verdict (parity rule 2026-11-08); (b) intended — a python program that
//! calls unlink stays stricter than bare rm. The spec sentence supports (a);
//! (b) is not ruled out here.
//!
//! Controls (GREEN): the bare `rm /tmp/x.tmp` is Allow; the same call on
//! `/etc/hosts` is Deny in both spellings.
//!
//! DOES NOT PROVE: subagent mode; paths other than /tmp/x.tmp; `os.remove`,
//! `Path.unlink` or `-c` spellings.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn decision(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let d = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let r = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (d, r)
}

#[test]
#[ignore = "RED: python heredoc os.unlink('/tmp/x.tmp') is Deny while bare `rm /tmp/x.tmp` is Allow (spec: os.unlink(p) == rm p)"]
fn parity_os_unlink_tmp_literal_matches_bare_rm() {
    let bare = decision("rm /tmp/x.tmp");
    let py = decision("python3 - <<'PY'\nimport os\nos.unlink('/tmp/x.tmp')\nPY");
    assert_eq!(
        py.0, bare.0,
        "os.unlink('/tmp/x.tmp') must get the bare `rm /tmp/x.tmp` verdict: \
         python={py:?} bare={bare:?}"
    );
}

#[test]
fn control_bare_rm_tmp_is_allow_and_etc_hosts_is_deny_both_ways() {
    assert_eq!(decision("rm /tmp/x.tmp").0, "allow");
    assert_eq!(decision("rm /etc/hosts").0, "deny");
    let py = decision("python3 - <<'PY'\nimport os\nos.unlink('/etc/hosts')\nPY");
    assert_eq!(py.0, "deny", "{py:?}");
}
