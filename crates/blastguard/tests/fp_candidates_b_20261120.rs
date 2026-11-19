#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FP candidates (b) 2026-11-20 — independent test author, not the
//! implementer (CLAUDE.md §2(a)).
//!
//! Item 1 (FP, system-harm label): argv TEXT handed to a non-interpreter
//! program that merely mentions an inline Python program writing a System
//! path is judged as an interpreter write. The originally blocked command
//! (subagent Deny, recovered verbatim from `~/.blastguard/blocks/blocks.jsonl`,
//! blastguard 0.2.675) was a `cd <wt> && . "$HOME/.cargo/env" && env -u
//! CLAUDE_CODE_SESSION_ID backlog add ... --title "...python3 -c '...'..."`.
//! Minimised at e56699977 (blastguard 0.2.751): the trigger is a command
//! WRAPPER (`env`, `env -u VAR`, `nice`) in front of a non-interpreter whose
//! argument holds the text `<word> python3 -c '<open-for-write of /etc/...>'`.
//! The same argument without the wrapper is Allow (pinned below as passing
//! twins), so only the wrapper differs. Reason text observed:
//! `'interpreter write /etc/x\' writes into a system directory` — the
//! literal carries a trailing backslash, i.e. a `\"` of the argv text was
//! read as program quoting.
//!
//! Item 2 (wrong placement of a relative redirect): `cd sub && cd ..` tracks
//! the cwd as `Known("")`, and `place_redirect_target` joins `"" + "/" +
//! target`, so a relative redirect target `T` is judged as `/T`. Observable
//! both ways:
//!   * fail-open: `cd sub && cd .. && echo x > existing.txt` is Allow while
//!     the same write without the `cd` round-trip (same real file) is not;
//!   * FP: `cd sub && cd .. && echo x > etc/hosts` is Deny ("writes into a
//!     system directory", judged as `/etc/hosts`) while the same write
//!     without the round-trip (real file `<proj>/etc/hosts`) is Allow.
//!
//! Pinned as parity: the variant must answer exactly as the baseline whose
//! redirect lands on the same real file.
//!
//! Harness: copied from `fp_candidates_20261120.rs` (hermetic `env_clear`,
//! fixture `HOME`, `CLAUDE_PROJECT_DIR` = payload cwd,
//! `BLASTGUARD_APPROVALS_DIR`, no `TMPDIR`, no session id), run in main mode
//! and subagent mode. System targets are non-existent sentinels
//! `/etc/bg-fpcb-sentinel-*`. The hook only JUDGES the text; nothing runs.
//!
//! DOES NOT PROVE: which rule produces a decision (only decisions are
//! asserted); that no other wrapper/program pair shows item 1; the verdict
//! for a protected-config target after `cd sub && cd ..` (observed Ask both
//! with and without the round-trip — same verdict, only the reason names
//! `/.claude/settings.json` — so not pinned).

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "fp-cand-b-20261120-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

/// Run the hook once; `(decision, reason)`. A non-zero exit panics; only then
/// is an empty stdout read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "fp-candidates-b-20261120",
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
        .env("HOME", fx.home())
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
        println!("[sub={subagent}] {command:?} => allow");
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    let d = h["permissionDecision"].as_str().unwrap_or("?").to_string();
    let r = h["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string();
    println!("[sub={subagent}] {command:?} => {d} ({r})");
    (d, r)
}

#[track_caller]
fn allow_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

#[track_caller]
fn deny_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

/// Pin `baseline` to `expected(subagent)` in each mode, then require
/// `variant` to answer exactly as the baseline did in that mode.
#[track_caller]
fn parity_at(
    fx: &Fx,
    cwd: &Path,
    baseline: &str,
    expected: fn(bool) -> &'static str,
    variant: &str,
) {
    for subagent in [false, true] {
        let (b, br) = run_as(fx, cwd, baseline, subagent);
        assert_eq!(
            b,
            expected(subagent),
            "subagent={subagent}: baseline {baseline:?} must be {}; got {b} ({br})",
            expected(subagent)
        );
        let (v, vr) = run_as(fx, cwd, variant, subagent);
        assert_eq!(
            v, b,
            "subagent={subagent}: {variant:?} must answer as the baseline {baseline:?} ({b}); got {v} ({vr})"
        );
    }
}

// ---------------------------------------------------------------------------
// Item 1 — argv text of a non-interpreter that mentions a python open-for-write
// of a System path, behind a command wrapper.
// ---------------------------------------------------------------------------

/// The argv TEXT (a shell double-quoted word) mentioning the program.
const MENTION: &str =
    "\"x: cd <dir> && python3 -c 'open(\\\"/etc/bg-fpcb-sentinel-1\\\",\\\"w\\\")' is Allow\"";

/// The originally blocked shape, minimised: `env -u VAR backlog add --title
/// "<text>"`. Expected Allow: the text is data passed to `backlog`.
#[test]
#[ignore = "RED fp-cand-b 2026-11-20: env-wrapped non-interpreter argv text mentioning a python System write is Deny (interpreter write)"]
fn wrapped_backlog_title_mentioning_python_system_write_is_allow() {
    let fx = fixture("i1a");
    allow_at(
        &fx,
        &fx.proj(),
        &format!("env -u CLAUDE_CODE_SESSION_ID backlog add --title {MENTION}"),
    );
}

/// `echo` twin behind `env` and behind `nice`.
#[test]
#[ignore = "RED fp-cand-b 2026-11-20: env/nice-wrapped echo of text mentioning a python System write is Deny (interpreter write)"]
fn wrapped_echo_mentioning_python_system_write_is_allow() {
    let fx = fixture("i1b");
    for c in [
        format!("env echo {MENTION}"),
        format!("nice echo {MENTION}"),
    ] {
        allow_at(&fx, &fx.proj(), &c);
    }
}

/// Twins without the wrapper: Allow at e56699977 (regression pins; they show
/// only the wrapper differs).
#[test]
fn unwrapped_mentions_are_allow() {
    let fx = fixture("i1t");
    for c in [
        format!("backlog add --title {MENTION}"),
        format!("echo {MENTION}"),
    ] {
        allow_at(&fx, &fx.proj(), &c);
    }
}

/// Control: the program actually run (bare and behind `env`) stays Deny.
#[test]
fn control_real_python_system_write_is_deny() {
    let fx = fixture("i1c");
    let prog = "python3 -c 'open(\"/etc/bg-fpcb-sentinel-1\",\"w\")'";
    deny_at(&fx, &fx.proj(), prog);
    deny_at(&fx, &fx.proj(), &format!("env {prog}"));
    deny_at(
        &fx,
        &fx.proj(),
        &format!("env -u CLAUDE_CODE_SESSION_ID {prog}"),
    );
}

// ---------------------------------------------------------------------------
// Item 2 — a relative redirect after `cd sub && cd ..` is placed at `/T`.
// ---------------------------------------------------------------------------

fn item2_fixture(tag: &str) -> Fx {
    let fx = fixture(tag);
    let p = fx.proj();
    std::fs::create_dir_all(p.join("sub")).unwrap();
    std::fs::create_dir_all(p.join("etc")).unwrap();
    std::fs::write(p.join("existing.txt"), "unrecoverable data\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(p.join("sub"), p.join("link")).unwrap();
    fx
}

/// Truncating an existing untracked project file: the round-trip variants
/// write the same real file and must not answer more permissively.
#[test]
#[ignore = "RED fp-cand-b 2026-11-20: cd sub && cd .. then a relative truncating redirect is placed at /T and answers Allow"]
fn cd_roundtrip_truncate_existing_file_parity() {
    let fx = item2_fixture("i2a");
    for variant in [
        "cd sub && cd .. && echo x > existing.txt",
        "cd link && cd .. && echo x > existing.txt",
    ] {
        parity_at(
            &fx,
            &fx.proj(),
            "echo x > existing.txt",
            |sub| if sub { "deny" } else { "ask" },
            variant,
        );
    }
}

/// A project file under `<proj>/etc`: the round-trip variant must not be
/// judged as the System path `/etc/...`.
#[test]
#[ignore = "RED fp-cand-b 2026-11-20: cd sub && cd .. then a relative redirect into proj/etc is judged as /etc (system harm Deny)"]
fn cd_roundtrip_redirect_into_project_etc_parity() {
    let fx = item2_fixture("i2b");
    parity_at(
        &fx,
        &fx.proj(),
        "echo x >> etc/bg-fpcb-sentinel-new",
        |_| "allow",
        "cd sub && cd .. && echo x >> etc/bg-fpcb-sentinel-new",
    );
}
