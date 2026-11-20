#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Possible FP observed 2026-11-20 (live hook, older blastguard) — independent
//! test author, not the implementer (CLAUDE.md §2(a)).
//!
//! Observed shape: a Bash command `python3 - <<'EOF'` … `EOF` whose Python
//! body only reads a file, edits the text in memory and writes it back to a
//! Rust test file inside the session's own linked worktree. The word
//! `truncate` occurs only inside Python string data. Refusal reported:
//! `truncate can shrink a file to zero bytes`.
//!
//! Rulings (memory heredoc-body-by-target-language): a python stdin program
//! is judged by the python reading, not as shell lines; pure calls stay Allow;
//! an fs-write sink is judged on its path by the existing path rules.
//! Related earlier RED: backlog_20261117_python_heredoc_literal_truncate_fp.rs
//! (main mode only, a `cd <abs> && python3 -` shape with `open(p,"w")`).
//!
//! Fixture: a git repo `proj` with a linked worktree
//! `.harness-worktrees/session-pytrunc` that is the session's project dir
//! (`CLAUDE_PROJECT_DIR`) and the payload cwd; hermetic env (`env_clear`,
//! fixture `HOME`, `BLASTGUARD_APPROVALS_DIR`, no `TMPDIR`, no session id env).
//! Run in main mode and subagent mode. The hook only JUDGES the text; nothing
//! runs.
//!
//! Measured at main c09b34cd7 (blastguard 0.2.784), 2026-11-20:
//!   * the `truncate`-reason Deny does NOT reproduce: (a) is Allow, and the
//!     b/c cases are not refused with the truncate reason;
//!   * b1/b2/b3 AND the word-free twin c are Deny in main mode with the
//!     whose-tree refusal ("... inside a git work tree, but not one of this
//!     session's trees (neither inside its safe roots nor a recognised
//!     worktree checkout) ...") for `pathlib.Path.write_text` on the tracked
//!     file, while the shell-redirect twin `echo x > <same file>` is Allow
//!     (calibration control). So the FP at main is the write_text tree
//!     attribution, independent of the `truncate` word. Pinned as ignored
//!     REDs. Probably the same family as backlog 94b93e95
//!     (backlog_20261120_python_heredoc_open_in_condukt_worktree_fp.rs,
//!     `open(p,'w')` in a condukt worktree) — not proven here.
//!
//! DOES NOT PROVE: which rule produces a decision (only decisions are
//! asserted); behaviour of the live (older) hook binary; other verbs than
//! `truncate`; unquoted heredocs.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home);
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fx {
    base: PathBuf,
    home: PathBuf,
    wt: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

const REL: &str = "crates/blastguard/tests/x_rules.rs";

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = neutral_base::neutral_base().join(format!(
        "py-trunc-20261120-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let base = std::fs::canonicalize(&root).unwrap();
    let home = base.join("home");
    let repo = base.join("proj");
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    std::fs::create_dir_all(repo.join("crates/blastguard/tests")).unwrap();
    git(&home, &repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(REL), "// OLD truncate\n#[test]\nfn a() {}\n").unwrap();
    git(&home, &repo, &["add", REL]);
    git(&home, &repo, &["commit", "-q", "-m", "init"]);
    std::fs::create_dir_all(base.join(".harness-worktrees")).unwrap();
    let wt = base.join(".harness-worktrees/session-pytrunc");
    git(
        &home,
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "session-pytrunc",
            wt.to_str().unwrap(),
        ],
    );
    assert!(wt.join(".git").is_file(), "fixture: linked worktree");
    assert!(wt.join(REL).is_file(), "fixture: tracked file in worktree");
    Fx { base, home, wt }
}

/// Run the hook once; `(decision, reason)`. A non-zero exit panics; only then
/// is an empty stdout read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-stdin-heredoc-verb-word-fp-20261120",
        "cwd": fx.wt,
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
        .env("CLAUDE_PROJECT_DIR", &fx.wt)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&fx.wt)
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
fn allow_both(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

#[track_caller]
fn deny_both(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

#[track_caller]
fn not_allow_both(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, command, subagent);
        assert_ne!(
            d, "allow",
            "subagent={subagent}: {command:?} must not be Allow ({r})"
        );
    }
}

/// Python body: read `path`, replace each listed word in memory, write back.
fn edit_body(path: &str, words: &[&str]) -> String {
    let list = words
        .iter()
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "python3 - <<'EOF'\nfrom pathlib import Path\np = Path(\"{path}\")\n\
s = p.read_text()\nwords = [{list}]\nfor w in words:\n    \
s = s.replace(\"OLD \" + w, \"NEW \" + w)\np.write_text(s)\nEOF"
    )
}

/// (a) the word only inside a `print` string literal.
#[test]
fn a_print_literal_truncate_word_is_allow() {
    let fx = fixture("a");
    allow_both(&fx, "python3 - <<'EOF'\nprint(\"truncate -s 0 x\")\nEOF");
}

/// (b1) observed shape: string list holding "truncate", read_text/write_text
/// of a tracked file in the session's own worktree (relative path, cwd = wt).
// test-weakening-justified: new RED for an observed FP (2026-11-20)
#[test]
#[ignore = "RED FP: python3 - heredoc Path.write_text of a tracked file in the session's own linked worktree gets the whose-tree refusal; shell redirect twin is Allow"]
fn b1_own_worktree_edit_relative_with_truncate_word_is_allow() {
    let fx = fixture("b1");
    allow_both(
        &fx,
        &edit_body(REL, &["truncate", "dd of=", "find -delete"]),
    );
}

/// (b2) same with an absolute path into the own worktree.
// test-weakening-justified: new RED for an observed FP (2026-11-20)
#[test]
#[ignore = "RED FP: python3 - heredoc Path.write_text of a tracked file in the session's own linked worktree gets the whose-tree refusal; shell redirect twin is Allow"]
fn b2_own_worktree_edit_absolute_with_truncate_word_is_allow() {
    let fx = fixture("b2");
    let abs = fx.wt.join(REL);
    allow_both(
        &fx,
        &edit_body(
            abs.to_str().unwrap(),
            &["truncate", "dd of=", "find -delete"],
        ),
    );
}

/// (b3) `cd <own wt> && python3 -` spelling (the 2026-11-17 refused shape's
/// head), relative path.
// test-weakening-justified: new RED for an observed FP (2026-11-20)
#[test]
#[ignore = "RED FP: python3 - heredoc Path.write_text of a tracked file in the session's own linked worktree gets the whose-tree refusal; shell redirect twin is Allow"]
fn b3_cd_own_worktree_then_edit_with_truncate_word_is_allow() {
    let fx = fixture("b3");
    let cmd = format!(
        "cd {} && {}",
        fx.wt.display(),
        edit_body(REL, &["truncate", "dd of=", "find -delete"])
    );
    allow_both(&fx, &cmd);
}

/// (c) twin of b1 without the `truncate` word (measured).
// test-weakening-justified: new RED for an observed FP (2026-11-20)
#[test]
#[ignore = "RED FP: python3 - heredoc Path.write_text of a tracked file in the session's own linked worktree gets the whose-tree refusal; shell redirect twin is Allow"]
fn c_own_worktree_edit_without_truncate_word_is_allow() {
    let fx = fixture("c");
    allow_both(&fx, &edit_body(REL, &["alpha", "beta"]));
}

/// Control (fixture calibration): the same overwrite spelled as a shell
/// redirect into the own worktree's tracked file.
#[test]
fn control_shell_redirect_overwrite_in_own_worktree_is_allow() {
    let fx = fixture("ctl0");
    allow_both(&fx, &format!("echo x > {REL}"));
}

/// Control: the python body really truncates a system file.
#[test]
fn control_python_os_truncate_etc_hosts_is_deny() {
    let fx = fixture("ctl1");
    deny_both(
        &fx,
        "python3 - <<'EOF'\nimport os\nos.truncate(\"/etc/hosts\", 0)\nEOF",
    );
}

/// Control: a shell heredoc body that runs `truncate` on a HOME dotfile.
#[test]
fn control_bash_heredoc_truncate_bashrc_is_not_allow() {
    let fx = fixture("ctl2");
    not_allow_both(&fx, "bash <<'EOF'\ntruncate -s 0 ~/.bashrc\nEOF");
}
