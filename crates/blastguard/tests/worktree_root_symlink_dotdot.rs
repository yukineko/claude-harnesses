#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `..` after a symlink component walks up from the symlink TARGET, not from the
//! symlink's parent. `<root>/w/lnk/../v2` with `lnk -> <outside>/victim` really
//! names `<outside>/v2`'s sibling-of-victim level, i.e. OUTSIDE the worktree
//! root. Collapsing `..` lexically before resolving symlinks turns it into
//! `<root>/w/v2` and wrongly Allows a recursive `rm`. These must NOT be Allow.
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
    outside: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("worktree_root_symlink_dotdot")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let f = Fx {
            home: base.join("home"),
            parent: base.join("src"),
            proj: base.join("src/proj"),
            outside: base.join("outside"),
            base,
        };
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::create_dir_all(&f.proj).unwrap();
        let w = f.root().join("w");
        std::fs::create_dir_all(w.join("sub")).unwrap();
        std::fs::create_dir_all(w.join("realdir")).unwrap();
        let victim = f.outside.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::create_dir_all(victim.join("v2")).unwrap();
        std::fs::create_dir_all(victim.join("x")).unwrap();
        // Real escape targets: `lnk/..` is `<outside>`, so `lnk/../v2` = `<outside>/v2`.
        std::fs::create_dir_all(f.outside.join("v2")).unwrap();
        std::fs::create_dir_all(f.outside.join("x")).unwrap();
        symlink(&victim, w.join("lnk")).unwrap();
        symlink(&victim, w.join("sub/lnk2")).unwrap();
        symlink(victim.join("x"), w.join("x2")).unwrap();
        f
    }

    fn root(&self) -> PathBuf {
        self.parent.join(".harness-worktrees")
    }

    /// Prove the fixture: `p` really resolves outside the (canonical) root.
    #[track_caller]
    fn assert_escapes(&self, p: &Path) {
        let root = std::fs::canonicalize(self.root()).unwrap();
        let real = std::fs::canonicalize(p)
            .unwrap_or_else(|e| panic!("fixture path {} must exist: {e}", p.display()));
        assert!(
            !real.starts_with(&root),
            "fixture: {} resolves to {} which is inside {}",
            p.display(),
            real.display(),
            root.display()
        );
    }

    #[track_caller]
    fn assert_inside(&self, p: &Path) {
        let root = std::fs::canonicalize(self.root()).unwrap();
        let real = std::fs::canonicalize(p).unwrap();
        assert!(
            real.starts_with(&root),
            "fixture: {} not inside root",
            p.display()
        );
    }

    fn run_in(&self, cwd: &Path, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "worktree-root-symlink-dotdot",
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

#[test]
fn d1_symlink_then_dotdot_escapes_root() {
    let f = Fx::new("d1");
    let root = f.root();
    f.assert_escapes(&root.join("w/lnk/../v2"));
    let out = f.run_in(&f.proj, &format!("rm -rf {}/w/lnk/../v2", root.display()));
    assert_not_allow("1: rm -rf root/w/lnk/../v2", &out);
}

#[test]
fn d2_nested_symlink_then_dotdot_escapes_root() {
    let f = Fx::new("d2");
    let root = f.root();
    f.assert_escapes(&root.join("w/sub/lnk2/../x"));
    let out = f.run_in(
        &f.proj,
        &format!("rm -rf {}/w/sub/lnk2/../x", root.display()),
    );
    assert_not_allow("2: rm -rf root/w/sub/lnk2/../x", &out);
}

#[test]
fn d3_relative_symlink_then_dotdot_escapes_root() {
    let f = Fx::new("d3");
    let w = f.root().join("w");
    f.assert_escapes(&w.join("lnk/../x"));
    let out = f.run_in(&w, "rm -rf lnk/../x");
    assert_not_allow("3: cwd=root/w: rm -rf lnk/../x", &out);
}

#[test]
fn d4_trailing_dotdot_after_file_symlink_escapes_root() {
    let f = Fx::new("d4");
    let root = f.root();
    f.assert_escapes(&root.join("w/x2/.."));
    let out = f.run_in(&f.proj, &format!("rm -rf {}/w/x2/..", root.display()));
    assert_not_allow("4: rm -rf root/w/x2/..", &out);
}

#[test]
fn c5_control_real_dir_absolute_is_allow() {
    let f = Fx::new("c5");
    let root = f.root();
    f.assert_inside(&root.join("w/realdir"));
    let out = f.run_in(&f.proj, &format!("rm -rf {}/w/realdir", root.display()));
    assert_allow("5: rm -rf root/w/realdir", &out);
}

#[test]
fn c6_control_real_dir_relative_is_allow() {
    let f = Fx::new("c6");
    let w = f.root().join("w");
    f.assert_inside(&w.join("realdir"));
    let out = f.run_in(&w, "rm -rf realdir");
    assert_allow("6: cwd=root/w: rm -rf realdir", &out);
}
