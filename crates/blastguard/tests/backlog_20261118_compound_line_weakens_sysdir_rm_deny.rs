#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin (2026-11-18): when the session cwd lies under a system directory
//! (`/private/var/…`, `deletion::SYSTEM_PREFIXES`; e.g. a project in macOS
//! `/private/var/folders` with no `TMPDIR` in the hook env), a recursive `rm`
//! of a path inside the cwd is a Deny as a single command but only the
//! confined Ask once the line is compound — any `&&` / `;` prefix, with or
//! without a `cd`:
//!
//! ```text
//! rm -rf build                 → deny  "… `…/wt/build` is a system directory [system harm …]"
//! true && rm -rf build         → ask   "recursive rm (-r) is confined to …/wt — a safe root …"
//! echo x; rm -rf build         → ask   (same)
//! cd <abs wt> && rm -rf build  → ask   (same)
//! =cd D && rm -rf build        → ask   (same)
//! ```
//!
//! Which reason is right: SUBSET.md Table C. The System row
//! (`harm_mark::harms_system`) escapes via `Ctx::confined_root`, so a target
//! strictly inside the session's own project root is not that row; the next
//! matching row is **Project** (`SafeRoots::classify` Inside), whose Delete
//! cell is `legacy (deletion::judge_rm recoverability)`. `judge_rm` refuses
//! a system directory "even if a git work tree happens to contain it"
//! (`src/deletion.rs`, `SYSTEM_PREFIXES` doc) — the single-command Deny. A
//! no-op `true &&` / `echo x;` prefix, or a `cd` to the same literal
//! directory, does not change the target, so the compound lines must get
//! the same Deny (Undetermined handling rule 2/3: the legacy layer may only
//! raise; memory `blastguard-must-resolve-statically`: a literal `cd` is
//! resolved and the target judged as its absolute spelling — whose
//! `rm -rf <abs>` twin is Deny, control below).
//!
//! The `[system harm]` marker on the single-command Deny is a separate
//! question this file does not pin (`harms_system` would say "not system
//! harm" for a confined path).
//!
//! Observed at origin/main 1dfa60681 (blastguard 0.2.699), debug binary,
//! main mode: the RED cases are `ask`; subagent mode is `deny` for every
//! case (the Ask is hardened), so the weakening is main-only.
//!
//! Content rule: every path is a NON-EXISTENT sentinel under
//! `/private/var/folders/zz/`; nothing is created there and nothing runs.
//!
//! DOES NOT PROVE: behaviour with `TMPDIR` set (then the cwd is a Scratch
//! temp root and a different row applies); other system prefixes; that the
//! fix keeps the `[system harm]` marker.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    /// Holds only the approvals store (a real, writable directory).
    base: PathBuf,
    /// Non-existent `HOME` under `/private/var/folders/zz/…`.
    home: String,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn wt(&self) -> String {
        format!("{}/wt", self.home)
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("sysdir-rm-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("store")).unwrap();
    let home = format!(
        "/private/var/folders/zz/bg-sysdir-compound-sentinel-{}-{tag}-{n}/home",
        std::process::id()
    );
    assert!(
        !std::path::Path::new(&home).exists(),
        "sentinel {home} must not exist"
    );
    Fx { base, home }
}

fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt();
    let mut p = serde_json::json!({
        "session_id": "sysdir-compound-20261118",
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
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("BLASTGUARD_RELAY_DIR", fx.base.join("relay"))
        .env("BLASTGUARD_BLOCK_LOG_DIR", fx.base.join("blocklog"))
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

fn deny(tag: &str, command: impl Fn(&Fx) -> String) {
    let fx = fixture(tag);
    let command = command(&fx);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

// ---------------------------------------------------------------- controls

/// Control: the single command is Deny (legacy `judge_rm` system-dir refusal).
#[test]
fn control_single_rm_denies() {
    deny("single", |_| "rm -rf build".to_string());
}

/// Control: the absolute spelling of the same target is Deny.
#[test]
fn control_absolute_rm_denies() {
    deny("abs", |fx| format!("rm -rf {}/build", fx.wt()));
}

// --------------------------------------------------------------------- RED

#[test]
#[ignore = "RED: `true && rm -rf build` under a /private/var cwd is the confined Ask; single command is Deny"]
fn and_prefix_denies() {
    deny("and", |_| "true && rm -rf build".to_string());
}

#[test]
#[ignore = "RED: `echo x; rm -rf build` under a /private/var cwd is the confined Ask; single command is Deny"]
fn semicolon_prefix_denies() {
    deny("semi", |_| "echo x; rm -rf build".to_string());
}

#[test]
#[ignore = "RED: `cd <same literal dir> && rm -rf build` under a /private/var cwd is the confined Ask; single command is Deny"]
fn literal_cd_same_dir_denies() {
    deny("cd", |fx| format!("cd {} && rm -rf build", fx.wt()));
}

#[test]
#[ignore = "RED: `=cd D && rm -rf build` under a /private/var cwd is the confined Ask; single command is Deny"]
fn zsh_equals_cd_prefix_denies() {
    deny("eqcd", |_| "=cd D && rm -rf build".to_string());
}
