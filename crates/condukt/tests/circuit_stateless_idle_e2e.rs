// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end coverage for `condukt circuit check` against a run that has NO
//! run-state JSON but does own `stateless` claims (backlog d7f2a4ea).
//!
//! A `/flow` driver claims tasks under `flow-<SID>` with `state claim-task
//! --stateless` and never calls `state init`. Before the fix, the idle axis
//! read "run state could not be loaded" for such a run and tripped
//! `idle_unmeasured` on every cycle — byte-identical to a run id that does not
//! exist at all — so the breaker detected nothing. These tests pin:
//!
//! * (a) stateless claim + transcript just modified  → continue (exit 0)
//! * (b) stateless claim + transcript older than TTL → trip `stall`
//! * (c) stateless claim + transcript unreadable     → trip `idle_unmeasured`
//!   with a reason that names the transcript/session, not the run state
//! * (d) no run state and no claim                   → trip `idle_unmeasured`,
//!   reason unchanged ("run state could not be loaded")
//! * (e) a run WITH run state keeps the run-state path even when a stateless
//!   claim with a frozen transcript exists under the same id.
//!
//! Everything runs the real binary against an isolated `$HOME` (which is also
//! where condukt's state dir and `~/.claude/projects` resolve), inside a
//! throwaway git repo, so the live claim registry is never touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

const SESSION: &str = "sess-circuit-e2e";

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

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-circuit-stateless-{pid}-{tag}"));
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

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            // Never let the developer's live session id leak into a claim.
            .env_remove("CLAUDE_CODE_SESSION_ID");
        c
    }

    fn condukt(&self, args: &[&str]) -> Output {
        self.cmd(args).output().expect("spawn condukt")
    }

    /// `<home>/.claude/projects/-fixture/<SESSION>.jsonl`, written fresh.
    fn write_transcript(&self) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{SESSION}.jsonl"));
        std::fs::write(&p, "{\"seed\":1}\n").unwrap();
        p
    }

    /// Claim one task hashkey for `run` with the `--stateless` marker, as the
    /// /flow driver does. `session` None ⇒ no owning session id at all.
    fn claim_stateless(&self, run: &str, session: Option<&str>) {
        let mut args = vec![
            "state",
            "claim-task",
            "--run",
            run,
            "--stateless",
            "--title",
            "t",
            "--hashkey",
            "hk-1",
        ];
        if let Some(s) = session {
            args.push("--session");
            args.push(s);
        }
        let out = self.condukt(&args);
        assert!(
            out.status.success(),
            "fixture precondition: stateless claim-task must succeed: {out:?}"
        );
        // Observed, not assumed: the registry now holds a stateless claim.
        let claims = self.condukt(&["state", "claims"]);
        let txt = String::from_utf8_lossy(&claims.stdout);
        assert!(
            txt.contains(run) && txt.contains("\"stateless\": true"),
            "fixture precondition: registry must show a stateless claim for {run}; got {txt}"
        );
    }

    /// Run `circuit check` and return `(exit code, parsed stdout JSON)`.
    fn circuit(&self, run: &str) -> (i32, serde_json::Value) {
        let out = self.condukt(&["circuit", "check", "--run", run, "--idle-ttl-secs", "1800"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "circuit check stdout is not JSON ({e}); stdout={stdout} stderr={}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().expect("exit code"), v)
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn set_mtime_ago(path: &Path, secs: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

/// (a) A /flow driver whose transcript was just written is making progress:
/// the stall axis must MEASURE that (idle ≈ 0) and continue.
#[test]
fn stateless_claim_with_fresh_transcript_continues() {
    let fx = Fixture::new("fresh");
    let run = "flow-fresh";
    fx.write_transcript();
    fx.claim_stateless(run, Some(SESSION));

    let (code, v) = fx.circuit(run);
    assert_eq!(
        code, 0,
        "a stateless run whose transcript just changed must continue; got {v}"
    );
    assert_eq!(v["verdict"], "continue", "{v}");
    let idle = v["idle_secs"]
        .as_i64()
        .unwrap_or_else(|| panic!("idle_secs must be a measurement, got {v}"));
    assert!(
        (0..600).contains(&idle),
        "idle must be measured from the fresh transcript; got {v}"
    );
    assert!(v["idle_unknown_reason"].is_null(), "{v}");
}

/// (b) Transcript frozen for longer than the TTL: that is a measured stall,
/// not an unmeasurable idle.
#[test]
fn stateless_claim_with_stale_transcript_trips_on_stall() {
    let fx = Fixture::new("stale");
    let run = "flow-stale";
    let t = fx.write_transcript();
    set_mtime_ago(&t, 7200);
    fx.claim_stateless(run, Some(SESSION));

    let (code, v) = fx.circuit(run);
    assert_eq!(code, 1, "a 2h-frozen transcript must trip; got {v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(
        v["reason"], "stall",
        "a frozen transcript is a MEASURED stall, not idle_unmeasured; got {v}"
    );
    let idle = v["idle_secs"].as_i64().expect("idle measured");
    assert!(
        idle >= 7000,
        "idle must reflect the transcript age; got {v}"
    );
}

/// (c-1) The claim names a session, but no transcript exists for it: trip
/// `idle_unmeasured`, and the reason must name the transcript, not the run
/// state.
#[test]
fn stateless_claim_with_missing_transcript_trips_unmeasured_with_honest_reason() {
    let fx = Fixture::new("notx");
    let run = "flow-notx";
    // Deliberately NO transcript written.
    fx.claim_stateless(run, Some(SESSION));

    let (code, v) = fx.circuit(run);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reason"], "idle_unmeasured", "{v}");
    assert!(v["idle_secs"].is_null(), "{v}");
    let why = v["idle_unknown_reason"].as_str().expect("reason string");
    assert!(
        !why.contains("run state could not be loaded"),
        "the cause is the transcript, not a missing run state; got {why:?}"
    );
    assert!(
        why.contains("transcript"),
        "the reason must name the transcript; got {why:?}"
    );
}

/// (c-2) The stateless claim carries no owning session id at all.
#[test]
fn stateless_claim_without_session_id_trips_unmeasured_with_honest_reason() {
    let fx = Fixture::new("nosid");
    let run = "flow-nosid";
    fx.write_transcript();
    fx.claim_stateless(run, None);

    let (code, v) = fx.circuit(run);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reason"], "idle_unmeasured", "{v}");
    let why = v["idle_unknown_reason"].as_str().expect("reason string");
    assert!(
        !why.contains("run state could not be loaded"),
        "the cause is the missing session id; got {why:?}"
    );
    assert!(
        why.contains("session id"),
        "the reason must name the missing session id; got {why:?}"
    );
}

/// (d) No run state and no claim at all: unchanged fail-closed behaviour.
#[test]
fn no_run_state_and_no_claim_still_trips_unmeasured_unchanged() {
    let fx = Fixture::new("ghost");
    // A transcript exists for the session, but nothing links it to this run.
    fx.write_transcript();
    let (code, v) = fx.circuit("flow-ghost");
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], "idle_unmeasured", "{v}");
    assert!(v["idle_secs"].is_null(), "{v}");
    assert_eq!(
        v["idle_unknown_reason"],
        "run state could not be loaded — time since last progress is unmeasurable",
        "{v}"
    );
}

/// (d') A stateless claim for a DIFFERENT run must not lend its transcript to
/// an unrelated run id.
#[test]
fn stateless_claim_of_another_run_does_not_measure_this_run() {
    let fx = Fixture::new("other");
    fx.write_transcript();
    fx.claim_stateless("flow-other", Some(SESSION));
    let (code, v) = fx.circuit("flow-ghost");
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reason"], "idle_unmeasured", "{v}");
    assert_eq!(
        v["idle_unknown_reason"],
        "run state could not be loaded — time since last progress is unmeasurable",
        "{v}"
    );
}

/// (e) A run WITH run state keeps the run-state idle path. A stateless claim
/// under the same id with a 2h-frozen transcript would trip `stall` if it were
/// (wrongly) consulted; the run-state path sees a task that just went
/// `running` and continues.
#[test]
fn run_with_run_state_ignores_stateless_claim_transcript() {
    let fx = Fixture::new("withstate");
    let run = "run-withstate";
    let dec = fx.repo.join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"edit","touched_files":["src/x.rs"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let init = fx.condukt(&[
        "state",
        "init",
        "--run",
        run,
        "--file",
        dec.to_str().unwrap(),
    ]);
    assert!(init.status.success(), "state init failed: {init:?}");
    let set = fx.condukt(&[
        "state", "set", "--run", run, "--task", "t1", "--status", "running",
    ]);
    assert!(set.status.success(), "state set failed: {set:?}");

    let t = fx.write_transcript();
    set_mtime_ago(&t, 7200);
    fx.claim_stateless(run, Some(SESSION));

    let (code, v) = fx.circuit(run);
    assert_eq!(
        code, 0,
        "a run with fresh run-state progress must continue regardless of a \
         stateless claim's transcript; got {v}"
    );
    let idle = v["idle_secs"].as_i64().expect("idle measured");
    assert!(idle < 600, "idle must come from run state; got {v}");
}
