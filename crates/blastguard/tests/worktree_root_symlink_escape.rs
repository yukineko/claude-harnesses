#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A worktree storage root must not be trusted by its resolved path alone: an
//! agent can redirect `.harness-worktrees` / `$HOME/.condukt[/worktrees]` with a
//! symlink (pre-existing, or created in the same command) so that
//! `rm -rf <root>/x` deletes something outside any root. Every such case must be
//! NOT Allow (Deny or Ask). Positive controls keep real roots Allow.
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
    victim: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        Fx::with_home_rel(name, "home")
    }

    /// `home_rel` is relative to the base; may traverse a symlinked ancestor
    /// (created by the caller before use).
    fn with_home_rel(name: &str, home_rel: &str) -> Fx {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("worktree_root_symlink_escape")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let f = Fx {
            home: base.join(home_rel),
            parent: base.join("src"),
            proj: base.join("src/proj"),
            victim: base.join("outside/victim"),
            base,
        };
        std::fs::create_dir_all(&f.proj).unwrap();
        std::fs::create_dir_all(f.victim.join("precious")).unwrap();
        std::fs::write(f.victim.join("precious/f"), "x").unwrap();
        f
    }

    fn hw(&self) -> PathBuf {
        self.parent.join(".harness-worktrees")
    }

    fn run(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "worktree-root-symlink-escape",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
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
    assert!(is_allow(out), "{case}: must be Allow (no output); got {out}");
}

// ---- escapes: must NOT be Allow ---------------------------------------------

#[test]
fn e1_harness_worktrees_is_symlink_to_victim() {
    let f = Fx::new("e1");
    symlink(&f.victim, f.hw()).unwrap();
    let out = f.run(&format!("rm -rf {}/precious", f.hw().display()));
    assert_not_allow("1: .harness-worktrees -> victim", &out);
}

#[test]
fn e2_condukt_worktrees_is_symlink_to_docs() {
    let f = Fx::new("e2");
    std::fs::create_dir_all(f.home.join("Docs/precious")).unwrap();
    std::fs::create_dir_all(f.home.join(".condukt")).unwrap();
    symlink(f.home.join("Docs"), f.home.join(".condukt/worktrees")).unwrap();
    let out = f.run(&format!(
        "rm -rf {}/.condukt/worktrees/precious",
        f.home.display()
    ));
    assert_not_allow("2: $HOME/.condukt/worktrees -> $HOME/Docs", &out);
}

#[test]
fn e3_condukt_is_symlink_to_dir_with_worktrees() {
    let f = Fx::new("e3");
    std::fs::create_dir_all(f.base.join("elsewhere/worktrees/precious")).unwrap();
    std::fs::create_dir_all(&f.home).unwrap();
    symlink(f.base.join("elsewhere"), f.home.join(".condukt")).unwrap();
    let out = f.run(&format!(
        "rm -rf {}/.condukt/worktrees/precious",
        f.home.display()
    ));
    assert_not_allow("3: $HOME/.condukt -> dir containing worktrees", &out);
}

#[test]
fn e4_root_absent_symlink_created_in_same_command() {
    let f = Fx::new("e4");
    let hw = f.hw();
    let out = f.run(&format!(
        "ln -s {} {} && rm -rf {}/precious",
        f.victim.display(),
        hw.display(),
        hw.display()
    ));
    assert_not_allow("4: ln -s victim root && rm -rf root/precious", &out);
}

#[test]
fn e5_real_root_swapped_for_symlink_in_same_command() {
    let f = Fx::new("e5");
    let hw = f.hw();
    std::fs::create_dir_all(hw.join("s1")).unwrap();
    let old = f.parent.join("old");
    let out = f.run(&format!(
        "mv {} {} && ln -s {} {} && rm -rf {}/precious",
        hw.display(),
        old.display(),
        f.victim.display(),
        hw.display(),
        hw.display()
    ));
    assert_not_allow("5: mv root old && ln -s victim root && rm -rf root/precious", &out);
}

#[test]
fn e6_nonexistent_root_is_not_trusted() {
    let f = Fx::new("e6");
    assert!(!f.hw().exists());
    let out = f.run(&format!("rm -rf {}/x", f.hw().display()));
    assert_not_allow("6: absent .harness-worktrees, rm -rf root/x", &out);
}

// ---- positive controls: must stay Allow ---------------------------------------

#[test]
fn c7_real_condukt_worktrees_dir_allows() {
    let f = Fx::new("c7");
    std::fs::create_dir_all(f.home.join(".condukt/worktrees/x")).unwrap();
    let out = f.run(&format!("rm -rf {}/.condukt/worktrees/x", f.home.display()));
    assert_allow("7: real $HOME/.condukt/worktrees/x", &out);
}

#[test]
fn c8_real_harness_worktrees_dir_allows() {
    let f = Fx::new("c8");
    std::fs::create_dir_all(f.hw().join("s1")).unwrap();
    let out = f.run(&format!("rm -rf {}/s1", f.hw().display()));
    assert_allow("8: real <parent>/.harness-worktrees/s1", &out);
}

#[test]
fn c9_home_reached_through_symlinked_ancestor_allows() {
    // HOME = <base>/linkparent/home where linkparent -> <base>/realparent.
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("worktree_root_symlink_escape")
        .join("c9");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("realparent/home/.condukt/worktrees/x")).unwrap();
    symlink(base.join("realparent"), base.join("linkparent")).unwrap();
    let f = Fx::with_home_rel("c9_keep", "unused");
    // Point HOME at the symlinked-ancestor path (fixture struct only supplies env).
    let f = Fx {
        home: base.join("linkparent/home"),
        ..f
    };
    assert!(f.home.join(".condukt/worktrees/x").is_dir());
    let out = f.run(&format!("rm -rf {}/.condukt/worktrees/x", f.home.display()));
    assert_allow("9: HOME via symlinked ancestor, real root", &out);
}
