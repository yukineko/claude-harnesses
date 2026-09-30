#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A recursive `rm` is allowed inside a worktree storage root only when the
//! `rm` really runs with its cwd inside that root. A `cd` earlier in the command
//! line does NOT reliably move the `rm`: a backgrounded `cd &` runs in a
//! subshell, a failed `cd ;` leaves the cwd unchanged, `cd || rm` runs the `rm`
//! only when the `cd` FAILED, and `cd | rm` puts each side in its own subshell.
//! In every such spelling the relative operand `src` resolves against the
//! payload cwd (`<parent>/proj`, which is NOT a root), so it must NOT be Allow.
//!
//! Fixtures live under `CARGO_TARGET_TMPDIR` (not `/tmp`, a blastguard safe root).

use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    parent: PathBuf,
    proj: PathBuf,
}

impl Fx {
    /// `<parent>/.harness-worktrees/s/src` (real), `<parent>/proj/src` (real),
    /// cwd for `run` defaults to `<parent>/proj`.
    fn new(name: &str) -> Fx {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("worktree_root_cd_desync")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let f = Fx {
            home: base.join("home"),
            parent: base.join("src"),
            proj: base.join("src/proj"),
            base,
        };
        std::fs::create_dir_all(f.home.clone()).unwrap();
        std::fs::create_dir_all(f.proj.join("src")).unwrap();
        std::fs::create_dir_all(f.hw().join("s/src")).unwrap();
        f
    }

    fn hw(&self) -> PathBuf {
        self.parent.join(".harness-worktrees")
    }

    fn run(&self, command: &str) -> String {
        self.run_in(&self.proj, command)
    }

    fn run_in(&self, cwd: &Path, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "worktree-root-cd-desync",
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", cwd)
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
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        println!("[{command}] cwd={} => {:?}", cwd.display(), s.trim());
        s
    }
}

fn is_allow(out: &str) -> bool {
    out.trim().is_empty()
}

#[track_caller]
fn assert_not_allow(case: &str, out: &str) {
    assert!(
        !is_allow(out),
        "{case}: must NOT be Allow; got empty output (= Allow)"
    );
}
#[track_caller]
fn assert_allow(case: &str, out: &str) {
    assert!(
        is_allow(out),
        "{case}: must be Allow (no output); got {out}"
    );
}

// ---- cd that does not move the rm: must NOT be Allow -------------------------

#[test]
fn e1_background_cd_does_not_move_rm() {
    let f = Fx::new("e1");
    let out = f.run(&format!("cd {}/s & rm -rf src", f.hw().display()));
    assert_not_allow("1: cd root/s & rm -rf src", &out);
}

#[test]
fn e2_failed_cd_leaves_cwd_unchanged() {
    let f = Fx::new("e2");
    let out = f.run(&format!("cd {}/nonexistent; rm -rf src", f.hw().display()));
    assert_not_allow("2: cd root/nonexistent; rm -rf src", &out);
}

#[test]
fn e3_rm_runs_only_when_cd_failed() {
    let f = Fx::new("e3");
    let out = f.run(&format!(
        "cd {}/nonexistent || rm -rf src",
        f.hw().display()
    ));
    assert_not_allow("3: cd root/nonexistent || rm -rf src", &out);
}

#[test]
fn e4_cd_then_semicolon_rm() {
    let f = Fx::new("e4");
    let out = f.run(&format!("cd {}/s; rm -rf src", f.hw().display()));
    assert_not_allow("4: cd root/s; rm -rf src", &out);
}

#[test]
fn e5_cd_piped_into_rm() {
    let f = Fx::new("e5");
    let out = f.run(&format!("cd {}/s | rm -rf src", f.hw().display()));
    assert_not_allow("5: cd root/s | rm -rf src", &out);
}

// ---- lstat: the root check must not follow a symlinked root ------------------

#[test]
fn e6_symlinked_root_target_spelled_canonically() {
    // <parent>/.harness-worktrees -> <parent>/victim; the command names the
    // link TARGET's own spelling, so only the root check (lstat vs stat)
    // decides whether `victim` is mistaken for a root.
    let f = Fx::new("e6");
    std::fs::remove_dir_all(f.hw()).unwrap();
    let victim = f.parent.join("victim");
    std::fs::create_dir_all(victim.join("precious")).unwrap();
    std::fs::write(victim.join("precious/f"), "x").unwrap();
    symlink(&victim, f.hw()).unwrap();
    let out = f.run(&format!("rm -rf {}/precious", victim.display()));
    assert_not_allow("6: rm -rf <victim>/precious with root -> victim", &out);
}

// ---- positive controls: must stay Allow --------------------------------------

#[test]
fn c7_absolute_operand_in_root_no_cd() {
    let f = Fx::new("c7");
    let out = f.run(&format!("rm -rf {}/s", f.hw().display()));
    assert_allow("7: rm -rf <root>/s from cwd=proj", &out);
}

#[test]
fn c8_absolute_operand_after_cd() {
    let f = Fx::new("c8");
    let out = f.run(&format!(
        "cd {} && rm -rf {}/s",
        f.hw().display(),
        f.hw().display()
    ));
    assert_allow("8: cd root && rm -rf <root>/s", &out);
}

#[test]
fn c9_relative_operand_with_cwd_inside_root() {
    let f = Fx::new("c9");
    let cwd = f.hw().join("s");
    let out = f.run_in(&cwd, "rm -rf src");
    assert_allow("9: cwd=<root>/s, rm -rf src", &out);
}
