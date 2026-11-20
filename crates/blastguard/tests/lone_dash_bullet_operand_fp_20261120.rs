#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Lone-dash / markdown-bullet operand false positive (2026-11-20).
//!
//! Background: at a26610f37 (blastguard 0.2.800, quoted option words read as
//! their unquoted twins) a memory append of the shape
//!
//! ```text
//! cd <dir> && printf '%s\n' '- <bullet text with spaces>' >> file.md
//! ```
//!
//! measured Ask (allow at main 0.2.799): the token `'-` (start of the quoted
//! bullet) was taken as an option word named `-` and lost its LEADING quote,
//! so readers that recognise a quoted string by its leading quote read the
//! bullet's words as a command line. The fix (0.2.801, `option_words` keeps a
//! lone dash / dash-plus-blank word as is) restores allow.
//!
//! Written by an independent test author (not the implementer; CLAUDE.md
//! §2(a)). Measured 2026-11-20: at a26610f37 the two
//! `destructive_words_bullet_*` cases FAIL (main reason: "`note:` is not a
//! command blastguard recognises, and what follows it parses as a destructive
//! command line"; subagent denied); all 11 pass at 8e1ff7a1a. The other allow
//! cases (plain bullet, lone `-`) pass on BOTH trees: they are regression
//! guards for the fix, not RED pins of the FP.
//!
//! Allow cases are held to `allow/allow` (main/subagent). Controls keep the
//! fix from being a blanket allow: a single-quoted `'-i'` sed rewrite of
//! `/etc/hosts` and an `rm -rf /` chained after the same printf append must
//! stay `deny/deny`.
//!
//! Fixture (same style as `raw_token_quoted_option_20261120.rs`):
//! `<base>/home` (hook `HOME`), `<base>/src/proj` (git main tree,
//! `CLAUDE_PROJECT_DIR`), `<base>/src/.harness-worktrees/wt-own` (linked
//! worktree, SESSION CWD, with a `notes/` subdir), `<base>/tmpdir`
//! (`TMPDIR`). `env_clear()` plus a minimal env; git variables scrubbed;
//! subagent mode = `agent_id` in the payload. Nothing is executed.
//!
//! DOES NOT PROVE: which layer produced a verdict (only the decision is
//! asserted); the reason text; multi-word quoted arguments that start with a
//! real option name (`'-v some text'`, the documented remaining side effect);
//! `$'..'` quoting; verdicts with the cwd in the main tree.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
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
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self) -> PathBuf {
        self.base
            .join("src")
            .join(".harness-worktrees")
            .join("wt-own")
    }
    fn memory_dir(&self) -> PathBuf {
        self.home()
            .join(".claude")
            .join("projects")
            .join("-proj")
            .join("memory")
    }
    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = git_env::pinned(dir);
        cmd.args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home());
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(
            out.status.success(),
            "fixture git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("lonedash-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&base);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base };
    let repo = fx.repo();
    std::fs::write(repo.join("t.txt"), "a\n").unwrap();
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    std::fs::create_dir_all(wt.join("notes")).unwrap();
    std::fs::write(wt.join("notes").join("file.md"), "# notes\n").unwrap();
    std::fs::write(wt.join("file.md"), "# notes\n").unwrap();
    std::fs::create_dir_all(fx.memory_dir()).unwrap();
    std::fs::write(fx.memory_dir().join("file.md"), "# memory\n").unwrap();
    fx
}

/// Run the hook once with the session cwd in `wt-own`; `(decision, reason)`.
/// A non-zero exit panics; only then is an empty stdout read as Allow;
/// non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt();
    let mut p = serde_json::json!({
        "session_id": "lone-dash-bullet-operand-fp-20261120",
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
        .env("TMPDIR", fx.base.join("tmpdir"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", fx.repo())
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git_env::scrub(&mut cmd);
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

/// `command` (built from the fixture) must be `expected` ("main/subagent").
fn expect_with(tag: &str, build: impl Fn(&Fx) -> String, expected: &str) {
    let fx = fixture(tag);
    let command = build(&fx);
    let (m, rm) = run_as(&fx, &command, false);
    let (s, rs) = run_as(&fx, &command, true);
    let got = format!("{m}/{s}");
    println!("MEASURE {command:?} => {got}");
    assert_eq!(
        got, expected,
        "{command:?} must be {expected}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

fn expect(tag: &str, command: &str, expected: &str) {
    expect_with(tag, |_| command.to_string(), expected);
}

const BULLET: &str = "'- 2026-11-19 literal-dir glob ruling: keep the literal dir as is'";
const DESTRUCTIVE_BULLET: &str = "'- note: rm -rf /tmp/zqxv* and `backticks`'";

// ---------------------------------------------------------------------------
// Allow: quoted markdown bullet operand
// ---------------------------------------------------------------------------

#[test]
fn bullet_append_in_own_dir() {
    expect(
        "own",
        &format!("printf '%s\\n' {BULLET} >> file.md"),
        "allow/allow",
    );
}

#[test]
fn bullet_append_after_cd_subdir() {
    expect(
        "cd-sub",
        &format!("cd notes && printf '%s\\n' {BULLET} >> file.md"),
        "allow/allow",
    );
}

#[test]
fn bullet_append_after_cd_memory_dir() {
    expect_with(
        "cd-mem",
        |fx| {
            format!(
                "cd {} && printf '%s\\n' {BULLET} >> file.md",
                fx.memory_dir().display()
            )
        },
        "allow/allow",
    );
}

#[test]
fn destructive_words_bullet_append_in_own_dir() {
    expect(
        "destr-own",
        &format!("printf '%s\\n' {DESTRUCTIVE_BULLET} >> file.md"),
        "allow/allow",
    );
}

#[test]
fn destructive_words_bullet_append_after_cd_memory_dir() {
    expect_with(
        "destr-mem",
        |fx| {
            format!(
                "cd {} && printf '%s\\n' {DESTRUCTIVE_BULLET} >> file.md",
                fx.memory_dir().display()
            )
        },
        "allow/allow",
    );
}

#[test]
fn double_quoted_bullet_append_after_cd_subdir() {
    expect(
        "dq-sub",
        "cd notes && printf '%s\\n' \"- bullet text with spaces\" >> file.md",
        "allow/allow",
    );
}

// ---------------------------------------------------------------------------
// Allow: lone dash operands
// ---------------------------------------------------------------------------

#[test]
fn lone_single_quoted_dash_operand() {
    expect("lone-sq", "printf '%s\\n' '-' >> file.md", "allow/allow");
}

#[test]
fn lone_double_quoted_dash_operand() {
    expect("lone-dq", "printf '%s\\n' \"-\" >> file.md", "allow/allow");
}

#[test]
fn lone_dash_operand_after_cd_subdir() {
    expect(
        "lone-cd",
        "cd notes && printf '%s\\n' '-' >> file.md",
        "allow/allow",
    );
}

// ---------------------------------------------------------------------------
// Controls: must stay strict
// ---------------------------------------------------------------------------

#[test]
fn control_single_quoted_sed_in_place_system_target() {
    expect("ctl-sed", "sed '-i' -e d /etc/hosts", "deny/deny");
}

#[test]
fn control_rm_rf_root_after_bullet_append() {
    expect(
        "ctl-rm",
        &format!("printf '%s\\n' {BULLET} >> file.md && rm -rf /"),
        "deny/deny",
    );
}
