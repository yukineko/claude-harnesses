// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! **A gate does not get to override the same instruction twice.**
//!
//! Operator ruling 2026-09-18, verbatim:
//!
//! > 「ユーザの指示を2度やぶるgateはいらない。2度目はaskせよ」
//! > 「他のgateでも同じルールを適用して」
//! > 「askのだせないgateもゴミ」
//!
//! Asked whether headless/agent runs should be exempt, the operator ruled
//! 「全 gate で 2度目は無条件に通す」 and scoped identity to 「同一セッション内のみ」.
//!
//! These are end-to-end tests against the real binary rather than unit tests on
//! `downgrade_on_repeat`, because the thing being pinned is the *emitted*
//! verdict: the downgrade sits after `Decision::hardened()`, and a unit test one
//! layer up would pass even if hardening folded the `Ask` straight back into a
//! `Deny` on the way out. The bug this guards against is only visible on stdout.
//!
//! Each test gets its own `HOME`, so the repeat ledger starts empty and tests
//! cannot see each other's markers.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A private `HOME` for one test, so the repeat ledger is isolated.
fn scratch_home(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "bg-repeat-{}-{}-{:?}",
        name,
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn payload(command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string()
}

/// Feed one payload to the real binary and return the `permissionDecision`
/// (`"allow"` when it printed nothing).
fn verdict(home: &PathBuf, session: &str, command: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut child = Command::new(bin)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload(command).as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no permissionDecision in {stdout}"))
}

/// THE RULE. Measured RED on the pre-wiring binary (all three of these answered
/// `deny` twice); this pins the `deny` → `ask` downgrade.
#[test]
fn a_second_identical_refusal_becomes_an_ask() {
    for (name, command) in [
        ("rmrf", "rm -rf /mnt/c/tmp/aegis-worktrees/some-worktree"),
        ("reset", "git reset --hard"),
        ("checkout", "git checkout -- ."),
    ] {
        let home = scratch_home(name);
        assert_eq!(
            verdict(&home, "sessA", command),
            "deny",
            "{command}: the FIRST refusal must stand at full strength — the user \
             has not seen the reason yet"
        );
        assert_eq!(
            verdict(&home, "sessA", command),
            "ask",
            "{command}: the user has now read the reason and asked again, so the \
             gate must hand the decision back instead of refusing a second time"
        );
    }
}

/// CONTROL — must hold before and after. Waiving one effect must not waive a
/// DIFFERENT one; otherwise the first repeat would silence the whole gate.
#[test]
fn waiving_one_effect_does_not_waive_another() {
    let home = scratch_home("other-effect");
    let a = "rm -rf /mnt/c/tmp/aegis-worktrees/some-worktree";
    let b = "git reset --hard";
    assert_eq!(verdict(&home, "sessA", a), "deny");
    assert_eq!(verdict(&home, "sessA", a), "ask", "a is now waived");
    assert_eq!(
        verdict(&home, "sessA", b),
        "deny",
        "b is a finding the user has NOT seen; it must block at full strength \
         even though a was just waived"
    );
}

/// CONTROL for 「同一セッション内のみ」. A waiver earned in one session must not
/// silence the next one.
#[test]
fn a_new_session_starts_back_at_a_full_strength_refusal() {
    let home = scratch_home("new-session");
    let command = "git reset --hard";
    assert_eq!(verdict(&home, "sessA", command), "deny");
    assert_eq!(verdict(&home, "sessA", command), "ask");
    assert_eq!(
        verdict(&home, "sessB", command),
        "deny",
        "a new session must not inherit the previous session's waiver"
    );
}

/// CONTROL, anti-vacuity. If the downgrade were unconditional rather than
/// keyed on a ledger, every first refusal would already be an `ask` and the
/// test above would still pass. Pointing a second run at a DIFFERENT `HOME`
/// (an empty ledger) must bring the `deny` back.
#[test]
fn a_different_ledger_brings_the_refusal_back() {
    let command = "git reset --hard";
    let home1 = scratch_home("ledger-1");
    assert_eq!(verdict(&home1, "sessA", command), "deny");
    assert_eq!(verdict(&home1, "sessA", command), "ask");
    let home2 = scratch_home("ledger-2");
    assert_eq!(
        verdict(&home2, "sessA", command),
        "deny",
        "the downgrade must come from the ledger, not from the code path always \
         downgrading — an empty ledger must refuse at full strength"
    );
}

/// CLAUDE.md §3. With no session id the ledger cannot show this is a second
/// occurrence, so the refusal must stand — "I could not check" is not "the user
/// already saw this".
#[test]
fn without_a_session_id_the_refusal_never_downgrades() {
    let home = scratch_home("no-session");
    let command = "git reset --hard";
    for attempt in 1..=3 {
        assert_eq!(
            verdict(&home, "", command),
            "deny",
            "attempt {attempt}: an unattributable run must keep the deny"
        );
    }
}
