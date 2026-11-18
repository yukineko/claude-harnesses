#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open strength): a `python3 -c` program that writes into a
//! System directory through a path passed in `sys.argv` is Ask (main), not
//! Deny. User ruling (memory `system-harm-always-deny`, 2026-11-07): "有害な
//! 対象は、どの wrapper を通しても Deny".
//!
//! Written by an independent test author (not the implementer; CLAUDE.md
//! §2(a)) against origin/main a19a34553 (blastguard 0.2.702).
//!
//! Observed 2026-11-18 at a19a34553 (probe of the built hook binary, same env
//! as this harness), main / subagent:
//!   python3 -c "import sys;open(sys.argv[1],'w').write('x')" /etc/bg-sentinel   ask / deny
//!   python3 -c "import sys;open(sys.argv[1],'w').write('x')" /etc/passwd        ask / deny
//!   python3 -c "import sys,pathlib;pathlib.Path(sys.argv[1]).write_text('x')" /etc/bg-sentinel  ask / deny
//!     reason (main): "the program calls `open` on a path blastguard cannot
//!     resolve statically — ..."
//!   python3 -c "open('/etc/bg-sentinel','w').write('x')"                        deny / deny
//!   python3 -c "import pathlib;pathlib.Path('/etc/bg-sentinel').write_text('x')" deny / deny
//! The argv operand is a literal on the command line, so the target is
//! statically known (memory `blastguard-must-resolve-statically`); the
//! literal-in-program twin is already Deny.
//!
//! Scratch twins (controls requested by the coordinator), observed:
//!   python3 -c "open('/tmp/bg-sentinel-py','w').write('x')"                    allow / allow
//!   python3 -c "import pathlib;pathlib.Path('/tmp/bg-sentinel-py').write_text('x')" allow / allow
//!   python3 -c "import sys;open(sys.argv[1],'w').write('x')" /tmp/bg-sentinel-py  ask / deny  (NOT Allow)
//!   python3 -c "import sys,pathlib;pathlib.Path(sys.argv[1]).write_text('x')" /tmp/bg-sentinel-py  ask / deny  (NOT Allow)
//! The argv scratch twins are kept as ignored REDs below: SUBSET.md ruling 15
//! keeps Python on its legacy readings until S5, so Allow for them is the
//! Table C Scratch cell's expectation, not a legacy pin.
//!
//! Content rule: every target is a non-existent sentinel path; the hook only
//! JUDGES the command text, nothing is executed.
//!
//! Harness: same as `s3_home_tilde_branches_20261118.rs` (`env_clear`,
//! fixture HOME, `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT=cli`, main and
//! subagent modes).
//!
//! DOES NOT PROVE (whole file): that the Deny is the system-harm kind
//! (`Decision::deny_system_harm`) rather than another Deny; `sys.argv[N]` for
//! N != 1; other write APIs (`os.open`, `shutil`, `write_bytes`, mode `'a'`);
//! other interpreters; argv operands that are expansions (backlog d6de38be).

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
        "pyc-argv-sys-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base, home }
}

/// Run the hook once; `(decision, reason)`. Non-zero exit panics before an
/// empty stdout can be read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-c-argv-system-write-20261118",
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

fn expect_both(tag: &str, command: &str, want: &str) {
    let fx = fixture(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &fx.proj(), command, subagent);
        assert_eq!(
            d, want,
            "subagent={subagent}: {command:?} expected {want}, got {d} ({r})"
        );
    }
}

const SYS: &str = "/etc/bg-sentinel";
const SCRATCH: &str = "/tmp/bg-sentinel-pyc-argv";

fn open_argv(p: &str) -> String {
    format!("python3 -c \"import sys;open(sys.argv[1],'w').write('x')\" {p}")
}
fn open_literal(p: &str) -> String {
    format!("python3 -c \"open('{p}','w').write('x')\"")
}
fn pathlib_argv(p: &str) -> String {
    format!("python3 -c \"import sys,pathlib;pathlib.Path(sys.argv[1]).write_text('x')\" {p}")
}
fn pathlib_literal(p: &str) -> String {
    format!("python3 -c \"import pathlib;pathlib.Path('{p}').write_text('x')\"")
}

// ---------------------------------------------------------------------------
// System target via argv: Deny in both modes (RED).
// ---------------------------------------------------------------------------

/// DOES NOT PROVE: the system-harm kind of the Deny.
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: python -c open(sys.argv[1],'w') with literal argv /etc/... is Ask (main), system-harm-always-deny requires Deny"]
fn open_argv_system_write_denies() {
    expect_both("open-argv-sys", &open_argv(SYS), "deny");
}

/// DOES NOT PROVE: the system-harm kind of the Deny.
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: python -c pathlib.Path(sys.argv[1]).write_text with literal argv /etc/... is Ask (main), system-harm-always-deny requires Deny"]
fn pathlib_argv_system_write_denies() {
    expect_both("pl-argv-sys", &pathlib_argv(SYS), "deny");
}

// ---------------------------------------------------------------------------
// System target as a literal in the program: Deny (observed GREEN; pinned so
// the fix for the argv form cannot trade it away).
// ---------------------------------------------------------------------------

#[test]
fn open_literal_system_write_denies() {
    expect_both("open-lit-sys", &open_literal(SYS), "deny");
}

#[test]
fn pathlib_literal_system_write_denies() {
    expect_both("pl-lit-sys", &pathlib_literal(SYS), "deny");
}

// ---------------------------------------------------------------------------
// Scratch twins: Allow.
// ---------------------------------------------------------------------------

/// Control: literal scratch target, `open`. Observed GREEN.
#[test]
fn control_open_literal_scratch_write_allows() {
    expect_both("open-lit-tmp", &open_literal(SCRATCH), "allow");
}

/// Control: literal scratch target, `pathlib`. Observed GREEN.
#[test]
fn control_pathlib_literal_scratch_write_allows() {
    expect_both("pl-lit-tmp", &pathlib_literal(SCRATCH), "allow");
}

/// Scratch target via argv: Table C Scratch x Truncate is Allow; observed
/// Ask / Deny ("cannot resolve statically"). Python is legacy until S5
/// (SUBSET.md ruling 15), so this is a RED for the static-resolution fix, not
/// a legacy pin.
/// DOES NOT PROVE: that the fix must land before S5.
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: python -c open(sys.argv[1],'w') with literal argv /tmp/... is Ask/Deny, Table C Scratch is Allow"]
fn open_argv_scratch_write_allows() {
    expect_both("open-argv-tmp", &open_argv(SCRATCH), "allow");
}

/// As above, `pathlib`.
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: python -c pathlib.Path(sys.argv[1]).write_text with literal argv /tmp/... is Ask/Deny, Table C Scratch is Allow"]
fn pathlib_argv_scratch_write_allows() {
    expect_both("pl-argv-tmp", &pathlib_argv(SCRATCH), "allow");
}
