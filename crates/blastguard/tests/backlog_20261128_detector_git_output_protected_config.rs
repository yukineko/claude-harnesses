#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: writes to protected Claude config through non-`>` writers are Allow.
//!
//! `git diff --output=<f>` / `git log --output=<f>` truncate and write `<f>`;
//! pointed at `~/.claude/settings.json` (or a file under `~/.claude/hooks/`)
//! they overwrite the protected config. `git worktree add ~/.claude/hooks/w`
//! creates a checkout inside the protected hooks directory. `touch` on a
//! DANGLING symlink in cwd whose target is `~/.claude/hooks/<new>` creates
//! the protected file through the link.
//! Measured 2026-11-28 on main f60cd1d6e: every case below is Allow.
//! `git diff --output <f>` (separate arg), `git log -1 --output=<f>` and
//! `git show --output=<f>` were checked with real git to overwrite `<f>`.
//! NOT reproduced (removed): `cp payload dlink` / `cp <abs>/payload
//! <abs>/dlink` through the same dangling link were already not Allow.
//!
//! Controls (GREEN): `cp payload ~/.claude/settings.json` is not Allow;
//! `git diff --output=out.patch` (plain file in cwd) is Allow.
//!
//! Harness: fixture under `/private/tmp/claude-askrules/`; cwd is a git repo
//! with one commit; hook `HOME` is the fixture home holding
//! `.claude/settings.json` and `.claude/hooks/`.
//!
//! DOES NOT PROVE: real `$HOME`; `~`-spelled paths (tilde is not expanded by
//! git inside `--output=`); Write/Edit tool paths; subagent mode; that the
//! commands actually write (git behaviour is assumed, not executed here,
//! except that the dangling-link fixture is checked to resolve into hooks/).
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("detector-git-output-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".claude/hooks")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        let work = base.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("plain.txt"), "x").unwrap();
        std::fs::write(work.join("payload"), "{\"hooks\":{}}").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "plain.txt", "payload"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ][..],
        ] {
            let st = git_env::pinned(&work).args(args).status().unwrap();
            assert!(st.success(), "fixture git {args:?}");
        }
        // Dangling symlink: target does not exist yet, lives under hooks/.
        symlink(home.join(".claude/hooks/new.sh"), work.join("dlink")).unwrap();
        assert!(
            std::fs::symlink_metadata(work.join("dlink")).is_ok() && !work.join("dlink").exists(),
            "fixture: dlink must be a dangling symlink"
        );
        Fx { _dir: dir, base }
    }
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
}

fn verdict(fx: &Fx, cmd: &str) -> String {
    let cwd = fx.work();
    let payload = serde_json::json!({
        "session_id": "detector-git-output",
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
        .env("HOME", fx.home())
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

fn allowed(fx: &Fx, cmds: Vec<String>) -> Vec<String> {
    cmds.into_iter()
        .map(|c| (verdict(fx, &c), c))
        .filter(|(v, _)| v == "allow")
        .map(|(v, c)| format!("{c} => {v}"))
        .collect()
}

#[test]
fn control_direct_cp_to_protected_config_is_not_allow() {
    let fx = Fx::new();
    let c = format!(
        "cp {}/payload {}/.claude/settings.json",
        fx.work().display(),
        fx.home().display()
    );
    assert_ne!(verdict(&fx, &c), "allow", "{c}");
}

#[test]
fn control_git_diff_output_to_plain_file_is_allow() {
    let fx = Fx::new();
    for c in [
        "git diff --output=out.patch".to_string(),
        format!("git diff --output={}/out.patch", fx.work().display()),
    ] {
        assert_eq!(verdict(&fx, &c), "allow", "over-block: {c}");
    }
}

#[test]
#[ignore = "RED: backlog 96faed52"]
fn git_output_option_to_protected_config_is_not_allow() {
    let fx = Fx::new();
    let s = fx.home().join(".claude/settings.json");
    let hk = fx.home().join(".claude/hooks/pre.sh");
    let bad = allowed(
        &fx,
        vec![
            format!("git diff --output={}", s.display()),
            format!("git log --output={}", s.display()),
            format!("git diff --output {}", s.display()),
            format!("git show --output={}", s.display()),
            format!("git diff --output={}", hk.display()),
        ],
    );
    assert!(
        bad.is_empty(),
        "these overwrite protected Claude config but are Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
#[ignore = "RED: backlog 96faed52"]
fn git_worktree_add_under_protected_hooks_is_not_allow() {
    let fx = Fx::new();
    let bad = allowed(
        &fx,
        vec![format!(
            "git worktree add {}/.claude/hooks/w",
            fx.home().display()
        )],
    );
    assert!(
        bad.is_empty(),
        "worktree inside ~/.claude/hooks is Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
#[ignore = "RED: backlog 96faed52"]
fn dangling_symlink_write_into_protected_hooks_is_not_allow() {
    let fx = Fx::new();
    let w = fx.work();
    let bad = allowed(
        &fx,
        vec![
            "touch dlink".to_string(),
            format!("touch {}/dlink", w.display()),
        ],
    );
    assert!(
        bad.is_empty(),
        "dangling link -> ~/.claude/hooks/new.sh write is Allow:\n{}",
        bad.join("\n")
    );
}
