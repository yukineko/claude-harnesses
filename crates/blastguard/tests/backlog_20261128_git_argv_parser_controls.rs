#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Controls for the shared git argv parser (backlog 4e7fc8e2 refactor): the
//! ordinary spellings of the rewritten subcommands stay Allow, and the
//! undetermined readings that touch a protected path are not Allow. Written by
//! the implementer as regression controls only; the independent RED lives in
//! `verifier_4e7fc8e2_fix_20261128.rs`.
//!
//! DOES NOT PROVE: that git really writes (nothing is executed), real `$HOME`.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::Path;
use std::process::Command;

fn verdict(base: &Path, cwd: &Path, cmd: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "controls-git-argv",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .env("HOME", base.join("home"))
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", base.join("store"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(base);
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0));
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
        .to_string()
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    std::fs::create_dir_all("/private/tmp/claude-wt-verify").unwrap();
    let dir = tempfile::Builder::new()
        .prefix("argv-ctl-")
        .tempdir_in("/private/tmp/claude-wt-verify")
        .unwrap();
    let base = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(base.join("home/.claude/hooks")).unwrap();
    std::fs::create_dir_all(base.join("work/sub")).unwrap();
    (dir, base)
}

#[test]
fn ordinary_spellings_stay_allow() {
    let (_d, base) = fixture();
    let work = base.join("work");
    let plain = [
        "git init",
        "git init -q --bare -b main r1",
        "git init -- r2",
        "git -C sub init",
        "git clone -q --depth 1 https://example.invalid/x.git",
        "git clone --branch main --single-branch https://example.invalid/x.git out",
        "git clone https://example.invalid/x.git",
        "git format-patch -1 --stdout",
        "git format-patch -3 -o patches --no-stat",
        "git format-patch -ko patches -2",
        "git worktree add --lock --reason r ../w1",
        "git worktree list --porcelain",
        "git config --get core.editor",
        "git config --file .gitmodules --get x.y",
        "git config user.name x",
        "git checkout-index -a -f",
        "git log --oneline -5",
        "git diff --stat",
        "git apply --check p.patch",
        "git apply -p1 p.patch",
    ];
    for c in plain {
        assert_eq!(verdict(&base, &work, c), "allow", "{c}");
    }
}

#[test]
fn operandless_init_in_a_project_root_holding_dot_claude_stays_allow() {
    let (_d, base) = fixture();
    let proj = base.join("proj");
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    std::fs::write(proj.join(".claude/settings.json"), "{}").unwrap();
    for c in ["git init", "git init -q", "git -C . init"] {
        assert_eq!(verdict(&base, &proj, c), "allow", "{c}");
    }
}

#[test]
fn unreadable_line_with_a_protected_word_is_not_allow() {
    let (_d, base) = fixture();
    let work = base.join("work");
    let h = base.join("home/.claude/hooks").display().to_string();
    for c in [
        format!("git init --brand-new-flag {h}/g"),
        format!("git clone --sh {h}/c"),
        format!("git worktree add --zzz {h}/w"),
        format!("git config --zzz --file {h}/cfg k v"),
    ] {
        assert_ne!(verdict(&base, &work, &c), "allow", "{c}");
    }
}

#[test]
fn work_tree_checkout_index_without_prefix_is_judged() {
    let (_d, base) = fixture();
    let work = base.join("work");
    let wt = base.join("home/.claude").display().to_string();
    let c = format!("git --work-tree={wt} checkout-index -a -f");
    assert_ne!(verdict(&base, &work, &c), "allow", "{c}");
}
