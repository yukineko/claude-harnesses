// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED regression for backlog 6a668d24: an UNMARKED (no `--stateless`) task
//! claim whose run state does not exist, whose owning session transcript is
//! determinately absent from `~/.claude/projects/*/`, and whose
//! `claimed_at == heartbeat_at` is ~51 days old, must NOT make
//! `condukt state is-claimed` report `claimed: true` forever.
//!
//! Fixture: the real binary, a sandboxed `$HOME` (state dir and
//! `~/.claude/projects` both resolve under it) and a throwaway git repo. The
//! claim is created by the real `state claim-task` (no `--stateless`); "old"
//! is then simulated by rewriting `claimed_at` and `heartbeat_at` of that
//! registry row in the sandboxed `claims.json` to now - 51 days (the binary has
//! no clock override, so editing the persisted timestamps is the only honest
//! way to age a claim). Nothing else in the row is touched.
//!
//! * (1) dead claim -> is-claimed exit 1 with `"claimed": false`   (RED today)
//! * (2) control: fresh claim stays claimed (exit 0)
//! * (3) same old claim, but transcript liveness CANNOT be determined
//!   (`~/.claude/projects` unreadable) -> stays claimed (fail-closed), so a fix
//!   cannot simply expire everything.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

const SESSION: &str = "sess-6a668d24";
const RUN: &str = "flow-sess-6a668d24";
const HASHKEY: &str = "hk-6a668d24";
const FIFTY_ONE_DAYS: i64 = 51 * 24 * 3600;

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Restore perms so removal of a chmod-000 projects dir succeeds.
        let projects = self.home.join(".claude").join("projects");
        let _ = set_mode(&projects, 0o755);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
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
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-6a668d24-{}-{tag}", std::process::id()));
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
        // Projects dir exists and is listable, with an unrelated project that
        // holds NO transcript for SESSION: the transcript is determinately absent.
        std::fs::create_dir_all(home.join(".claude").join("projects").join("-other")).unwrap();
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            // Reaping is multi-sample: window 0 lets the 2nd identical sample stall.
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .output()
            .expect("spawn condukt")
    }

    /// Unmarked claim (NO --stateless), exactly as an older /flow made it.
    fn claim_unmarked(&self, run: &str) {
        let out = self.condukt(&[
            "state",
            "claim-task",
            "--run",
            run,
            "--session",
            SESSION,
            "--title",
            "t",
            "--hashkey",
            HASHKEY,
        ]);
        assert!(out.status.success(), "claim-task must succeed: {out:?}");
        let claims =
            String::from_utf8_lossy(&self.condukt(&["state", "claims"]).stdout).into_owned();
        assert!(
            claims.contains(run) && !claims.contains("\"stateless\""),
            "precondition: registry must hold an UNMARKED claim for {run}; got {claims}"
        );
    }

    fn registry(&self) -> PathBuf {
        let found: Vec<PathBuf> = std::fs::read_dir(self.home.join(".condukt").join("state"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path().join("claims.json"))
                    .filter(|p| p.is_file())
                    .collect()
            })
            .unwrap_or_default();
        if found.len() == 1 {
            return found[0].clone();
        }
        // Layout fallback: search the whole sandboxed HOME.
        let mut hits = Vec::new();
        fn walk(d: &Path, hits: &mut Vec<PathBuf>) {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, hits);
                    } else if p.file_name().is_some_and(|n| n == "claims.json") {
                        hits.push(p);
                    }
                }
            }
        }
        walk(&self.home, &mut hits);
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one sandboxed claims.json: {hits:?}"
        );
        hits.remove(0)
    }

    /// Age the claim: claimed_at == heartbeat_at == now - 51 days.
    fn age_claim(&self) {
        let path = self.registry();
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let old = now - FIFTY_ONE_DAYS;
        let row = &mut v["task_claims"][HASHKEY];
        assert!(row.is_object(), "claim row for {HASHKEY} missing: {v}");
        row["claimed_at"] = old.into();
        row["heartbeat_at"] = old.into();
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    fn is_claimed(&self) -> (i32, String) {
        let out = self.condukt(&["state", "is-claimed", "--hashkey", HASHKEY]);
        (
            out.status.code().expect("exit code"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }
}

/// Two `is-claimed` calls: reaping is multi-sample (`progress::sample` is
/// Undetermined on the first observation), so the first must still say
/// claimed and only the SECOND can reap. Returns the second call's result.
fn two_calls(fx: &Fixture) -> (i32, String) {
    let (c1, o1) = fx.is_claimed();
    assert!(
        c1 == 0 && o1.contains("\"claimed\": true"),
        "first sample must still be claimed (no prior sample); got exit {c1}, stdout={o1}"
    );
    fx.is_claimed()
}

/// (1) Determinately dead: unmarked, no run state, transcript absent from a
/// readable projects dir, 51 days old and never heartbeated. Must be free on
/// the second sample.
#[test]
fn dead_unmarked_claim_is_not_reported_claimed() {
    let fx = Fixture::new("dead");
    fx.claim_unmarked(RUN);
    fx.age_claim();
    let (code, stdout) = two_calls(&fx);
    assert!(
        code == 1 && stdout.contains("\"claimed\": false"),
        "a 51-day-old, never-heartbeated, unmarked /flow claim with no run state and no \
         transcript must be reapable on the 2nd sample (exit 1, claimed:false); \
         got exit {code}, stdout={stdout}"
    );
}

/// (4) Aged unmarked claim whose run id is NOT `flow-<session id>`: its run
/// state may live under another worktree's key, so run-state absence is not
/// evidence of death. Must stay claimed after two samples.
#[test]
fn old_non_flow_run_claim_stays_claimed() {
    let fx = Fixture::new("nonflow");
    fx.claim_unmarked("run-20260801-x");
    fx.age_claim();
    let (code, stdout) = two_calls(&fx);
    assert_eq!(
        code, 0,
        "non-/flow run claim must stay claimed; stdout={stdout}"
    );
    assert!(stdout.contains("\"claimed\": true"), "{stdout}");
}

/// (2) Control: a fresh claim is live and stays claimed.
#[test]
fn fresh_unmarked_claim_stays_claimed() {
    let fx = Fixture::new("fresh");
    fx.claim_unmarked(RUN);
    let (code, stdout) = two_calls(&fx);
    assert_eq!(code, 0, "fresh claim must stay claimed; stdout={stdout}");
    assert!(stdout.contains("\"claimed\": true"), "{stdout}");
}

/// (3) Same old claim, but transcript liveness cannot be determined
/// (`~/.claude/projects` unreadable): fail closed, stay claimed.
#[test]
fn old_claim_with_undeterminable_transcript_stays_claimed() {
    let fx = Fixture::new("undet");
    fx.claim_unmarked(RUN);
    fx.age_claim();
    let projects = fx.home.join(".claude").join("projects");
    set_mode(&projects, 0o000).unwrap();
    // Observed, not assumed: the dir really is unreadable to this process.
    assert!(
        std::fs::read_dir(&projects).is_err(),
        "precondition: projects dir must be unreadable (running as root?)"
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| two_calls(&fx)));
    set_mode(&projects, 0o755).unwrap();
    let (code, stdout) = r.unwrap();
    assert_eq!(
        code, 0,
        "undeterminable transcript liveness must stay claimed (fail-closed); stdout={stdout}"
    );
    assert!(stdout.contains("\"claimed\": true"), "{stdout}");
}

/// B-1 (verifier finding): a LIVE holder whose transcript lives under
/// `$CLAUDE_CONFIG_DIR/projects` (not `$HOME/.claude/projects`) must stay
/// claimed. `$HOME/.claude/projects` exists, is readable and has no transcript
/// for the session, so the reaper reads "determinately absent" and reaps a
/// holder whose transcript is in fact GROWING. Holder and reaper share the env.
fn live_holder_under_config_dir(extra_claim_args: &[&str]) {
    let fx = Fixture::new(if extra_claim_args.is_empty() {
        "cfgdir"
    } else {
        "cfgdir-sl"
    });
    let cfg = fx.base.join("cfg");
    let slug = cfg.join("projects").join("-live-slug");
    std::fs::create_dir_all(&slug).unwrap();
    let transcript = slug.join(format!("{SESSION}.jsonl"));
    std::fs::write(&transcript, "{\"n\":0}\n").unwrap();
    let run = |args: &[&str]| -> Output {
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&fx.repo)
            .env("HOME", &fx.home)
            .env("CLAUDE_CONFIG_DIR", &cfg)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .output()
            .expect("spawn condukt")
    };
    let mut args = vec![
        "state",
        "claim-task",
        "--run",
        RUN,
        "--session",
        SESSION,
        "--title",
        "t",
        "--hashkey",
        HASHKEY,
    ];
    args.extend_from_slice(extra_claim_args);
    let out = run(&args);
    assert!(out.status.success(), "claim-task must succeed: {out:?}");
    fx.age_claim();
    let isc = || {
        let o = run(&["state", "is-claimed", "--hashkey", HASHKEY]);
        (
            o.status.code().expect("exit code"),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    };
    let (c1, o1) = isc();
    assert!(
        c1 == 0 && o1.contains("\"claimed\": true"),
        "first sample: {c1} {o1}"
    );
    // The holder is alive: its transcript grows between the samples.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        f,
        "{{\"n\":1,\"pad\":\"more output from the live session\"}}"
    )
    .unwrap();
    drop(f);
    let (code, stdout) = isc();
    assert!(
        code == 0 && stdout.contains("\"claimed\": true"),
        "a live holder (transcript under CLAUDE_CONFIG_DIR, growing) was reaped: \
         exit {code}, stdout={stdout}"
    );
}

#[test]
fn live_holder_with_transcript_under_claude_config_dir_stays_claimed() {
    live_holder_under_config_dir(&[]);
}

#[test]
fn live_stateless_holder_with_transcript_under_claude_config_dir_stays_claimed() {
    live_holder_under_config_dir(&["--stateless"]);
}
