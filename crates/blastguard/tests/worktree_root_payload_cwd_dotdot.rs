#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A payload `cwd` containing `..` must not be collapsed lexically before use.
//! `<root>/w/lnk/..` with `lnk -> <outside>/victim` really is `<outside>`, so a
//! relative recursive delete of `sub` there removes `<outside>/sub`, outside the
//! worktree root. Judging it against the lexical `<root>/w` wrongly Allows it.
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
    outside: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("worktree_root_payload_cwd_dotdot")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let f = Fx {
            home: base.join("home"),
            parent: base.join("src"),
            outside: base.join("outside"),
            base,
        };
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::create_dir_all(f.parent.join("proj")).unwrap();
        let w = f.root().join("w");
        std::fs::create_dir_all(w.join("realdir")).unwrap();
        std::fs::create_dir_all(f.outside.join("sub")).unwrap();
        std::fs::create_dir_all(f.outside.join("victim/x")).unwrap();
        symlink(f.outside.join("victim"), w.join("lnk")).unwrap();
        // Prove the fixture: `lnk/..` is really <outside>.
        let via = std::fs::canonicalize(w.join("lnk/..")).unwrap();
        assert_eq!(via, std::fs::canonicalize(&f.outside).unwrap());
        f
    }

    fn root(&self) -> PathBuf {
        self.parent.join(".harness-worktrees")
    }

    /// Payload cwd and CLAUDE_PROJECT_DIR are the same literal string.
    fn run(&self, cwd: &str, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "worktree-root-payload-cwd-dotdot",
            "cwd": cwd,
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
        println!("[{command}] cwd={cwd} => {:?}", s.trim());
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

#[test]
fn p1_cwd_symlink_dotdot_relative_operand() {
    let f = Fx::new("p1");
    let cwd = format!("{}/w/lnk/..", f.root().display());
    let out = f.run(&cwd, "rm -rf sub");
    assert_not_allow("1: cwd=root/w/lnk/..: rm -rf sub", &out);
}

#[test]
fn p2_cwd_symlink_dotdot_dot_slash_operand() {
    let f = Fx::new("p2");
    let cwd = format!("{}/w/lnk/..", f.root().display());
    let out = f.run(&cwd, "rm -rf ./sub");
    assert_not_allow("2: cwd=root/w/lnk/..: rm -rf ./sub", &out);
}

#[test]
fn p3_cwd_symlink_dotdot_victim() {
    let f = Fx::new("p3");
    let cwd = format!("{}/w/lnk/../victim", f.root().display());
    let out = f.run(&cwd, "rm -rf x");
    assert_not_allow("3: cwd=root/w/lnk/../victim: rm -rf x", &out);
}

#[test]
fn p4_noncanonical_cwd_resolving_inside_is_not_trusted() {
    let f = Fx::new("p4");
    let cwd = format!("{}/w/./realdir/..", f.root().display());
    let out = f.run(&cwd, "rm -rf realdir");
    assert_not_allow("4: cwd=root/w/./realdir/..: rm -rf realdir", &out);
}

#[test]
fn c5_control_canonical_cwd_relative_is_allow() {
    let f = Fx::new("c5");
    let cwd = format!("{}/w", f.root().display());
    let out = f.run(&cwd, "rm -rf realdir");
    assert_allow("5: cwd=root/w: rm -rf realdir", &out);
}

#[test]
fn c6_control_absolute_operand_from_proj_is_allow() {
    let f = Fx::new("c6");
    let cwd = format!("{}/proj", f.parent.display());
    let out = f.run(&cwd, &format!("rm -rf {}/w/realdir", f.root().display()));
    assert_allow("6: cwd=proj: rm -rf root/w/realdir", &out);
}
