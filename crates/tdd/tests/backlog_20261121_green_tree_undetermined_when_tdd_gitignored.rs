#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `tdd green` records `tree_identity.tree: null` whenever the
//! repo's `.gitignore` ignores `.tdd/` and `.tdd/` already exists (it always
//! does at GREEN time: the RED proof lives there).
//!
//! Observed 2026-11-21 (git 2.50.1 Apple Git-155, tdd 0.1.101) in condukt
//! run run-20261121-190152-69747 task t1: this repo's `.gitignore:27` is
//! `.tdd/`; `tdd green` wrote `tree_identity.undetermined.tree` =
//! "git exited Some(1): The following paths are ignored by one of your
//! .gitignore files:\n.tdd\nhint: Use -f if you really want to add them."
//! and `condukt worktree merge` then refused with
//! "merge-proof-contract(undetermined-proof): t1.green.json:
//! tree_identity.tree is missing or null". Cause: `content_tree`
//! (crates/tdd/src/evidence.rs) runs `git add -A -- . ':(exclude).tdd'`;
//! git exits 1 when an excluded pathspec names an existing ignored path,
//! which a minimal repo reproduces (init, `.tdd/` in .gitignore, mkdir .tdd).
//!
//! Shape: temp repo with `.tdd/` committed in .gitignore; RED (command fails
//! until impl.txt exists), create impl.txt, GREEN. Assert both proofs carry a
//! determined hex `tree_identity.tree`. Control: the same flow without the
//! ignore rule yields a determined tree (proves the fixture reaches the
//! tree computation at all).
//!
//! DOES NOT PROVE: that the tree id equals the merge tip's tree (condukt's
//! merge-proof-contract(h) check); behaviour with a custom proof dir; other
//! git versions.
//!
//! Written by the orchestrator (finder), not the implementer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tdd(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

fn git(cwd: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn s(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout: {}\nstderr: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn red_then_green(tag: &str, ignore_tdd: bool) -> (Dir, serde_json::Value, serde_json::Value) {
    let d = Dir(std::env::temp_dir().join(format!("tdd-gi-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    if ignore_tdd {
        std::fs::write(work.join(".gitignore"), ".tdd/\n").unwrap();
    } else {
        std::fs::write(work.join(".gitignore"), "target/\n").unwrap();
    }
    git(&work, &["add", "a.txt", ".gitignore"]);
    git(&work, &["commit", "-q", "--no-verify", "-m", "init"]);

    let cmd = "test -f impl.txt";
    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", cmd]);
    assert_eq!(red.status.code(), Some(0), "control: RED recorded: {}", s(&red));
    std::fs::write(work.join("impl.txt"), "impl\n").unwrap();
    let green = tdd(&home, &work, &["green", "--task", "t1", "--cmd", cmd]);
    assert_eq!(green.status.code(), Some(0), "control: GREEN recorded: {}", s(&green));
    let read = |kind: &str| -> serde_json::Value {
        let p = work.join(".tdd").join(format!("t1.{kind}.json"));
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    };
    let (r, g) = (read("red"), read("green"));
    (d, r, g)
}

fn tree_is_hex(p: &serde_json::Value) -> bool {
    p["tree_identity"]["tree"]
        .as_str()
        .is_some_and(|t| t.len() >= 40 && t.chars().all(|c| c.is_ascii_hexdigit()))
}

#[test]
fn control_tree_determined_when_tdd_dir_not_ignored() {
    let (_d, r, g) = red_then_green("ctl", false);
    assert!(tree_is_hex(&r), "control: RED tree determined: {r}");
    assert!(tree_is_hex(&g), "control: GREEN tree determined: {g}");
}

#[test]
#[ignore = "RED(83c9d1ee): tdd green tree_identity.tree is null when .gitignore ignores .tdd/"]
fn green_tree_determined_when_tdd_dir_is_gitignored() {
    let (_d, r, g) = red_then_green("ign", true);
    assert!(
        tree_is_hex(&r),
        "RED proof tree_identity.tree must be a determined hex tree id: {}",
        r["tree_identity"]
    );
    assert!(
        tree_is_hex(&g),
        "GREEN proof tree_identity.tree must be a determined hex tree id when .tdd/ is \
         gitignored (the RED proof already exists there): {}",
        g["tree_identity"]
    );
    assert_eq!(
        g["tree_identity"]["determined"],
        serde_json::Value::Bool(true),
        "GREEN tree_identity.determined: {}",
        g["tree_identity"]
    );
}
