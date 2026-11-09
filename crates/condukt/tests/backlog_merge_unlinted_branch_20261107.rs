// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07, session c2e3bc17): `condukt worktree merge` lands a
//! task branch whose tip fails `cargo fmt --check` / `cargo clippy -D warnings`.
//!
//! Observed: condukt workers committed code that failed clippy
//! (`collapsible_if`, crates/blastguard/src/detect/interpreter.rs:1168) and
//! rustfmt. Nothing on the worker -> commit -> `condukt worktree merge` path ran
//! fmt or clippy, so the first thing to notice was donegate on the PARENT
//! session's Stop, after the fact (CLAUDE.md: "no gate enforces fmt/clippy";
//! .githooks/pre-commit does not run them).
//!
//! `condukt worktree merge` is the one deterministic choke point every task
//! branch passes before it reaches main, so the property is pinned there: a
//! branch whose tip is not fmt/clippy clean must NOT land on main (refused,
//! main unmoved). The control proves the fixture can merge at all, so a refusal
//! in the RED tests cannot be an unrelated fixture failure.
//!
//! Each RED test first runs the lint itself in the fixture worktree and asserts
//! it fails (apparatus), so "merged anyway" really is "merged a red tip".
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// HOME is replaced by a tempdir below; rustup/cargo must still find the real
/// toolchain, so pin CARGO_HOME / RUSTUP_HOME to the ambient ones.
fn toolchain_env(cmd: &mut Command) -> &mut Command {
    let real_home = std::env::var("HOME").unwrap_or_default();
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{real_home}/.cargo"));
    let rustup_home =
        std::env::var("RUSTUP_HOME").unwrap_or_else(|_| format!("{real_home}/.rustup"));
    cmd.env("CARGO_HOME", cargo_home)
        .env("RUSTUP_HOME", rustup_home)
}

fn git(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn cargo(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("cargo");
    toolchain_env(&mut cmd)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .output()
        .expect("spawn cargo")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

const MANIFEST: &str =
    "[package]\nname = \"mergefx\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n";
const CLEAN_LIB: &str = "pub fn answer() -> u32 {\n    42\n}\n";

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("Cargo.toml"), MANIFEST).unwrap();
        std::fs::write(repo.join(".gitignore"), "/target\nCargo.lock\n").unwrap();
        std::fs::write(repo.join("src/lib.rs"), CLEAN_LIB).unwrap();
        run_git(&repo, &["add", "-A"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);
        Fixture {
            _tmp: tmp,
            repo,
            home,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        toolchain_env(&mut cmd)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-unlinted-merge")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    /// `condukt worktree create`, write `lib` into the task worktree, commit it
    /// with the recorded `Condukt-Task` trailer. Returns (worktree, branch).
    fn task_branch(&self, topic: &str, lib: &str) -> (PathBuf, String) {
        let out = self.condukt(&[
            "worktree",
            "create",
            "--run",
            "run-unlinted",
            "--topic",
            topic,
            "--branch",
            &format!("b-{topic}"),
        ]);
        assert!(out.status.success(), "worktree create: {out:?}");
        let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
        let branch = run_git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let identity = run_git(
            &self.repo,
            &[
                "config",
                "--local",
                "--get",
                &format!("branch.{branch}.conduktTask"),
            ],
        );
        std::fs::write(wt.join("src/lib.rs"), lib).unwrap();
        run_git(&wt, &["add", "src/lib.rs"]);
        run_git(
            &wt,
            &[
                "commit",
                "-q",
                "-m",
                &format!("worker change\n\nCondukt-Task: {identity}"),
            ],
        );
        (wt, branch)
    }

    fn merge(&self, branch: &str) -> Output {
        self.condukt(&["worktree", "merge", "--branch", branch])
    }

    fn main_sha(&self) -> String {
        run_git(&self.repo, &["rev-parse", "main"])
    }
}

/// rustfmt reports a diff for this (the gate would run `cargo fmt --check`).
const FMT_VIOLATION: &str =
    "pub fn answer() -> u32 {\n    42\n}\n\npub fn badly_formatted(   ) ->u32{let x=1;x}\n";

/// rustfmt-clean, but `clippy::collapsible_if` fires under `-D warnings` (the
/// lint observed at crates/blastguard/src/detect/interpreter.rs:1168).
const CLIPPY_VIOLATION: &str = "pub fn answer() -> u32 {\n    42\n}\n\npub fn nested(a: bool, b: bool) -> u32 {\n    if a {\n        if b {\n            return 1;\n        }\n    }\n    0\n}\n";

/// Clean under both fmt and clippy.
const CLEAN_ADDITION: &str =
    "pub fn answer() -> u32 {\n    42\n}\n\npub fn also_clean(x: u32) -> u32 {\n    x + 1\n}\n";

fn assert_refused_and_main_unmoved(f: &Fixture, branch: &str, what: &str) {
    let before = f.main_sha();
    let out = f.merge(branch);
    let after = f.main_sha();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && after == before,
        "condukt worktree merge landed a branch whose tip fails {what}: exit={:?}, main \
         {before} -> {after}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
}

/// ANTI-VACUITY CONTROL: a fmt+clippy clean task branch merges and main
/// advances. Proves the fixture's create/trailer/merge path works, so the RED
/// tests below fail on the lint property, not on fixture plumbing.
#[test]
fn control_clean_branch_merges() {
    let f = Fixture::new();
    let (wt, branch) = f.task_branch("clean", CLEAN_ADDITION);
    let fmt = cargo(&wt, &f.home, &["fmt", "--", "--check"]);
    assert!(
        fmt.status.success(),
        "apparatus: clean lib must be fmt-clean: {fmt:?}"
    );
    let before = f.main_sha();
    let out = f.merge(&branch);
    assert!(out.status.success(), "clean branch must merge: {out:?}");
    assert_ne!(f.main_sha(), before, "main must advance on a clean merge");
}

#[test]
fn merge_refuses_branch_whose_tip_fails_cargo_fmt_check() {
    let f = Fixture::new();
    let (wt, branch) = f.task_branch("fmt", FMT_VIOLATION);
    let fmt = cargo(&wt, &f.home, &["fmt", "--", "--check"]);
    assert!(
        !fmt.status.success(),
        "apparatus: the task tip must fail `cargo fmt --check`: {fmt:?}"
    );
    assert_refused_and_main_unmoved(&f, &branch, "`cargo fmt --check`");
}

#[test]
fn merge_refuses_branch_whose_tip_fails_cargo_clippy() {
    let f = Fixture::new();
    let (wt, branch) = f.task_branch("clippy", CLIPPY_VIOLATION);
    let fmt = cargo(&wt, &f.home, &["fmt", "--", "--check"]);
    assert!(
        fmt.status.success(),
        "apparatus: the clippy fixture must be fmt-clean so only clippy is red: {fmt:?}"
    );
    let clippy = cargo(
        &wt,
        &f.home,
        &["clippy", "--all-targets", "--", "-D", "warnings"],
    );
    assert!(
        !clippy.status.success(),
        "apparatus: the task tip must fail `cargo clippy -D warnings`: {clippy:?}"
    );
    assert_refused_and_main_unmoved(&f, &branch, "`cargo clippy -D warnings`");
}
