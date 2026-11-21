#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-open observed 2026-11-21 while writing the controls of
//! `backlog_20261121_unmodelled_cmd_redirect_target_fp.rs` (measured at
//! 3c792ee32, base 366eab917): a truncating write into `/etc/hosts` is Ask,
//! not Deny, in the MAIN session when an earlier segment of the line holds a
//! command that `relink::parse` reads as an unmodelled relinker (the word
//! `rsync` inside a jq filter string) and `relink::mentions` fires (a glob
//! metachar `[0]`). Verbatim reason:
//! "'> /etc/hosts': `jq '.tasks[0].title=\"fix rsync\"' in.json` may create
//! or rename an entry on this path and is not modelled, so blastguard cannot
//! tell which file the write really lands on".
//!
//! Without the jq segment, `echo x > /etc/hosts` is Deny (control below). The
//! relink layer's "cannot tell where it lands" Ask therefore REPLACES the
//! system-path Deny the spelled target already earns: whatever the relinker
//! does, the spelled landing `/etc/hosts` is a system file, so the verdict
//! can only be Deny (memory `system-harm-always-deny`: harmful targets are
//! Deny through any wrapper).
//!
//! Readings:
//! (a) defect (fail-open) — the strictest verdict must stand: an undetermined
//!     relink of a target whose spelled path is already system harm stays
//!     Deny.
//! (b) intended — Ask is the relink layer's answer and main-session Ask is a
//!     human prompt, so not a bypass.
//! How to tell: whether the spelled target alone is Deny (it is — the control)
//! — an extra unknown can only make the verdict stricter, never weaker.
//!
//! Written by an independent test author (not the implementer).
//!
//! DOES NOT PROVE: other system targets; subagent mode (Ask there is already
//! hardened to Deny); the reason text.

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
        "bg-relink-sys-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "relink-unmodelled-ask-masks-system-write-20261121",
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

#[test]
fn unmodelled_relinker_does_not_weaken_system_write_deny() {
    let home = unique_home("red");
    let c = "jq '.tasks[0].title=\"fix rsync\"' in.json; echo x > /etc/hosts";
    let (d, r) = run_main(c, &home);
    assert_eq!(d, "deny", "{c:?} => {d} ({r})");
}

#[test]
fn control_spelled_system_write_alone_is_deny() {
    let home = unique_home("ctl");
    let c = "echo x > /etc/hosts";
    let (d, r) = run_main(c, &home);
    assert_eq!(d, "deny", "{c:?} => {d} ({r})");
}
