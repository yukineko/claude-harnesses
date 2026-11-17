#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin (2026-11-18): `rsync --delete` whose DESTINATION is the home
//! directory itself or an irreplaceable home-level asset (`~/.ssh`) is Allow.
//!
//! `rsync -a --delete SRC/ DEST/` removes every file in DEST that SRC lacks,
//! so `rsync -a --delete /tmp/empty/ ~/` empties `$HOME` (dotfiles included)
//! and `rsync -a --delete /tmp/empty/ ~/.ssh/` deletes the SSH keys.
//!
//! Expected verdict, and where it comes from:
//!   * `src/detect/ast/SUBSET.md` ruling 3: "System-harm targets are always
//!     Deny." Table C's System row is `harm_mark::harms_system`, whose doc
//!     (`src/detect/harm_mark.rs`) names "the home directory itself, or an
//!     irreplaceable home-level asset (`~/.ssh`, `~/.gnupg`, a home dotfile)"
//!     as system harm, with no safe-root escape for the home checks.
//!   * user ruling 2026-11-07 (memory `system-harm-always-deny`):
//!     "system-harming targets (system dirs, $HOME-level config, …) resolve
//!     to Deny regardless of the command shape that carries them".
//!   * the legacy layer already models this exact command: `rsync` with a
//!     `--del*` flag judges its destination (`src/detect/system_paths.rs`,
//!     `rsync_mutated`), but only against `scope::is_inside_system_dir`, so
//!     `rsync -a --delete /tmp/empty/ /usr/` is a system-harm Deny (control
//!     below) while the home destinations are not judged at all.
//!   * the same destruction through `rm` is Deny today (`rm -rf ~`,
//!     `rm -rf ~/.ssh`; controls below).
//!
//! Note: rsync is NOT a Table B row, so SUBSET.md's "unknown program →
//! Allow" would answer Allow if the legacy rsync arm did not exist. This
//! file pins only the destinations `harms_system` classifies as system harm;
//! a non-asset home subdirectory (`~/Documents/`) is deliberately NOT pinned
//! (that is a spec gap, reported separately, not a RED).
//!
//! Observed at origin/main 1dfa60681 (blastguard 0.2.699), debug binary,
//! hermetic fixture HOME, main and subagent mode: every `rsync_*` case below
//! is `allow` in both modes.
//!
//! Content rule: no real destructive payloads. Every path is a sentinel; the
//! hook only JUDGES the command text, nothing is executed.
//!
//! Harness: as `s3_home_tilde_branches_20261118.rs` (env_clear, fixture
//! `HOME`, `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT=cli`, main + subagent).
//!
//! DOES NOT PROVE: which rule produces the Deny once fixed; `--delete-*`
//! variants other than `--delete`; `--remove-source-files` on home sources;
//! remote (`host:`) destinations; anything about `~/Documents/`.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "rsync-home-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::create_dir_all(home.join("build")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base, home }
}

/// Run the hook once; `(decision, reason)`. A non-zero exit panics before an
/// empty stdout can be read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "rsync-home-20261118",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn deny_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

fn allow_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

// ---------------------------------------------------------------- controls

/// Control: the legacy rsync arm denies a system-directory destination.
/// DOES NOT PROVE: anything about home destinations.
#[test]
fn control_rsync_delete_into_usr_denies() {
    let fx = fixture("usr");
    deny_at(&fx, &fx.proj(), "rsync -a --delete /tmp/empty/ /usr/");
}

/// Control: the same home destruction through `rm` is Deny today.
/// DOES NOT PROVE: that rsync and rm share a rule.
#[test]
fn control_rm_rf_home_and_ssh_deny() {
    let fx = fixture("rm");
    deny_at(&fx, &fx.proj(), "rm -rf ~");
    deny_at(&fx, &fx.proj(), "rm -rf ~/.ssh");
}

/// Control: rsync WITHOUT a delete flag into a home subdirectory stays Allow
/// (it only adds / overwrites files; not pinned as harm here).
/// DOES NOT PROVE: that the fix leaves every non-delete rsync Allow.
#[test]
fn control_rsync_without_delete_into_home_subdir_allows() {
    let fx = fixture("nodel");
    allow_at(&fx, &fx.proj(), "rsync -a /tmp/empty/ ~/bg-sentinel-dir/");
}

// --------------------------------------------------------------------- RED

#[test]
#[ignore = "RED: rsync --delete into ~/ (empties $HOME) is Allow; harms_system target → Deny (ruling 3)"]
fn rsync_delete_into_tilde_home_denies() {
    let fx = fixture("tilde");
    deny_at(&fx, &fx.proj(), "rsync -a --delete /tmp/empty/ ~/");
}

#[test]
#[ignore = "RED: rsync --delete into \"$HOME\" is Allow; harms_system target → Deny (ruling 3, ruling 12)"]
fn rsync_delete_into_dollar_home_denies() {
    let fx = fixture("dollar");
    deny_at(&fx, &fx.proj(), "rsync -a --delete /tmp/empty/ \"$HOME\"");
}

#[test]
#[ignore = "RED: rsync --delete into the absolute home path is Allow; harms_system target → Deny (ruling 3)"]
fn rsync_delete_into_absolute_home_denies() {
    let fx = fixture("abs");
    let cmd = format!("rsync -a --delete /tmp/empty/ {}/", fx.home.display());
    deny_at(&fx, &fx.proj(), &cmd);
}

#[test]
#[ignore = "RED: rsync --delete into ~/.ssh/ (deletes SSH keys) is Allow; irreplaceable home asset → Deny (ruling 3)"]
fn rsync_delete_into_ssh_denies() {
    let fx = fixture("ssh");
    deny_at(&fx, &fx.proj(), "rsync -a --delete /tmp/empty/ ~/.ssh/");
}

/// Session cwd = `$HOME`: `./` is the home directory itself.
#[test]
#[ignore = "RED: with cwd = $HOME, rsync --delete build/ ./ is Allow; it empties $HOME → Deny (ruling 3)"]
fn rsync_delete_into_dot_with_cwd_home_denies() {
    let fx = fixture("dot");
    let home = fx.home.clone();
    deny_at(&fx, &home, "rsync -a --delete build/ ./");
}
