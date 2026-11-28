#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: a brace expansion hides the destructive flag of a git subcommand.
//!
//! bash expands `git reset {--hard,HEAD}` to `git reset --hard HEAD` before
//! exec (the test proves each expansion with `bash -c 'echo ...'`). blastguard
//! judges the unexpanded word, sees no `--hard`, and allows.
//! Measured 2026-11-28 on main 6cc6d735d (interactive session):
//!   git reset {--hard,HEAD}        => allow   (expanded: deny)
//!   git clean {-f,-x}              => allow   (expanded: ask)
//!   git push {--force,origin} main => allow   (expanded: ask)
//!   git checkout {--,.}            => allow   (expanded: deny)
//!   git branch {-D,foo}            => allow   (expanded: ask)
//! `rm {-rf,x} /etc`, `rm {-r,-f} /etc`, `rm -rf {/etc,x}` are already Deny,
//! so the gap is in the git flag detectors, not in rm.
//!
//! Controls (GREEN): every expanded spelling keeps its current verdict;
//! harmless braces (`git status {a,b}`, `git clean {-n,-x}`,
//! `git reset {--soft,HEAD}`, `echo {a,b}`) are Allow.
//!
//! DOES NOT PROVE: nested braces, sequence braces (`{a..b}`), braces built
//! from variables; subagent mode.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// (braced spelling, its bash expansion, verdict of the expansion today)
const CASES: [(&str, &str, &str); 5] = [
    ("git reset {--hard,HEAD}", "git reset --hard HEAD", "deny"),
    ("git clean {-f,-x}", "git clean -f -x", "ask"),
    (
        "git push {--force,origin} main",
        "git push --force origin main",
        "ask",
    ),
    ("git checkout {--,.}", "git checkout -- .", "deny"),
    ("git branch {-D,foo}", "git branch -D foo", "ask"),
];

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("detector-brace-flag-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("work")).unwrap();
        let st = Command::new("git")
            .args(["init", "-q"])
            .current_dir(base.join("work"))
            .status()
            .unwrap();
        assert!(st.success());
        Fx { _dir: dir, base }
    }
}

fn bash_expand(cmd: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("echo {cmd}"))
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn verdict(fx: &Fx, cmd: &str) -> String {
    let cwd = fx.base.join("work");
    let payload = serde_json::json!({
        "session_id": "detector-brace-flag",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", fx.base.join("home"))
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&fx.base);
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
        .to_string()
}

#[test]
fn control_expansions_are_what_bash_runs() {
    for (braced, expanded, _) in CASES {
        assert_eq!(bash_expand(braced), expanded, "fixture: {braced}");
    }
}

#[test]
fn control_expanded_spelling_keeps_its_verdict() {
    let fx = Fx::new();
    for (_, expanded, want) in CASES {
        assert_eq!(verdict(&fx, expanded), want, "{expanded}");
    }
}

#[test]
fn control_harmless_braces_are_allow() {
    let fx = Fx::new();
    for c in [
        "git status {a,b}",
        "git clean {-n,-x}",
        "git reset {--soft,HEAD}",
        "echo {a,b}",
    ] {
        assert_eq!(verdict(&fx, c), "allow", "over-block: {c}");
    }
}

#[test]
#[ignore = "RED(66e7a741): pending"]
fn braced_destructive_git_flag_matches_expanded_verdict() {
    let fx = Fx::new();
    let bad: Vec<String> = CASES
        .iter()
        .map(|(braced, _, want)| (verdict(&fx, braced), *braced, *want))
        .filter(|(got, _, want)| got != want)
        .map(|(got, braced, want)| format!("{braced} => {got} (expanded: {want})"))
        .collect();
    assert!(
        bad.is_empty(),
        "braced spelling is weaker than its expansion:\n{}",
        bad.join("\n")
    );
}
