// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros (scout 2026-11-12 F3): `max_parallel` is never enforced, and an
//! unparseable `CONDUKT_MAX_PARALLEL` / `CONDUKT_STUCK_TTL_SECS` is silently
//! ignored.
//!
//! - F3a: `Config::max_parallel` is documented as "Soft cap on concurrent
//!   workers (advisory; the skill honors it)" (crates/condukt/src/config.rs:28-29)
//!   and no binary path reads it, so with `CONDUKT_MAX_PARALLEL=1` a second
//!   `condukt worktree create` in the same run, while the first worktree is
//!   live, still succeeds.
//! - F3b: crates/condukt/src/config.rs:446-455 does `if let Ok(n) = v.parse()`
//!   and drops a parse failure on the floor, so a typo'd cap / TTL silently
//!   falls back to the default with nothing on stderr.
//!
//! RED observed at rev 656b1c38 (written without a fix, by a disinterested
//! author — CLAUDE.md §2(a)). Each RED test runs its control first on the same
//! fixture, so the RED assertion can only fail on the property under test.
//!
//! The real binary runs against a per-test `$HOME`, state dir and worktree base
//! inside a throwaway git repo with `GIT_CONFIG_GLOBAL=/dev/null`; nothing
//! under the real `~/.condukt`, `~/.budgetguard` or `~/.overwatch` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt_base: PathBuf,
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        let wt_base = base.join("wts");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "base.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            _tmp: tmp,
            repo,
            home,
            wt_base,
        }
    }

    /// Run condukt with the isolated env plus `extra` env vars. The two knobs
    /// under test are removed unless `extra` sets them.
    fn condukt(&self, extra: &[(&str, &str)], args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CLAUDE_CODE_SESSION_ID", "sess-scout-f3")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CONDUKT_DISABLE")
            .env_remove("CONDUKT_MAX_PARALLEL")
            .env_remove("CONDUKT_STUCK_TTL_SECS");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn condukt")
    }
}

fn show(out: &Output) -> String {
    format!(
        "exit={:?} stdout={:?} stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// F3a: with `CONDUKT_MAX_PARALLEL=1`, a second live worktree in the same run
/// must be refused.
#[test]
#[ignore = "scout 2026-11-12 F3: RED, open defect. Remove the ignore when fixed"]
fn max_parallel_one_refuses_a_second_live_worktree_in_the_same_run() {
    let fx = Fixture::new();
    let cap = [("CONDUKT_MAX_PARALLEL", "1")];

    // CONTROL: the first worktree of the run is within the cap and is created.
    let a = fx.condukt(
        &cap,
        &[
            "worktree", "create", "--run", "run-f3", "--topic", "a", "--branch", "b-a",
        ],
    );
    assert!(
        a.status.success(),
        "CONTROL: first worktree create must succeed: {}",
        show(&a)
    );
    let wt_a = PathBuf::from(String::from_utf8_lossy(&a.stdout).trim());
    assert!(
        wt_a.is_dir(),
        "CONTROL: worktree a must be live at {}",
        wt_a.display()
    );

    // RED: a second worktree of the same run while `a` is still live.
    let b = fx.condukt(
        &cap,
        &[
            "worktree", "create", "--run", "run-f3", "--topic", "b", "--branch", "b-b",
        ],
    );
    eprintln!("observation (second create under cap 1): {}", show(&b));
    assert!(
        wt_a.is_dir(),
        "worktree a must still be live when b is created"
    );
    assert!(
        !b.status.success(),
        "CONDUKT_MAX_PARALLEL=1 must refuse a second live worktree in run-f3 while \
         worktree a is live; got {}",
        show(&b)
    );
}

/// Shared body of the two F3b cases: control (unset) then the unparseable value.
fn unparseable_env_is_not_silent(var: &str) {
    let fx = Fixture::new();

    // CONTROL: `state list` with the variable unset succeeds and does not
    // mention it (so naming it below is a signal, not noise).
    let ctl = fx.condukt(&[], &["state", "list"]);
    assert!(
        ctl.status.success(),
        "CONTROL: state list must succeed: {}",
        show(&ctl)
    );
    assert!(
        !String::from_utf8_lossy(&ctl.stderr).contains(var),
        "CONTROL: stderr must not mention {var} when unset: {}",
        show(&ctl)
    );

    // RED: the variable is set to something that is not a number.
    let out = fx.condukt(&[(var, "notanumber")], &["state", "list"]);
    eprintln!("observation ({var}=notanumber): {}", show(&out));
    let named = String::from_utf8_lossy(&out.stderr).contains(var);
    assert!(
        !out.status.success() || named,
        "{var}=notanumber was silently ignored: `condukt state list` exited 0 and did not \
         name {var} on stderr; got {}",
        show(&out)
    );
}

/// F3b (cap): an unparseable `CONDUKT_MAX_PARALLEL` must not be silent.
#[test]
#[ignore = "scout 2026-11-12 F3: RED, open defect. Remove the ignore when fixed"]
fn unparseable_max_parallel_env_is_not_silently_ignored() {
    unparseable_env_is_not_silent("CONDUKT_MAX_PARALLEL");
}

/// F3b (TTL): an unparseable `CONDUKT_STUCK_TTL_SECS` must not be silent.
#[test]
#[ignore = "scout 2026-11-12 F3: RED, open defect. Remove the ignore when fixed"]
fn unparseable_stuck_ttl_env_is_not_silently_ignored() {
    unparseable_env_is_not_silent("CONDUKT_STUCK_TTL_SECS");
}
