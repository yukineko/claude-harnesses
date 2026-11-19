#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FP candidates 2026-11-18 .. 11-20 (scratchpad list `fp-candidates-20261118.md`,
//! entries 2, 3, 4) — independent test author, not the implementer (CLAUDE.md
//! §2(a)). Entries 1, 5, 6 are NOT pinned here: their expected verdict is
//! unclear or contradicts an existing pinned control
//! (`scope_redirect_relaxation_boundaries_20261108.rs`
//! `wt_semicolon_between_benign_prefix_and_write`;
//! `backlog_20261118_static_resolve_fp.rs` `rc5_…`), so no expectation is
//! invented for them.
//!
//! Observed at 47bd5a888: entries 2, 3, 4 no longer reproduce (their FP
//! tests pass as regression pins). The entry-3 CONTROL twin (a System write
//! from the same `cd <wt> && python3 - <<'EOF'` shape) is Allow — a
//! system-harm fail-open, pinned as an ignored RED together with a minimal
//! `cd <dir> && python3 -c` form.
//!
//! Each FP case is paired with a control twin that must stay blocked, so a
//! fix that simply stops judging the shape is caught.
//!
//! Harness: as `backlog_20261118_static_resolve_fp.rs` (hermetic `env_clear`,
//! fixture `HOME`, `CLAUDE_PROJECT_DIR` = payload cwd,
//! `BLASTGUARD_APPROVALS_DIR`, no `TMPDIR`, no `CLAUDE_CODE_SESSION_ID`), run
//! in main mode and in subagent mode (`agent_id`). Every target is a
//! non-existent sentinel under `/tmp/bg-fpc-sentinel-*`, inside the fixture's
//! own linked-worktree-shaped checkout, or `/etc/bg-fpc-sentinel-*` for the
//! System controls. The hook only JUDGES the text; nothing is executed.
//!
//! DOES NOT PROVE: which rule produces a decision (only decisions are
//! asserted); the real (unsanitized) commands; anything about entries 1, 5, 6.

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
    /// A linked-worktree-shaped checkout under the hermetic
    /// `$HOME/.condukt/worktrees` storage root.
    fn own_worktree(&self) -> PathBuf {
        let wt = self
            .home()
            .join(".condukt")
            .join("worktrees")
            .join("bg-fpc-wt");
        std::fs::create_dir_all(wt.join("crates/x/tests")).unwrap();
        std::fs::write(
            wt.join(".git"),
            "gitdir: /nonexistent/bg-fpc/.git/worktrees/bg-fpc-wt\n",
        )
        .unwrap();
        wt
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "fp-cand-20261120-{tag}-{}",
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
        "session_id": "fp-candidates-20261120",
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

/// Pin `baseline` to `expected` in both modes, then require `variant` to
/// answer exactly as the baseline did in each mode.
#[track_caller]
fn parity_at(fx: &Fx, cwd: &Path, baseline: &str, expected: &str, variant: &str) {
    for subagent in [false, true] {
        let (b, br) = run_as(fx, cwd, baseline, subagent);
        assert_eq!(
            b, expected,
            "subagent={subagent}: baseline {baseline:?} must be {expected}; got {b} ({br})"
        );
        let (v, vr) = run_as(fx, cwd, variant, subagent);
        assert_eq!(
            v, b,
            "subagent={subagent}: {variant:?} must answer as the baseline {baseline:?} ({b}); got {v} ({vr})"
        );
    }
}

// ---------------------------------------------------------------------------
// Entry 2 — `cd <scratch> && cat >> X.md <<'EOF'` whose body holds a
// backticked `git show :3:$f > $f`. Expected: Allow (a .md landing is data,
// rulings 2026-11-08 / 11-18; fe06d0b4b step 0a). Checks `>>` append.
// ---------------------------------------------------------------------------

const E2_BODY: &str = "## 1. Redirect overwrite\n- Command shape: `cd /x; f=<rel>; git show :3:$f > $f`\n- Hook cwd: main.";

#[test]
fn e2_append_heredoc_to_md_with_backtick_redirect_is_allow() {
    let fx = fixture("e2");
    let s = "/tmp/bg-fpc-sentinel-e2";
    allow_at(
        &fx,
        &fx.proj(),
        &format!("cd {s} && cat >> fp-candidates.md <<'EOF'\n{E2_BODY}\nEOF"),
    );
}

/// Control twin: the same append aimed at a System path stays Deny.
#[test]
fn e2_control_append_heredoc_to_system_md_is_deny() {
    let fx = fixture("e2c");
    deny_at(
        &fx,
        &fx.proj(),
        &format!("cat >> /etc/bg-fpc-sentinel-e2.md <<'EOF'\n{E2_BODY}\nEOF"),
    );
}

// ---------------------------------------------------------------------------
// Entry 3 — `cd <own wt> && python3 - <<'EOF'` whose Python body rewrites a
// tests/*.rs file and holds the string literal
// `scoped("cd .githooks && rm ../src/build.o")`. Expected: the body is not
// judged as shell lines (S5: python parser). Measured as parity: the program
// with that literal must answer exactly as the same program without it.
// ---------------------------------------------------------------------------

fn e3_program(literal: &str) -> String {
    format!(
        "import pathlib\np = pathlib.Path(\"crates/x/tests/t.rs\")\ns = p.read_text()\ns = s.replace(\"OLD\", {literal})\np.write_text(s)"
    )
}

#[test]
fn e3_python_stdin_heredoc_string_literal_shell_text_parity() {
    let fx = fixture("e3");
    let wt = fx.own_worktree();
    let w = wt.display().to_string();
    let base = format!(
        "cd {w} && python3 - <<'EOF'\n{}\nEOF",
        e3_program("'scoped(\"cd crates && ls\")'")
    );
    let variant = format!(
        "cd {w} && python3 - <<'EOF'\n{}\nEOF",
        e3_program("'scoped(\"cd .githooks && rm ../src/build.o\")'")
    );
    parity_at(&fx, &wt, &base, "allow", &variant);
}

/// Control twin: the same program shape writing a System path stays Deny.
/// Observed Allow in both modes at 47bd5a888 (system-harm fail-open).
#[test]
#[ignore = "RED fp-cand 2026-11-20: entry 3 control — cd <own wt> && python3 - heredoc system write is Allow (fail-open)"]
fn e3_control_python_stdin_heredoc_system_write_is_deny() {
    let fx = fixture("e3c");
    let wt = fx.own_worktree();
    let w = wt.display().to_string();
    let prog = "import pathlib\np = pathlib.Path(\"/etc/bg-fpc-sentinel-e3\")\np.write_text('scoped(\"cd .githooks && rm ../src/build.o\")')";
    deny_at(
        &fx,
        &wt,
        &format!("cd {w} && python3 - <<'EOF'\n{prog}\nEOF"),
    );
}

// ---------------------------------------------------------------------------
// Entry 4 — `git commit -F - -- <two literal paths> <<'EOF'` whose message
// holds `echo 'print(1)' | python3 -m asyncio pinned to the`. Expected: Allow
// (the quoted heredoc fed to `git commit -F -` is data, 2026-11-18 ruling).
// Observed: main passed, subagent denied.
// ---------------------------------------------------------------------------

const E4_MSG: &str = "blastguard: RED tests for X\n\n- Parity twin added: echo 'print(1)' | python3 -m asyncio pinned to the\n  same verdict as the literal spelling.\n\nCo-Authored-By: t <t@example.invalid>";

#[test]
fn e4_commit_f_dash_pathspec_heredoc_asyncio_line_is_allow() {
    let fx = fixture("e4");
    let wt = fx.own_worktree();
    allow_at(
        &fx,
        &wt,
        &format!(
            "git commit -F - -- crates/x/tests/a.rs crates/x/tests/b.rs <<'EOF'\n{E4_MSG}\nEOF"
        ),
    );
}

/// Control twin: the message line run as a real command (stdin program to
/// python3) stays refused in the subagent.
#[test]
fn e4_control_real_stdin_program_to_python_is_refused() {
    let fx = fixture("e4c");
    let wt = fx.own_worktree();
    let cmd = "echo 'print(1)' | python3 -m asyncio";
    let (ds, rs) = run_as(&fx, &wt, cmd, true);
    assert_eq!(
        ds, "deny",
        "subagent: {cmd:?} expected Deny, got {ds} ({rs})"
    );
    let (dm, rm) = run_as(&fx, &wt, cmd, false);
    assert!(
        dm == "ask" || dm == "deny",
        "main: {cmd:?} expected Ask or stricter, got {dm} ({rm})"
    );
}

// ---------------------------------------------------------------------------
// Found while building the entry-3 control twin: a `cd <dir> &&` prefix makes
// an interpreter write into a System directory Allow (system harm: must be
// Deny through any wrapper). The same program without the prefix, or with
// `cd <dir>;` / `true &&`, is Deny today. Same class as the existing
// `interpreter_system_dir_new_file_failopen_20261107.rs` (backlog 0fbfe00d,
// closed done at 0.2.395), which fails again at 47bd5a888.
// ---------------------------------------------------------------------------

const SYS_PY_C: &str = "python3 -c 'open(\"/etc/bg-fpc-sentinel-cd\", \"w\").write(\"x\")'";

#[test]
#[ignore = "RED fp-cand 2026-11-20: entry 3 control — cd <dir> && python3 -c system write is Allow (fail-open)"]
fn cd_and_prefix_python_c_system_write_is_deny() {
    let fx = fixture("cdpy");
    deny_at(
        &fx,
        &fx.proj(),
        &format!("cd /tmp/bg-fpc-sentinel-cd && {SYS_PY_C}"),
    );
}

/// Twins that are Deny today: no prefix, `;`-closed `cd`, `true &&`.
#[test]
fn cd_and_prefix_python_c_system_write_twins_are_deny() {
    let fx = fixture("cdpyt");
    for c in [
        SYS_PY_C.to_string(),
        format!("cd /tmp/bg-fpc-sentinel-cd; {SYS_PY_C}"),
        format!("true && {SYS_PY_C}"),
    ] {
        deny_at(&fx, &fx.proj(), &c);
    }
}
