#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: a command wrapper (`xcrun`, `watch`, `caffeinate`, `arch`, `chronic`,
//! `script -q /dev/null`, `unbuffer`) in front of `git add -A` in the MAIN
//! working tree is weaker than the bare `git add -A`.
//!
//! Ruling: a whole-tree stage in the main working tree is Ask (memory
//! `condukt-worker-dirty-tree-commit`, `audit_b_memory_rules_20261107.rs`
//! `whole_tree_stage_or_commit_all_is_ask`): it sweeps foreign WIP from
//! other sessions into the commit. Each wrapper runs its argv as a child
//! command, so the wrapped form stages the same files.
//!
//! Controls (GREEN): bare `git add -A` / `git add --all` is Ask; an
//! explicit-path stage is Allow both bare and wrapped (no over-block);
//! `rm -rf /etc` is Deny bare and not Allow wrapped (the wrappers are not
//! blindly transparent).
//!
//! Harness: payload `cwd` = `CLAUDE_PROJECT_DIR` = a git repo on `main` with
//! a dirty tracked file and an untracked file, under
//! `/private/tmp/claude-askrules/` (not `/var/folders`, a system directory
//! for blastguard); hook `HOME` is a fixture dir beside it.
//!
//! DOES NOT PROVE: subagent mode; `git commit -a` behind the same wrappers;
//! wrappers with option values other than the ones listed.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const WRAPPERS: [&str; 7] = [
    "xcrun",
    "watch",
    "caffeinate",
    "arch",
    "chronic",
    "script -q /dev/null",
    "unbuffer",
];

fn base() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let b = PathBuf::from(format!(
        "/private/tmp/claude-askrules/detector-wrapper-stage-{}-{nanos}-{}",
        std::process::id(),
        // libtest names each test thread after its test: unique per test.
        std::thread::current().name().unwrap_or("main")
    ));
    std::fs::create_dir_all(b.join("home")).unwrap();
    let repo = b.join("src/repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    // Foreign WIP a whole-tree stage would sweep in.
    std::fs::write(repo.join("tracked.txt"), "foreign wip\n").unwrap();
    std::fs::write(repo.join("foreign.txt"), "foreign untracked\n").unwrap();
    b
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn verdict(b: &Path, cmd: &str) -> String {
    let repo = b.join("src/repo");
    let payload = serde_json::json!({
        "session_id": "detector-wrapper-stage",
        "hook_event_name": "PreToolUse",
        "cwd": repo,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", b.join("home"))
        .env("CLAUDE_PROJECT_DIR", &repo)
        .env("BLASTGUARD_APPROVALS_DIR", b.join("store"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(b);
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
fn control_bare_whole_tree_stage_is_ask() {
    let b = base();
    for c in ["git add -A", "git add --all"] {
        assert_eq!(verdict(&b, c), "ask", "{c}");
    }
}

#[test]
fn control_wrapped_explicit_path_stage_is_allow() {
    let b = base();
    assert_eq!(verdict(&b, "git add tracked.txt"), "allow");
    for w in WRAPPERS {
        let c = format!("{w} git add tracked.txt");
        assert_eq!(verdict(&b, &c), "allow", "over-block: {c}");
    }
}

/// The wrappers are not blindly transparent-Allow: a wrapped `rm -rf /etc`
/// is refused. (Measured 2026-11-28: Ask, not the bare form's Deny — that
/// gap is its own RED, `backlog_20261128_detector_wrapper_system_harm_deny.rs`;
/// this control only pins "not Allow".)
#[test]
fn control_wrapped_system_harm_is_refused() {
    let b = base();
    assert_eq!(verdict(&b, "rm -rf /etc"), "deny");
    for w in WRAPPERS {
        let c = format!("{w} rm -rf /etc");
        assert_ne!(verdict(&b, &c), "allow", "{c}");
    }
}

#[test]
#[ignore = "RED(3221b79b): pending-id"]
fn wrapped_whole_tree_stage_in_main_tree_is_ask() {
    let b = base();
    let mut bad = Vec::new();
    for stage in ["git add -A", "git add --all"] {
        for w in WRAPPERS {
            let c = format!("{w} {stage}");
            let v = verdict(&b, &c);
            if v == "allow" {
                bad.push(format!("allow (bare is ask): {c}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} wrapped whole-tree stages weaker than bare:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
