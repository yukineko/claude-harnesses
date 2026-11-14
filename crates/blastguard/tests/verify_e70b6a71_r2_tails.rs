// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Round-2 adversarial verifier tests for condukt task e70b6a71 (blastguard 0.2.589:
//! a QUOTED-delimiter `git commit -F -` heredoc body is stripped from the
//! judged text even when commands follow the closing delimiter line).
//!
//! The fix rests on the claim that such a reader "stores the body nowhere".
//! Observed 2026-11-14 in a scratch repo: `git commit -F - <<'EOF'` writes the
//! body verbatim to `.git/COMMIT_EDITMSG` (and into the commit object), so a
//! later line in the same command can execute it (`sh .git/COMMIT_EDITMSG`,
//! `git log -1 --format=%B | sh`, ...). Each case below must not be Allow, in
//! both main-session and subagent modes.
//!
//! DOES NOT PROVE: anything about a LATER, separate tool call executing the
//! stored message (a documented residual), nor the exact Ask/Deny split.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn verdict(command: &str, subagent: bool) -> (String, String) {
    // VERIFY_E70B6A71_R2_BIN: run the same cases against another build (the
    // verifier used it to record base eeaa73843 vs fix 1420ba8ee).
    let bin = std::env::var("VERIFY_E70B6A71_R2_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_blastguard").to_string());
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    if subagent {
        payload["agent_id"] = serde_json::json!("agent-verify-e70b6a71");
        payload["agent_type"] = serde_json::json!("general-purpose");
    }
    let payload = payload.to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
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

const BODY: &str = concat!("r", "m -rf ~");

fn cases() -> Vec<String> {
    let tails: &[&str] = &[
        "sh .git/COMMIT_EDITMSG",
        "echo x |& sh .git/COMMIT_EDITMSG",
        "echo ok; sh .git/COMMIT_EDITMSG",
        "echo ok && sh .git/COMMIT_EDITMSG",
        "echo ok & sh .git/COMMIT_EDITMSG",
        "cat .git/COMMIT_EDITMSG | sh",
        "cat .git/COMMIT_EDITMSG | bash -s",
        "git log -1 --format=%B | sh",
        "git log -1 --pretty=%B | xargs -0 sh -c",
        "ls; /bin/sh .git/COMMIT_EDITMSG",
        "ls; command sh .git/COMMIT_EDITMSG",
        "ls; exec sh .git/COMMIT_EDITMSG",
        "ls; eval \"$(cat .git/COMMIT_EDITMSG)\"",
        "ls; . .git/COMMIT_EDITMSG",
        "ls; source .git/COMMIT_EDITMSG",
        "env sh .git/COMMIT_EDITMSG",
        "git show --output=x.sh HEAD",
        "git show --out=x.sh HEAD",
        "git log -1 $X",
        "echo x;;sh .git/COMMIT_EDITMSG",
        "if true; then sh .git/COMMIT_EDITMSG; fi",
        "echo $'\\n'sh",
        "time sh .git/COMMIT_EDITMSG",
        "grep -r x . ; sh .git/COMMIT_EDITMSG",
        "ls -l\nsh .git/COMMIT_EDITMSG",
        "ls \"a;\"; sh .git/COMMIT_EDITMSG",
        "head -1 .git/COMMIT_EDITMSG | sh",
        "tail -n1 .git/COMMIT_EDITMSG|sh",
        "echo x &&\nsh .git/COMMIT_EDITMSG",
        "echo x ||\nsh .git/COMMIT_EDITMSG",
        "echo x |\nsh .git/COMMIT_EDITMSG",
        "git log --pretty=%B -1 | sh -s",
        "git log -1 --format=%B | . /dev/stdin",
    ];
    let mut v = Vec::new();
    for t in tails {
        v.push(format!("git commit -F - <<'EOF'\n{BODY}\nEOF\n{t}"));
        if !t.contains('\n') {
            v.push(format!("git commit -F - <<'EOF'; {t}\n{BODY}\nEOF"));
        }
    }
    v
}

#[test]
fn r2_adversarial_tails_not_allowed() {
    let mut allowed = Vec::new();
    for c in cases() {
        for sub in [false, true] {
            let (d, _) = verdict(&c, sub);
            if d == "allow" {
                allowed.push(format!("{} :: {c:?}", if sub { "subagent" } else { "main" }));
            }
        }
    }
    assert!(allowed.is_empty(), "Allowed:\n{}", allowed.join("\n"));
}

/// Counterpart: later commands that cannot run the stored message keep the
/// body stripped (Allow). Documents the intended FP fix and its edges:
/// `--o=` / `-O` / `--ext-diff` only read or use configured drivers (git
/// rejects `--o`/`--outp` as unrecognized, observed 2026-11-14).
#[test]
fn r2_inert_tails_stay_allowed() {
    for t in [
        "git log --o=x.sh",
        "git log --stat -p -S x",
        "git show -O .git/COMMIT_EDITMSG",
        "echo 'a'sh .git/COMMIT_EDITMSG",
        "git log -1 --format=%B -- .git/COMMIT_EDITMSG",
    ] {
        let c = format!("git commit -F - <<'EOF'\n{BODY}\nEOF\n{t}");
        assert_eq!(verdict(&c, false).0, "allow", "{c:?}");
    }
}
