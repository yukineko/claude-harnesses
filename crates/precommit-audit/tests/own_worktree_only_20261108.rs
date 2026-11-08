#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! precommit-audit judges ONLY the worktree it runs in (user ruling 2026-11-08:
//! gates judge own worktree only — "donegateそのものが他のworktreeを見たり検証
//! したりテストしたりすることは禁止とする。他のgateでも禁止とする").
//!
//! The root is `--root`, else the git top-level of the PROCESS cwd.
//! `$CLAUDE_PROJECT_DIR` names the session's project dir, which for a session
//! working in a linked worktree is a DIFFERENT worktree; it used to take
//! precedence and so made the audit read/judge that other worktree. These tests
//! drive the real binary WITHOUT `--root`, with `CLAUDE_PROJECT_DIR` set to
//! another worktree, and pin:
//!  (a) a red other worktree does not change the verdict of a clean cwd;
//!  (b) an unreadable / broken other location is not read (no undetermined
//!      block);
//!  (c) a red cwd root still blocks (anti-vacuity), also with `CLAUDE_PROJECT_DIR`
//!      pointing at a clean other worktree.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

fn unique_dir() -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("pca-own-wt-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr)
    );
}

struct Fx {
    home: PathBuf,
    /// The primary checkout (clean).
    main: PathBuf,
    /// A linked worktree of `main` (the "other" worktree).
    other: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let home = unique_dir();
        let main = home.join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["config", "user.email", "t@example.com"]);
        git(&main, &["config", "user.name", "Test"]);
        std::fs::write(main.join("README.md"), "# repo\n").unwrap();
        std::fs::write(
            main.join(".precommit-audit.toml"),
            "[checks]\nlinters = false\n",
        )
        .unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-qm", "init"]);
        let other = home.join("other-wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "other",
                other.to_str().unwrap(),
            ],
        );
        Fx { home, main, other }
    }

    /// Stage a source file with no test in `repo`: a blocking finding.
    fn make_red(&self, repo: &Path) {
        std::fs::write(repo.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap();
        git(repo, &["add", "app.py"]);
    }

    /// Run the real binary in `cwd` (precommit mode), NO `--root`, with
    /// `CLAUDE_PROJECT_DIR` = `project_dir` when given. Returns (exit, output).
    fn audit(&self, cwd: &Path, project_dir: Option<&Path>) -> (i32, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_precommit-audit"));
        let c = harness_core::git_env::scrub(&mut cmd);
        c.args(["--mode", "precommit"])
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("HARNESS_TRUST_ALL", "1")
            .env("CLAUDE_CODE_SESSION_ID", "s-own-wt");
        match project_dir {
            Some(p) => {
                c.env("CLAUDE_PROJECT_DIR", p);
            }
            None => {
                c.env_remove("CLAUDE_PROJECT_DIR");
            }
        }
        let o = c.output().expect("binary runs");
        (
            o.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        )
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.other, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Restores 0o755 on drop so teardown can delete the dir even if an assert panics.
struct Unreadable(PathBuf);

impl Unreadable {
    fn new(dir: &Path) -> Unreadable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        Unreadable(dir.to_path_buf())
    }
}

impl Drop for Unreadable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// CONTROL (anti-vacuity): with no `CLAUDE_PROJECT_DIR`, a red cwd blocks
/// (exit 1) and a clean cwd passes (exit 0), so the cases below can tell the
/// two verdicts apart.
#[test]
fn control_red_cwd_blocks_and_clean_cwd_passes() {
    let f = Fx::new();
    let (c, o) = f.audit(&f.main, None);
    assert_eq!(c, 0, "clean cwd must pass: {o}");
    f.make_red(&f.main);
    let (c, o) = f.audit(&f.main, None);
    assert_eq!(c, 1, "red cwd must block: {o}");
}

/// (a) A RED other worktree (the session's `CLAUDE_PROJECT_DIR`) does not
/// affect the verdict of a clean cwd. Used to audit the other worktree (exit 1).
#[test]
fn red_other_worktree_via_project_dir_does_not_block_clean_cwd() {
    let f = Fx::new();
    f.make_red(&f.other);
    // apparatus: the other worktree really is red when audited on its own.
    let (c, o) = f.audit(&f.other, None);
    assert_eq!(c, 1, "apparatus: the other worktree must be red: {o}");
    let (c, o) = f.audit(&f.main, Some(&f.other));
    assert_eq!(
        c, 0,
        "the verdict must be that of the cwd checkout (clean), not of \
         CLAUDE_PROJECT_DIR's worktree: {o}"
    );
}

/// (c) A RED cwd still blocks when `CLAUDE_PROJECT_DIR` names a CLEAN other
/// worktree (the old code audited the clean one and let it through).
#[test]
fn red_cwd_blocks_even_when_project_dir_is_a_clean_other_worktree() {
    let f = Fx::new();
    f.make_red(&f.main);
    let (c, o) = f.audit(&f.main, Some(&f.other));
    assert_eq!(c, 1, "a red cwd must block: {o}");
}

/// (c) Both red: the verdict is the cwd's and the findings come from the cwd
/// checkout only (the other worktree's path is not what was audited).
#[test]
fn both_red_blocks_on_the_cwd_checkout() {
    let f = Fx::new();
    f.make_red(&f.main);
    f.make_red(&f.other);
    let (c, o) = f.audit(&f.main, Some(&f.other));
    assert_eq!(c, 1, "{o}");
    assert!(
        !o.contains(&f.other.display().to_string()),
        "the other worktree must not be audited or named: {o}"
    );
}

/// (b) NOT READ: `CLAUDE_PROJECT_DIR` names an unreadable directory (mode 000,
/// restored on Drop). It is never opened, so a clean cwd passes (the old code
/// took it as the root and could not audit it).
#[test]
fn unreadable_project_dir_is_not_read() {
    let f = Fx::new();
    let _unlock = Unreadable::new(&f.other);
    let (c, o) = f.audit(&f.main, Some(&f.other));
    assert_eq!(
        c, 0,
        "an unreadable CLAUDE_PROJECT_DIR must not influence a clean cwd: {o}"
    );
}

/// (b) NOT READ: `CLAUDE_PROJECT_DIR` names a corrupt checkout (a `.git`
/// gitfile pointing nowhere). Its git probe would fail; it is not run.
#[test]
fn corrupt_project_dir_is_not_read() {
    let f = Fx::new();
    let broken = f.home.join("broken-checkout");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(".git"), "gitdir: /nonexistent/harness-test\n").unwrap();
    let (c, o) = f.audit(&f.main, Some(&broken));
    assert_eq!(
        c, 0,
        "a corrupt CLAUDE_PROJECT_DIR must not influence a clean cwd: {o}"
    );
}
