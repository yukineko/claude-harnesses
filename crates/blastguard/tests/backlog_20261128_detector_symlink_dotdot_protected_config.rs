#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: deleting `~/.claude/settings.json` through `<symlink>/..` is not Deny.
//!
//! `hlink -> ~/.claude/hooks`. The kernel resolves `hlink/..` physically, so
//! `rm -f <cwd>/hlink/../settings.json` unlinks `~/.claude/settings.json`
//! (the fixture proves it with `canonicalize`). Collapsing `..` lexically
//! first turns the path into `<cwd>/settings.json`, a harmless-looking file.
//! Measured 2026-11-28 on main 6cc6d735d: `rm -f` form is Allow, `rm -rf`
//! form is Ask; the direct spelling of the same file is Deny for both.
//! The relative spelling (`hlink/../settings.json`, cwd = the symlink's
//! parent) gives the same Allow / Ask. Pointing `HOME` at an unrelated
//! directory changed nothing (the direct spelling stays Deny, the symlink
//! spelling stays Allow / Ask), so the HOME location is not a factor here.
//!
//! Controls (GREEN): direct `rm -f` / `rm -rf` of the protected file is Deny;
//! deleting a plain file in cwd, also through a non-symlink `sub/..`, is Allow.
//!
//! Harness: fixture under `/private/tmp/claude-askrules/`; hook `HOME` is the
//! fixture home holding `.claude/settings.json` and `.claude/hooks/`.
//!
//! DOES NOT PROVE: other protected files; symlinks reached through
//! variables/substitutions; Write/Edit tool paths; subagent mode.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("detector-symlink-dotdot-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".claude/hooks")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(base.join("work/sub")).unwrap();
        std::fs::write(base.join("work/plain.txt"), "x").unwrap();
        symlink(home.join(".claude/hooks"), base.join("work/hlink")).unwrap();
        let fx = Fx { _dir: dir, base };
        // Prove the fixture: the symlink spelling really names the protected file.
        assert_eq!(
            std::fs::canonicalize(fx.work().join("hlink/../settings.json")).unwrap(),
            std::fs::canonicalize(home.join(".claude/settings.json")).unwrap(),
            "fixture: hlink/.. must resolve physically to ~/.claude"
        );
        fx
    }
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
}

fn verdict(fx: &Fx, home: &Path, cmd: &str) -> String {
    let cwd = fx.work();
    let payload = serde_json::json!({
        "session_id": "detector-symlink-dotdot",
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
        .env("HOME", home)
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
fn control_direct_protected_config_delete_is_deny() {
    let fx = Fx::new();
    let h = fx.home();
    for c in [
        format!("rm -f {}/.claude/settings.json", h.display()),
        format!("rm -rf {}/.claude/settings.json", h.display()),
    ] {
        assert_eq!(verdict(&fx, &h, &c), "deny", "{c}");
    }
}

#[test]
fn control_plain_delete_in_cwd_is_allow() {
    let fx = Fx::new();
    let h = fx.home();
    let w = fx.work();
    for c in [
        format!("rm -f {}/plain.txt", w.display()),
        format!("rm -f {}/sub/../plain.txt", w.display()),
        "rm -f plain.txt".to_string(),
    ] {
        assert_eq!(verdict(&fx, &h, &c), "allow", "over-block: {c}");
    }
}

#[test]
fn symlink_dotdot_protected_config_delete_is_deny() {
    let fx = Fx::new();
    let h = fx.home();
    let w = fx.work();
    let bad: Vec<String> = [
        format!("rm -f {}/hlink/../settings.json", w.display()),
        format!("rm -rf {}/hlink/../settings.json", w.display()),
        "rm -f hlink/../settings.json".to_string(),
        "rm -rf hlink/../settings.json".to_string(),
    ]
    .into_iter()
    .map(|c| (verdict(&fx, &h, &c), c))
    .filter(|(v, _)| v != "deny")
    .map(|(v, c)| format!("{c} => {v}"))
    .collect();
    assert!(
        bad.is_empty(),
        "symlink/.. reaches ~/.claude/settings.json but is not Deny:\n{}",
        bad.join("\n")
    );
}
