// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec (user ruling 2026-11-21): for /flow the only stop reason is the
//! circuit breaker, and compaction must no longer be a stop reason. A new flag
//! `condukt circuit check --no-compaction-axis` turns the compaction axis off;
//! every other axis stays intact and fail-closed.
//!
//! - with the flag, a session holding >= the default max_compactions
//!   top-level `compact_boundary` lines CONTINUES (exit 0) and the stdout JSON
//!   reports `"compaction_axis":"disabled"`; without it the same fixture trips
//!   `session_compacted` (the control) and reports `"compaction_axis":"enabled"`;
//! - with the flag, a missing transcript still trips `idle_unmeasured`, a stale
//!   transcript still trips `stall`, and a failure streak in the item-outcome
//!   log still trips `failure_streak`.
//!
//! Driven through the built binary; HOME is sandboxed so no test reads or
//! writes the real ~/.claude, ~/.condukt or ~/.backlog.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

const SESSION: &str = "sess-no-compaction-axis";
const FLAG: &str = "--no-compaction-axis";

/// Shape copied from a real Claude Code transcript (top-level
/// `type`/`subtype`), as in circuit_session_compacted_e2e.rs.
const BOUNDARY: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"system","subtype":"compact_boundary","content":"Conversation compacted","level":"info","compactMetadata":{"trigger":"auto","preTokens":272284,"postTokens":12126},"sessionId":"sess-no-compaction-axis"}"#;
const USER: &str = r#"{"type":"user","message":{"role":"user","content":"hello"},"sessionId":"sess-no-compaction-axis"}"#;

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let base = std::env::temp_dir().join(format!("condukt-nocompax-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wtb"))
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .expect("spawn condukt")
    }

    /// `<home>/.claude/projects/-fixture/<SESSION>.jsonl`, written fresh
    /// (mtime = now, so the idle axis does not trip on its own).
    fn write_transcript(&self, lines: &[&str]) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{SESSION}.jsonl"));
        let mut body = String::new();
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Record one finished item WITHOUT `--outcome done` in the run's
    /// item-outcome log (pattern of release_outcome_streak.rs).
    fn release_not_done(&self, run: &str, hk: &str) {
        let o = self.condukt(&["state", "release-task", "--run", run, "--hashkey", hk]);
        assert!(o.status.success(), "apparatus: release-task failed: {o:?}");
    }

    /// `circuit check --run flow-S --session S --idle-ttl-secs 1800` + `extra`.
    fn circuit(&self, extra: &[&str]) -> (i32, serde_json::Value) {
        let run = format!("flow-{SESSION}");
        let mut args = vec![
            "circuit",
            "check",
            "--run",
            run.as_str(),
            "--idle-ttl-secs",
            "1800",
            "--session",
            SESSION,
        ];
        args.extend_from_slice(extra);
        let out = self.condukt(&args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "circuit check stdout is not JSON ({e}); exit={:?} stdout={stdout} stderr={}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().expect("exit code"), v)
    }
}

fn set_mtime_ago(path: &Path, secs: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

/// Two boundaries == the default max_compactions (2).
const COMPACTED_TWICE: &[&str] = &[USER, BOUNDARY, USER, BOUNDARY, USER];

// --- a) compaction is no longer a stop reason with the flag ----------------

#[test]
fn control_compacted_session_trips_without_flag() {
    let fx = Fixture::new("ctl-trip");
    fx.write_transcript(COMPACTED_TWICE);
    let (code, v) = fx.circuit(&[]);
    assert_eq!(
        code, 1,
        "control: compaction must trip without the flag: {v}"
    );
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], "session_compacted", "{v}");
}

#[test]
fn flag_lets_compacted_session_continue() {
    let fx = Fixture::new("flag-cont");
    fx.write_transcript(COMPACTED_TWICE);
    let (code, v) = fx.circuit(&[FLAG]);
    assert_eq!(
        code, 0,
        "--no-compaction-axis must not stop on compaction: {v}"
    );
    assert_eq!(v["verdict"], "continue", "{v}");
    assert_eq!(v["compaction_axis"], "disabled", "{v}");
}

#[test]
fn flag_lets_heavily_compacted_session_continue_even_with_max_one() {
    // Even the tightest cap cannot re-enable the axis once it is disabled.
    let fx = Fixture::new("flag-max1");
    fx.write_transcript(&[BOUNDARY, BOUNDARY, BOUNDARY, BOUNDARY, BOUNDARY]);
    let (code, v) = fx.circuit(&[FLAG, "--max-compactions", "1"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["verdict"], "continue", "{v}");
    assert_eq!(v["compaction_axis"], "disabled", "{v}");
}

// --- b) without the flag the axis reports itself enabled -------------------

#[test]
fn without_flag_trip_reports_compaction_axis_enabled() {
    let fx = Fixture::new("enabled-trip");
    fx.write_transcript(COMPACTED_TWICE);
    let (code, v) = fx.circuit(&[]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["compaction_axis"], "enabled", "{v}");
}

#[test]
fn without_flag_continue_reports_compaction_axis_enabled() {
    let fx = Fixture::new("enabled-cont");
    fx.write_transcript(&[USER, USER]);
    let (code, v) = fx.circuit(&[]);
    assert_eq!(
        code, 0,
        "control: fresh uncompacted transcript continues: {v}"
    );
    assert_eq!(v["verdict"], "continue", "{v}");
    assert_eq!(v["compaction_axis"], "enabled", "{v}");
}

// --- c) the other axes stay intact and fail-closed with the flag -----------

#[test]
fn flag_missing_transcript_still_trips_idle_unmeasured() {
    let fx = Fixture::new("flag-notx");
    let (code, v) = fx.circuit(&[FLAG]);
    assert_eq!(code, 1, "a missing transcript must still trip: {v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], "idle_unmeasured", "{v}");
    assert_eq!(v["compaction_axis"], "disabled", "{v}");
}

#[test]
fn flag_stale_transcript_still_trips_stall() {
    let fx = Fixture::new("flag-stale");
    let t = fx.write_transcript(&[USER]);
    set_mtime_ago(&t, 7200);
    let (code, v) = fx.circuit(&[FLAG]);
    assert_eq!(code, 1, "a stale transcript must still trip: {v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], "stall", "{v}");
}

#[test]
fn flag_failure_streak_still_trips_failure_streak() {
    let fx = Fixture::new("flag-streak");
    fx.write_transcript(&[USER]);
    let run = format!("flow-{SESSION}");
    for hk in ["hk-1", "hk-2", "hk-3"] {
        fx.release_not_done(&run, hk);
    }
    let (code, v) = fx.circuit(&[FLAG]);
    assert_eq!(code, 1, "a failure streak must still trip: {v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], "failure_streak", "{v}");
    assert_eq!(v["streak"], 3, "{v}");
}

#[test]
fn flag_failure_streak_trips_even_when_transcript_is_compacted() {
    // Disabling compaction must not mask a co-occurring failure streak.
    let fx = Fixture::new("flag-streak-compacted");
    fx.write_transcript(COMPACTED_TWICE);
    let run = format!("flow-{SESSION}");
    for hk in ["hk-1", "hk-2", "hk-3"] {
        fx.release_not_done(&run, hk);
    }
    let (code, v) = fx.circuit(&[FLAG]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reason"], "failure_streak", "{v}");
}

// --- apparatus controls: the fixtures above trip today WITHOUT the flag ----

#[test]
fn control_failure_streak_fixture_trips_without_flag() {
    let fx = Fixture::new("ctl-streak");
    fx.write_transcript(&[USER]);
    let run = format!("flow-{SESSION}");
    for hk in ["hk-1", "hk-2", "hk-3"] {
        fx.release_not_done(&run, hk);
    }
    let (code, v) = fx.circuit(&[]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reason"], "failure_streak", "{v}");
}

#[test]
fn control_stale_and_missing_fixtures_trip_without_flag() {
    let fx = Fixture::new("ctl-notx");
    let (code, v) = fx.circuit(&[]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reason"], "idle_unmeasured", "{v}");

    let fx = Fixture::new("ctl-stale");
    let t = fx.write_transcript(&[USER]);
    set_mtime_ago(&t, 7200);
    let (code, v) = fx.circuit(&[]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reason"], "stall", "{v}");
}

// --- follow-up: uncountable compactions on an idle-measurable transcript ----
//
// From circuit.rs: `count_compact_boundaries` silently skips unparseable lines
// (a garbage line does NOT make the count uncountable); the only route to an
// Undetermined compaction count with a located transcript is a failed
// `std::fs::read`. chmod 000 makes the read fail while the mtime (stat) stays
// measurable, so the idle axis is Known — same fixture as
// circuit_session_compacted_e2e.rs::unreadable_transcript_trips_even_with_idle_axis_off.

#[cfg(unix)]
fn make_unreadable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
}

#[cfg(unix)]
#[test]
fn control_uncountable_compactions_trip_without_flag() {
    let fx = Fixture::new("ctl-uncountable");
    let t = fx.write_transcript(&[USER]);
    make_unreadable(&t);
    let (code, v) = fx.circuit(&[]);
    assert_eq!(
        code, 1,
        "uncountable compactions must trip without the flag: {v}"
    );
    assert_eq!(v["verdict"], "trip", "{v}");
    assert!(v["compactions_unknown_reason"].is_string(), "{v}");
    assert!(
        v["idle_secs"].is_i64(),
        "apparatus: idle must be measured: {v}"
    );
}

#[cfg(unix)]
#[test]
fn flag_uncountable_compactions_continue() {
    let fx = Fixture::new("flag-uncountable");
    let t = fx.write_transcript(&[USER]);
    make_unreadable(&t);
    let (code, v) = fx.circuit(&[FLAG]);
    assert_eq!(
        code, 0,
        "a disabled axis must not trip on an uncountable count: {v}"
    );
    assert_eq!(v["verdict"], "continue", "{v}");
    assert_eq!(v["compaction_axis"], "disabled", "{v}");
    assert!(v["idle_secs"].is_i64(), "idle axis still measured: {v}");
}
