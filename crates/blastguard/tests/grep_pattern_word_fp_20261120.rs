#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Grep-pattern-word FP candidate, 2026-11-20 — independent test author, not
//! the implementer (CLAUDE.md §2(a)).
//!
//! Reported: a subagent's blastguard hook answered this read-only command
//! with "`grep` is not a command blastguard recognises, and what follows it
//! parses as a destructive command line — blastguard cannot tell whether
//! `grep` runs it":
//!
//! `git -C /Users/yuki/src/harness worktree add -b red/bg-s6-4-truncate-20261120
//!  /Users/yuki/src/.harness-worktrees/bg-s6-4-red main && cd
//!  /Users/yuki/src/.harness-worktrees/bg-s6-4-red && grep -rn "truncate"
//!  crates/blastguard/src/detect* | grep -v "^.*//" | head -50`
//!
//! Suspected trigger: the grep pattern word `truncate` (a destructive verb
//! name) read as the start of the unknown-verb tail, or the `grep -v
//! "^.*//"` pattern. Every case below is read-only (grep / head / cd / a
//! worktree add) and is expected Allow in main and subagent mode.
//!
//! Harness: copied from `fp_candidates_b_20261120.rs` (hermetic `env_clear`,
//! fixture `HOME`, `CLAUDE_PROJECT_DIR` = payload cwd,
//! `BLASTGUARD_APPROVALS_DIR`, no session id). The fixture project holds a
//! `crates/blastguard/src/detect.rs` so the relative glob has a real match.
//! The hook only JUDGES the text; nothing runs.
//!
//! Observed at f34e298af (2026-11-20): every form containing the pattern
//! word `truncate` is Ask in main mode (reason above, verbatim) and Deny in
//! subagent mode, including the bare `grep -rn "truncate" <glob>`; the
//! worktree-add and cd prefixes and the `| grep -v "^.*//" | head` suffix do
//! not change it. The same pipeline with pattern `foo` is Allow, so the
//! `grep -v "^.*//"` pattern is not the trigger. Minimal observed trigger:
//! `grep -rn "truncate" crates/blastguard/src/detect*`. Without the glob
//! (plain file operand) the same pattern word is Allow
//! (minimal_grep_truncate_word_is_allow), so the trigger needs the glob
//! operand. The five failing cases were RED (`#[ignore]`d while the defect
//! stood) and are un-ignored now that `unknown_wrapper_ask` skips read-only
//! heads (`readonly::argv_is_data_only`); the passing twin and controls were
//! never ignored.
//!
//! DOES NOT PROVE: which rule produces a decision (only decisions are
//! asserted); that other destructive verb names used as grep patterns behave
//! the same; the verdict of whatever older blastguard the reporting session
//! had installed (this judges the binary built from this tree).

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
        "grep-word-fp-20261120-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    let src = base.join("src").join("proj").join("crates/blastguard/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("detect.rs"), "// truncate\nfn truncate() {}\n").unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

/// Run the hook once; `(decision, reason)`. A non-zero exit panics; only then
/// is an empty stdout read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "grep-word-fp-20261120",
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

/// Judge every command in both modes; collect all non-Allow answers, then
/// fail once listing them (so one run records every verdict).
#[track_caller]
fn allow_all(tag: &str, commands: &[&str]) {
    let fx = fixture(tag);
    let mut bad = Vec::new();
    for c in commands {
        for subagent in [false, true] {
            let (d, r) = run_as(&fx, &fx.proj(), c, subagent);
            if d != "allow" {
                bad.push(format!("subagent={subagent}: {c:?} => {d} ({r})"));
            }
        }
    }
    assert!(bad.is_empty(), "expected Allow:\n{}", bad.join("\n"));
}

#[track_caller]
fn not_allow_all(tag: &str, commands: &[&str]) {
    let fx = fixture(tag);
    for c in commands {
        for subagent in [false, true] {
            let (d, r) = run_as(&fx, &fx.proj(), c, subagent);
            assert_ne!(
                d, "allow",
                "control subagent={subagent}: {c:?} must not be Allow ({r})"
            );
        }
    }
}

const WT_ADD: &str = "git -C /Users/yuki/src/harness worktree add -b red/bg-s6-4-truncate-20261120 /Users/yuki/src/.harness-worktrees/bg-s6-4-red main";
const CD: &str = "cd /Users/yuki/src/.harness-worktrees/bg-s6-4-red";

/// The reported command, verbatim.
#[test]
fn full_reported_command_is_allow() {
    allow_all(
        "full",
        &[&format!(
            "{WT_ADD} && {CD} && grep -rn \"truncate\" crates/blastguard/src/detect* | grep -v \"^.*//\" | head -50"
        )],
    );
}

/// Bare grep with the `truncate` pattern word (quoted and unquoted).
#[test]
fn bare_grep_truncate_is_allow() {
    allow_all(
        "bare",
        &[
            "grep -rn \"truncate\" crates/blastguard/src/detect*",
            "grep -rn truncate crates/blastguard/src/detect*",
        ],
    );
}

/// Bare grep piped into `grep -v "^.*//"` (and `| head -50`).
#[test]
fn grep_truncate_piped_grep_v_is_allow() {
    allow_all(
        "pipe",
        &[
            "grep -rn \"truncate\" crates/blastguard/src/detect* | grep -v \"^.*//\"",
            "grep -rn \"truncate\" crates/blastguard/src/detect* | grep -v \"^.*//\" | head -50",
        ],
    );
}

/// The `grep -v "^.*//"` pattern alone, with a non-destructive search word.
#[test]
fn grep_v_slashes_pattern_alone_is_allow() {
    allow_all(
        "gv",
        &["grep -rn \"foo\" crates/blastguard/src/detect* | grep -v \"^.*//\" | head -50"],
    );
}

/// With the cd prefix, no worktree add.
#[test]
fn cd_prefix_is_allow() {
    allow_all(
        "cd",
        &[
            &format!("{CD} && grep -rn \"truncate\" crates/blastguard/src/detect*"),
            &format!(
                "{CD} && grep -rn \"truncate\" crates/blastguard/src/detect* | grep -v \"^.*//\" | head -50"
            ),
        ],
    );
}

/// With the worktree add prefix, no cd.
#[test]
fn worktree_add_prefix_is_allow() {
    allow_all(
        "wt",
        &[
            &format!("{WT_ADD} && grep -rn \"truncate\" crates/blastguard/src/detect*"),
            &format!(
                "{WT_ADD} && grep -rn \"truncate\" crates/blastguard/src/detect* | grep -v \"^.*//\" | head -50"
            ),
        ],
    );
}

/// Controls: shapes the existing suite already shows as non-Allow stay so.
#[test]
fn control_destructive_shapes_not_allow() {
    not_allow_all(
        "ctl",
        &[
            "xargs rm -rf /",
            "grep -rl x . | xargs rm -rf /",
            "truncate -s 0 /usr/lib/libc.so",
        ],
    );
}

/// Minimisation: no glob, no flags, a non-verb word as control twin.
#[test]
fn minimal_grep_truncate_word_is_allow() {
    allow_all(
        "min",
        &[
            "grep -rn truncate crates/blastguard/src/detect.rs",
            "grep truncate crates/blastguard/src/detect.rs",
        ],
    );
}

/// Twin: same shape with a non-destructive pattern word is Allow today.
#[test]
fn minimal_grep_plain_word_twin_is_allow() {
    allow_all(
        "mintwin",
        &[
            "grep -rn foo crates/blastguard/src/detect*",
            "grep foo crates/blastguard/src/detect.rs",
        ],
    );
}

/// Control (verifier finding 2026-11-21, run-20261121-102650-23366): a
/// read-only-looking head whose option runs its argument as a program must
/// keep a non-Allow verdict. Observed: `bat --pager rm -rf /etc` was ask at
/// base 3e4c4359a and Allow at 1ee756c2a (the data-only exclusion covered
/// `bat`). Independent test author (orchestrator), not the implementer.
#[test]
fn control_exec_capable_option_heads_not_allow() {
    not_allow_all(
        "execopt",
        &[
            "bat --pager rm -rf /etc",
            "rg --pre rm -rf /etc x",
        ],
    );
}
