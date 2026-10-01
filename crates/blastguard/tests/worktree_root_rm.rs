// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Recursive `rm` strictly inside a worktree storage root is allowed with no
//! confirmation (user ruling 2026-09-30); everything else about `rm -r` is
//! unchanged.
//!
//! Worktree storage roots: `$HOME/.condukt/worktrees`, and for each of the
//! payload `cwd` / env `CLAUDE_PROJECT_DIR` the `.harness-worktrees` directory
//! (the prefix up to that component if present, else `dirname(P)/.harness-worktrees`).
//!
//! The positive cases assert Allow (empty hook stdout). The controls pin what
//! must NOT become Allow: the root itself, globs, `..` escapes, symlink escapes,
//! mixed operands, prefix-lookalike siblings, protected paths.
//!
//! The base is [`neutral_base::neutral_base`]: a location calibrated so that
//! neither an enclosing real `.harness-worktrees` (which would turn everything
//! into Allow) nor a blastguard temp safe root such as `/tmp` (which would turn
//! everything into an Ask) decides the verdict. `TMPDIR` is removed from the
//! hook's environment, so Allow is distinguishable from the pre-feature Deny.

#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    hw: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("worktree_root_rm")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let f = Fixture {
            home: base.join("home"),
            proj: base.join("src/proj"),
            hw: base.join("src/.harness-worktrees"),
            base,
        };
        for d in [
            f.home.join(".condukt/worktrees/wt1/sub"),
            f.home.join(".condukt/worktrees-evil/x"),
            f.proj.clone(),
            f.hw.join("session-x/sub"),
            f.hw.join("session-y"),
            f.base.join("outside/victim"),
            f.home.join(".condukt/victim"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(f.home.join(".condukt/worktrees/wt1/sub/f"), "x").unwrap();
        std::fs::write(f.hw.join("session-x/sub/f"), "x").unwrap();
        std::fs::write(f.base.join("outside/victim/f"), "x").unwrap();
        std::os::unix::fs::symlink(
            f.base.join("outside/victim"),
            f.home.join(".condukt/worktrees/link"),
        )
        .unwrap();
        f
    }

    fn h(&self) -> String {
        self.home.display().to_string()
    }

    /// Hook stdout for `command` with the given cwd and CLAUDE_PROJECT_DIR.
    fn run(&self, command: &str, cwd: &Path, project_dir: &Path) -> String {
        let payload = serde_json::json!({
            "session_id": "worktree-root-rm",
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", project_dir)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn run_default(&self, command: &str) -> String {
        self.run(command, &self.proj, &self.proj)
    }
}

fn is_allow(out: &str) -> bool {
    out.trim().is_empty()
}
fn is_deny(out: &str) -> bool {
    out.contains(r#""permissionDecision":"deny""#)
}

#[track_caller]
fn assert_allow(case: &str, out: &str) {
    assert!(
        is_allow(out),
        "{case}: must be Allow (no output); got {out}"
    );
}
#[track_caller]
fn assert_deny(case: &str, out: &str) {
    assert!(is_deny(out), "{case}: must be Deny; got {out}");
}
#[track_caller]
fn assert_not_allow(case: &str, out: &str) {
    assert!(
        !is_allow(out),
        "{case}: must NOT be Allow; got empty output"
    );
}

// ---- positive cases -------------------------------------------------------

#[test]
fn allow_rm_rf_inside_condukt_worktrees() {
    let f = Fixture::new("pos_condukt");
    let out = f.run_default(&format!("rm -rf {}/.condukt/worktrees/wt1", f.h()));
    assert_allow("rm -rf $HOME/.condukt/worktrees/wt1", &out);
}

#[test]
fn allow_rm_r_inside_harness_worktrees_sibling_of_project() {
    let f = Fixture::new("pos_hw");
    let out = f.run_default(&format!("rm -r {}", f.hw.join("session-x").display()));
    assert_allow("rm -r <base>/src/.harness-worktrees/session-x", &out);
}

#[test]
fn allow_rm_rf_sibling_session_from_inside_a_session_worktree() {
    let f = Fixture::new("pos_hw_from_wt");
    let sy = f.hw.join("session-y");
    let out = f.run(
        &format!("rm -rf {}", f.hw.join("session-x").display()),
        &sy,
        &sy,
    );
    assert_allow("session-y (cwd+project) deleting sibling session-x", &out);
}

// ---- negative controls ----------------------------------------------------

#[test]
fn deny_condukt_worktrees_root_itself() {
    let f = Fixture::new("neg_root_condukt");
    let out = f.run_default(&format!("rm -rf {}/.condukt/worktrees", f.h()));
    assert_deny("storage root $HOME/.condukt/worktrees itself", &out);
}

#[test]
fn deny_harness_worktrees_root_itself() {
    let f = Fixture::new("neg_root_hw");
    let out = f.run_default(&format!("rm -rf {}", f.hw.display()));
    assert_deny("storage root <base>/src/.harness-worktrees itself", &out);
}

#[test]
fn deny_glob_operand() {
    let f = Fixture::new("neg_glob");
    let out = f.run_default(&format!("rm -rf {}/.condukt/worktrees/*", f.h()));
    assert_deny("glob operand worktrees/*", &out);
}

#[test]
fn deny_dotdot_escape() {
    let f = Fixture::new("neg_dotdot");
    let out = f.run_default(&format!("rm -rf {}/.condukt/worktrees/../victim", f.h()));
    assert_deny("`..` escape worktrees/../victim", &out);
}

#[test]
fn not_allow_symlink_escape() {
    let f = Fixture::new("neg_symlink");
    let out = f.run_default(&format!("rm -rf {}/.condukt/worktrees/link", f.h()));
    assert_not_allow("symlink inside root pointing outside", &out);
}

#[test]
fn deny_mixed_operands() {
    let f = Fixture::new("neg_mixed");
    let out = f.run_default(&format!(
        "rm -rf {}/.condukt/worktrees/wt1 {}",
        f.h(),
        f.base.join("outside/victim").display()
    ));
    assert_deny("mixed: one inside worktree root + one outside", &out);
}

#[test]
fn deny_prefix_lookalike_sibling() {
    let f = Fixture::new("neg_prefix");
    let out = f.run_default(&format!("rm -rf {}/.condukt/worktrees-evil/x", f.h()));
    assert_deny("lookalike sibling worktrees-evil/x", &out);
}

#[test]
fn protected_path_inside_worktree_still_stopped() {
    let f = Fixture::new("neg_protected");
    let dir = f.home.join(".condukt/worktrees/wt1/x/.githooks");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pre-commit"), "#!/bin/sh\n").unwrap();
    let out = f.run_default(&format!("rm -rf {}", dir.display()));
    assert_not_allow("protected <worktree>/x/.githooks", &out);
    assert_deny("protected <worktree>/x/.githooks", &out);
}
