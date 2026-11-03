// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Intentionally RED reproduction for backlog 03885f11 (measured 2026-11-03).
//!
//! A plain file write to `.git/config` can set `core.hooksPath` and thereby
//! disable every local gate, exactly like `git config core.hooksPath /tmp`
//! (which blastguard refuses). Yet blastguard allows the file-write spellings:
//! `PROTECTED_GLOBS` in `src/exclude.rs` lists `.git/hooks/**` but not
//! `.git/config`.
//!
//! Each protection case asserts the verdict is NOT allow (deny and ask are
//! both acceptable). Each spelling is its own test so a RED run shows every
//! case. Controls pin what a fix must NOT over-block: reading `.git/config`,
//! `git config --get`, and writing an unrelated file whose name merely
//! contains `git-config`.
//!
//! A sanity pair (`.git/hooks/pre-commit` write is refused in the same
//! fixture) proves the fixture actually exercises the protected-path rule, so
//! a fixture that silently stopped reaching it cannot make the controls pass
//! vacuously.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.
//! Do not weaken these assertions to match current behaviour.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(20);

struct Fx {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Fx {
    /// A real git repo at `<base>/proj` (so `.git/config` exists) plus a real
    /// nested repo at `<base>/proj/sub`.
    fn new(name: &str) -> Fx {
        let root = Path::new("/var/tmp").join(format!(
            "bg-git-config-write-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let base = std::fs::canonicalize(&root).unwrap();
        let f = Fx {
            home: base.join("home"),
            repo: base.join("proj"),
            base,
        };
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::create_dir_all(&f.repo).unwrap();
        f.git(&f.repo, &["init", "-q", "-b", "main"]);
        let sub = f.repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        f.git(&sub, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(f.repo.join("notes")).unwrap();
        assert!(f.repo.join(".git/config").is_file(), "fixture: .git/config");
        assert!(sub.join(".git/config").is_file(), "fixture: sub/.git/config");
        f
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.home);
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn bash(&self, command: &str) -> String {
        self.call("Bash", serde_json::json!({ "command": command }))
    }

    fn call(&self, tool: &str, input: serde_json::Value) -> String {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "git-config-write",
            "cwd": self.repo.display().to_string(),
            "tool_name": tool,
            "tool_input": input,
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("TMPDIR")
            .current_dir(&self.repo);
        let Output {
            status,
            stdout,
            stderr,
        } = bounded::run_bounded(&mut cmd, payload.as_bytes(), CALL_DEADLINE);
        assert_eq!(
            status.code(),
            Some(0),
            "hook must exit 0 ({tool} {input:?}); stderr: {}",
            String::from_utf8_lossy(&stderr)
        );
        String::from_utf8_lossy(&stdout).into_owned()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn decision(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "unparsed"
    }
}

/// Not allowed means an explicit deny or ask; an unparsed output is neither
/// and fails too (it is not evidence of a refusal).
fn assert_refused(out: &str, what: &str) {
    let d = decision(out);
    assert!(
        d == "deny" || d == "ask",
        "{what} must not be allowed (deny or ask); got {d}: {out:?}"
    );
}

fn assert_allowed(out: &str, what: &str) {
    assert_eq!(
        decision(out),
        "allow",
        "{what} is a control that must stay allowed; got {out:?}"
    );
}

// ---------------------------------------------------------------- protection

#[test]
fn bash_append_redirect_to_git_config_is_not_allowed() {
    let f = Fx::new("append");
    let cmd = "echo x >> .git/config";
    assert_refused(&f.bash(cmd), cmd);
}

#[test]
fn bash_python_c_append_hookspath_to_git_config_is_not_allowed() {
    let f = Fx::new("pyc");
    let cmd = "python3 -c \"open('.git/config','a').write('[core]\\n\\thooksPath = /tmp\\n')\"";
    assert_refused(&f.bash(cmd), cmd);
}

#[test]
fn bash_printf_truncate_write_to_git_config_is_not_allowed() {
    let f = Fx::new("trunc");
    let cmd = "printf '[core]\\n' > .git/config";
    assert_refused(&f.bash(cmd), cmd);
}

#[test]
fn write_tool_on_git_config_is_not_allowed() {
    let f = Fx::new("write");
    let p = f.repo.join(".git/config");
    let out = f.call(
        "Write",
        serde_json::json!({
            "file_path": p.display().to_string(),
            "content": "[core]\n\thooksPath = /tmp\n",
        }),
    );
    assert_refused(&out, &format!("Write {}", p.display()));
}

#[test]
fn edit_tool_on_git_config_is_not_allowed() {
    let f = Fx::new("edit");
    let p = f.repo.join(".git/config");
    let out = f.call(
        "Edit",
        serde_json::json!({
            "file_path": p.display().to_string(),
            "old_string": "[core]",
            "new_string": "[core]\n\thooksPath = /tmp",
        }),
    );
    assert_refused(&out, &format!("Edit {}", p.display()));
}

#[test]
fn bash_append_redirect_to_nested_repo_git_config_is_not_allowed() {
    let f = Fx::new("nested");
    let cmd = "echo x >> sub/.git/config";
    assert_refused(&f.bash(cmd), cmd);
}

// ------------------------------------------------------------------ sanity

/// The same fixture must reach the protected-path rule for `.git/hooks/**`;
/// otherwise the controls below could pass because nothing is ever refused.
#[test]
fn sanity_bash_write_to_git_hooks_is_refused_in_this_fixture() {
    let f = Fx::new("sanity");
    let cmd = "echo x > .git/hooks/pre-commit";
    assert_refused(&f.bash(cmd), cmd);
}

// ---------------------------------------------------------------- controls

#[test]
fn control_cat_git_config_stays_allowed() {
    let f = Fx::new("ctl-cat");
    let cmd = "cat .git/config";
    assert_allowed(&f.bash(cmd), cmd);
}

#[test]
fn control_git_config_get_hookspath_stays_allowed() {
    let f = Fx::new("ctl-get");
    let cmd = "git config --get core.hooksPath";
    assert_allowed(&f.bash(cmd), cmd);
}

#[test]
fn control_append_to_unrelated_git_config_named_file_stays_allowed() {
    let f = Fx::new("ctl-notes");
    let cmd = "echo x >> notes/git-config.txt";
    assert_allowed(&f.bash(cmd), cmd);
}
