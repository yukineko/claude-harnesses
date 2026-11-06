// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction (measured 2026-11-07, deployed blastguard 0.2.276).
//!
//! `PROTECTED_GLOBS` in `src/exclude.rs` names only `.git/config` and
//! `**/.git/config`. The sibling files git also reads as repo config
//! (`.git/config.worktree`, `.git/worktrees/NAME/config`,
//! `.git/worktrees/NAME/config.worktree`) can set `core.hooksPath` just the
//! same, yet shell writes to them (echo `>`, echo `>>`, `cp`, `tee`) produce
//! no decision. The Write tool is already denied for all of them.
//!
//! 12 targets (4 spellings x 3 twins), one test each so RED shows each.
//! Controls: shell write to `.git/config` stays non-allow; reads (`cat`,
//! `git config --file PATH --get`) of every twin stay allow; a write to an
//! ordinary `notes.txt` keeps its measured verdict. Sanity: a `.git/hooks`
//! write is refused in this same fixture.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)); `src/` untouched.
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
    /// A real git repo at `<base>/proj` plus the config twins under `.git/`.
    fn new(name: &str) -> Fx {
        Fx::build(name, true)
    }

    /// Same, but the twin config files are absent (parent dirs exist). A
    /// truncating redirect or tee to an EXISTING file is already asked/denied
    /// by a generic overwrite rule (measured 2026-11-07), which would mask
    /// the missing protected-path rule; the absent state is the one the issue
    /// measured as allow.
    fn new_absent(name: &str) -> Fx {
        Fx::build(name, false)
    }

    fn build(name: &str, twins_exist: bool) -> Fx {
        let root = Path::new("/var/tmp")
            .join(format!("bg-git-config-twins-{}-{name}", std::process::id()));
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
        std::fs::create_dir_all(f.repo.join("notes")).unwrap();
        std::fs::write(f.repo.join("notes/x"), "x\n").unwrap();
        let wt = f.repo.join(".git/worktrees/NAME");
        std::fs::create_dir_all(&wt).unwrap();
        if twins_exist {
            std::fs::write(f.repo.join(".git/config.worktree"), "[core]\n").unwrap();
            std::fs::write(wt.join("config"), "[core]\n").unwrap();
            std::fs::write(wt.join("config.worktree"), "[core]\n").unwrap();
        }
        assert!(f.repo.join(".git/config").is_file(), "fixture: .git/config");
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
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "git-config-twins",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
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
            "hook must exit 0 ({command:?}); stderr: {}",
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

const TWINS: [&str; 3] = [
    ".git/config.worktree",
    ".git/worktrees/NAME/config",
    ".git/worktrees/NAME/config.worktree",
];

macro_rules! target {
    ($name:ident, $tag:literal, $cmd:literal) => {
        #[test]
        #[ignore = "RED 2026-11-07: PROTECTED_GLOBS names only .git/config; config twins writable from shell, remove ignore when fixed"]
        fn $name() {
            let f = Fx::new($tag);
            assert_refused(&f.bash($cmd), $cmd);
        }
    };
    (absent $name:ident, $tag:literal, $cmd:literal) => {
        #[test]
        #[ignore = "RED 2026-11-07: PROTECTED_GLOBS names only .git/config; config twins writable from shell, remove ignore when fixed"]
        fn $name() {
            let f = Fx::new_absent($tag);
            assert_refused(&f.bash($cmd), $cmd);
        }
    };
}

// ---------------------------------------------------------------- targets

target!(absent echo_overwrite_config_worktree, "eo1", "echo x > .git/config.worktree");
target!(absent echo_overwrite_wt_config, "eo2", "echo x > .git/worktrees/NAME/config");
target!(
    absent echo_overwrite_wt_config_worktree,
    "eo3",
    "echo x > .git/worktrees/NAME/config.worktree"
);
target!(
    echo_append_config_worktree,
    "ea1",
    "echo x >> .git/config.worktree"
);
target!(
    echo_append_wt_config,
    "ea2",
    "echo x >> .git/worktrees/NAME/config"
);
target!(
    echo_append_wt_config_worktree,
    "ea3",
    "echo x >> .git/worktrees/NAME/config.worktree"
);
target!(
    cp_into_config_worktree,
    "cp1",
    "cp notes/x .git/config.worktree"
);
target!(
    cp_into_wt_config,
    "cp2",
    "cp notes/x .git/worktrees/NAME/config"
);
target!(
    cp_into_wt_config_worktree,
    "cp3",
    "cp notes/x .git/worktrees/NAME/config.worktree"
);
target!(absent tee_config_worktree, "tee1", "echo x | tee .git/config.worktree");
target!(absent tee_wt_config, "tee2", "echo x | tee .git/worktrees/NAME/config");
target!(
    absent tee_wt_config_worktree,
    "tee3",
    "echo x | tee .git/worktrees/NAME/config.worktree"
);

// ------------------------------------------------------------------ sanity

/// The same fixture must reach the protected-path rule for `.git/hooks/**`.
#[test]
fn sanity_bash_write_to_git_hooks_is_refused_in_this_fixture() {
    let f = Fx::new("sanity");
    let cmd = "echo x > .git/hooks/pre-commit";
    assert_refused(&f.bash(cmd), cmd);
}

// ---------------------------------------------------------------- controls

#[test]
fn control_shell_write_to_git_config_stays_refused() {
    let f = Fx::new("ctl-cfg");
    let cmd = "echo x > .git/config";
    assert_refused(&f.bash(cmd), cmd);
}

#[test]
fn control_cat_of_every_twin_stays_allowed() {
    let f = Fx::new("ctl-cat");
    for t in TWINS {
        let cmd = format!("cat {t}");
        assert_allowed(&f.bash(&cmd), &cmd);
    }
}

#[test]
fn control_git_config_file_get_of_every_twin_stays_allowed() {
    let f = Fx::new("ctl-get");
    for t in TWINS {
        let cmd = format!("git config --file {t} --get core.hooksPath");
        assert_allowed(&f.bash(&cmd), &cmd);
    }
}

/// Verdict measured on current code (2026-11-07) before pinning.
#[test]
fn control_write_to_ordinary_notes_txt_keeps_current_verdict() {
    let f = Fx::new("ctl-notes");
    let cmd = "echo x > notes.txt";
    assert_allowed(&f.bash(cmd), cmd);
}
