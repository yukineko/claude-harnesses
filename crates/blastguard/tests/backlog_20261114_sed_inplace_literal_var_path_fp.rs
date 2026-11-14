#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14, blastguard 0.2.575, SUBAGENT mode
//! i.e. payload has agent_id): `S=/ABS/scratch; sed -i '' 's/a/b/' $S/full.py`
//! was denied: "`sed` rewrites in place `$S/full.py`, whose path depends on an
//! expansion blastguard cannot resolve — it cannot tell whether that lands
//! inside a system directory, so it refuses to guess". S is a literal assigned
//! earlier in the SAME command.
//!
//! Two readings: (1) real FP: a variable assigned a literal earlier in the same
//! command is resolvable, so the path is statically known (ABS/full.py, not a
//! system dir); (2) intended: variable-built in-place targets are always refused.
//! Tell them apart: the control with the plain literal path is allow, and other
//! gates in this repo resolve same-command literal assignments (e.g. the
//! `L=/etc/hosts; rm -f $L` deny is reached by resolving L), so refusing to
//! resolve S is inconsistent -> reading (1).
//!
//! Asserted: NOT deny in subagent mode (expected "allow", same as the control
//! with the literal path). ABS is the test's temp `wt` dir (also the cwd).
//!
//! DOES NOT PROVE: main mode, variables assigned from command substitutions or
//! env, S pointing at a system dir (must stay Deny), other in-place editors.
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
        "bg-varmultisub-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "var-multi-statement-subst-20261114",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
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

fn seed(home: &Home) -> String {
    let abs = home.0.join("wt");
    std::fs::write(abs.join("full.py"), "a\n").unwrap();
    abs.display().to_string()
}

#[test]
#[ignore = "RED: 2026-11-14 sed -i on $S/file with S assigned a literal in the same command is denied as unresolvable — remove the ignore when fixed"]
fn sed_inplace_on_same_command_literal_var_path_is_not_denied() {
    let home = unique_home("red");
    let abs = seed(&home);
    let cmd = format!("S={abs}; sed -i '' 's/a/b/' $S/full.py");
    let (d, r) = run_as(&cmd, &home, true);
    assert_eq!(d, "allow", "expected allow; cmd={cmd:?} reason={r}");
}

#[test]
fn control_sed_inplace_on_literal_path_is_allow() {
    let home = unique_home("ctl");
    let abs = seed(&home);
    let cmd = format!("sed -i '' 's/a/b/' {abs}/full.py");
    let (d, r) = run_as(&cmd, &home, true);
    assert_eq!(
        d, "allow",
        "control must stay allow; cmd={cmd:?} reason={r}"
    );
}
