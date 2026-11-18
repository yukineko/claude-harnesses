#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Kills the mutant "remove the `=cd`/`=pushd` guard in
//! `detect::cwd::advance_cwd_and_rewrite`" (added by e8df66146).
//!
//! Why the existing `equals_cd_does_not_move_cwd_into_scratch`
//! (tests/zsh_equals_cmd_independent_20261118.rs) does not kill it: its
//! control `cd D && rm -rf build` is Allow only through the deletion-principle
//! path, which requires every segment head to be the LITERAL word `rm`/`cd`
//! (`worktree_rm_eligible`, src/detect/rm.rs). `=cd D && rm -rf build` is never
//! eligible, so with or without the guard it lands on the "confined to" Ask —
//! the guard only changes which directory the Ask names (observed 2026-11-18
//! with f6e00c445 debug binaries, guard present and removed).
//!
//! Shape used here: the session cwd is `$HOME`, and the relative operand
//! `.ssh/authorized_keys` names a home-level credential asset there but a
//! harmless scratch file under D. Observed in zsh 5.9 on macOS
//! (2026-11-18): `cd /; =cd /tmp; pwd` printed `/` and `whence -p cd` printed
//! `/usr/bin/cd`, so after `=cd D` the shell is still in `$HOME` and the `rm`
//! deletes `$HOME/.ssh/authorized_keys`. With the guard that is Deny; with the
//! guard removed the tracker places the operand in D and it became Allow
//! (main and subagent).
//!
//! DOES NOT PROVE: `=popd` (no shape distinguishing it was found: `popd` is
//! tracked as Unknown cwd either way); that every rule consuming the tracked
//! cwd honours the guard; the line-level redirect scans
//! (`segment_cwd_states`) separately.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/fixed_tmp.rs"]
mod fixed_tmp;

use std::process::{Command, Stdio};

struct Rm(std::path::PathBuf);
impl Drop for Rm {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_dir(base: &std::path::Path, tag: &str) -> Rm {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = base.join(format!(
        "bg-zsh-equals-cd-guard-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join(".ssh")).unwrap();
    std::fs::write(dir.join(".ssh/authorized_keys"), b"x\n").unwrap();
    Rm(dir)
}

/// Runs the hook with session cwd = `$HOME` = `home`.
fn run_as(command: &str, home: &std::path::Path, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "zsh-equals-cd-guard-20261118",
        "cwd": home,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", home)
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

/// `=cd D` / `=pushd D` leave the shell in `$HOME`, so the following relative
/// `rm -f .ssh/authorized_keys` deletes the home credential file: Deny, as the
/// bare `rm -f .ssh/authorized_keys` is. The control `cd D && rm -f
/// .ssh/authorized_keys` (a real directory change into a scratch dir under the
/// fixed temp root) must be Allow, so the difference is attributable to the
/// `=` alone.
#[test]
fn equals_cd_keeps_relative_operand_in_home() {
    let home = unique_dir(&std::env::temp_dir(), "home");
    let scratch = unique_dir(&fixed_tmp::canonical_fixed_temp_root(), "scratch");
    let d = scratch.0.display();
    let control = format!("cd {d} && rm -f .ssh/authorized_keys");
    let probes = [
        "rm -f .ssh/authorized_keys".to_string(),
        format!("=cd {d} && rm -f .ssh/authorized_keys"),
        format!("=cd {d}; rm -f .ssh/authorized_keys"),
        format!("=pushd {d} && rm -f .ssh/authorized_keys"),
        format!("=cd {d} && unlink .ssh/authorized_keys"),
    ];
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (dec, r) = run_as(&control, &home.0, subagent);
        if dec != "allow" {
            bad.push(format!(
                "CONTROL subagent={subagent} {control:?} => {dec} (want allow) ({r})"
            ));
        }
        for probe in &probes {
            let (dec, r) = run_as(probe, &home.0, subagent);
            if dec != "deny" {
                bad.push(format!(
                    "subagent={subagent} {probe:?} => {dec} (want deny) ({r})"
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
