#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Behavioural tests (backlog cccc9c43 / 40dde238), written independently of
//! the implementation: `state release-task` records each item's outcome in the
//! run's item-outcome log, `circuit check` counts the trailing non-`done`
//! outcomes of a run WITHOUT run state as its failure streak (an uncountable
//! log trips `streak_unmeasured`), and release-task only frees the releasing
//! run's own claims.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

fn find_logs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_logs(&p, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".release-log.jsonl"))
        {
            out.push(p);
        }
    }
}

impl Fx {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("condukt-relout-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn c(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_condukt"))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wtb"))
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .unwrap()
    }

    fn release(&self, run: &str, hk: &str, outcome: Option<&str>) -> Output {
        let mut args = vec!["state", "release-task", "--run", run, "--hashkey", hk];
        if let Some(o) = outcome {
            args.push("--outcome");
            args.push(o);
        }
        let o = self.c(&args);
        assert!(o.status.success(), "apparatus: release-task failed: {o:?}");
        o
    }

    fn circuit(&self, run: &str, extra: &[&str]) -> (Option<i32>, serde_json::Value, Output) {
        let mut args = vec!["circuit", "check", "--run", run, "--idle-ttl-secs", "0"];
        args.extend_from_slice(extra);
        let o = self.c(&args);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|e| panic!("circuit stdout is not JSON ({e}): {o:?}"));
        (o.status.code(), v, o)
    }

    /// The single item-outcome log written for this fixture.
    fn release_log(&self) -> PathBuf {
        let mut logs = Vec::new();
        find_logs(&self.home, &mut logs);
        find_logs(&self.repo, &mut logs);
        assert_eq!(
            logs.len(),
            1,
            "apparatus: expected one release log: {logs:?}"
        );
        logs.pop().unwrap()
    }
}

#[test]
fn three_releases_without_outcome_trip_failure_streak() {
    let f = Fx::new("three-unspec");
    let run = "flow-s1";
    for hk in ["hk-1", "hk-2", "hk-3"] {
        f.release(run, hk, None);
    }
    let (code, v, o) = f.circuit(run, &[]);
    assert_eq!(code, Some(1), "three unspecified releases must trip: {o:?}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], "failure_streak", "{v}");
    assert_eq!(v["streak"], 3, "{v}");
}

#[test]
fn a_done_release_resets_the_streak() {
    let f = Fx::new("done-resets");
    let run = "flow-s2";
    f.release(run, "hk-1", Some("failed"));
    f.release(run, "hk-2", Some("failed"));
    f.release(run, "hk-3", Some("done"));
    let (code, v, o) = f.circuit(run, &[]);
    assert_eq!(code, Some(0), "a trailing done must not trip: {o:?}");
    assert_eq!(v["verdict"], "continue", "{v}");
    assert_eq!(v["streak"], 0, "{v}");
}

#[test]
fn a_corrupt_release_log_trips_streak_unmeasured() {
    let f = Fx::new("corrupt");
    let run = "flow-s3";
    f.release(run, "hk-1", Some("done"));
    let log = f.release_log();
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str("{not json\n");
    std::fs::write(&log, text).unwrap();

    let (code, v, o) = f.circuit(run, &[]);
    assert_eq!(code, Some(1), "an uncountable streak must trip: {o:?}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], "streak_unmeasured", "{v}");
    assert!(v["streak"].is_null(), "streak must be null, not 0: {v}");
    assert!(
        v["streak_unknown_reason"].is_string(),
        "streak_unknown_reason must be set: {v}"
    );
}

#[test]
fn a_corrupt_release_log_with_streak_axis_off_continues() {
    let f = Fx::new("corrupt-cap0");
    let run = "flow-s4";
    f.release(run, "hk-1", Some("done"));
    let log = f.release_log();
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str("{not json\n");
    std::fs::write(&log, text).unwrap();

    let (code, v, o) = f.circuit(run, &["--streak-cap", "0"]);
    assert_eq!(
        code,
        Some(0),
        "streak axis opted out: corrupt log must not trip: {o:?}"
    );
    assert_eq!(v["verdict"], "continue", "{v}");
}

#[test]
fn release_of_a_foreign_claim_fails_but_releases_own_claim() {
    let f = Fx::new("foreign");
    for (run, hk) in [("run-A", "hk-a"), ("run-B", "hk-b")] {
        let o = f.c(&["state", "claim-task", "--run", run, "--hashkey", hk]);
        assert!(o.status.success(), "apparatus: claim {hk} failed: {o:?}");
    }

    let o = f.c(&[
        "state",
        "release-task",
        "--run",
        "run-B",
        "--hashkey",
        "hk-a",
        "--hashkey",
        "hk-b",
    ]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "releasing another run's claim must exit 1: {o:?}"
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("hk-a"),
        "the refused hashkey must be named on stderr: {stderr}"
    );

    let a = f.c(&["state", "is-claimed", "--hashkey", "hk-a"]);
    assert_eq!(
        a.status.code(),
        Some(0),
        "run-A's claim was released: {a:?}"
    );
    let b = f.c(&["state", "is-claimed", "--hashkey", "hk-b"]);
    assert_eq!(
        b.status.code(),
        Some(1),
        "run-B's own claim was not released: {b:?}"
    );
}
