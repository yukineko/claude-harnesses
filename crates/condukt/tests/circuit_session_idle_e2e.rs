// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt circuit check --run flow-S --session S` for a run with NO run-state
//! and NO stateless claim: idle is measured from S's transcript mtime; every
//! non-matching shape stays fail-closed `idle_unmeasured`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

const SESSION: &str = "sess-circuit-e2e";
const UNMEASURED: &str = "idle_unmeasured";

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
        base.push(format!("condukt-circuit-session-{pid}-{tag}"));
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
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .expect("spawn condukt")
    }

    /// `<home>/.claude/projects/-fixture/<SESSION>.jsonl`, written fresh.
    fn write_transcript(&self) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{SESSION}.jsonl"));
        std::fs::write(&p, "{\"seed\":1}\n").unwrap();
        p
    }

    /// Run `circuit check` (optionally with `--session`); `(exit code, JSON)`.
    fn circuit(&self, run: &str, session: Option<&str>) -> (i32, serde_json::Value) {
        let mut args = vec!["circuit", "check", "--run", run, "--idle-ttl-secs", "1800"];
        if let Some(s) = session {
            args.push("--session");
            args.push(s);
        }
        let out = self.condukt(&args);
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

#[test]
fn session_flag_fresh_transcript_continues() {
    let fx = Fixture::new("fresh");
    fx.write_transcript();
    let (code, v) = fx.circuit(&format!("flow-{SESSION}"), Some(SESSION));
    assert_eq!(code, 0, "fresh transcript must continue; got {v}");
    assert_eq!(v["verdict"], "continue", "{v}");
    let idle = v["idle_secs"]
        .as_i64()
        .unwrap_or_else(|| panic!("idle_secs must be measured: {v}"));
    assert!((0..600).contains(&idle), "{v}");
}

#[test]
fn session_flag_stale_transcript_trips_on_idle_not_unmeasured() {
    let fx = Fixture::new("stale");
    let t = fx.write_transcript();
    set_mtime_ago(&t, 7200);
    let (code, v) = fx.circuit(&format!("flow-{SESSION}"), Some(SESSION));
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_ne!(
        v["reason"], UNMEASURED,
        "stale transcript is a measured stall: {v}"
    );
    assert!(v["idle_secs"].as_i64().unwrap_or(0) >= 7000, "{v}");
}

#[test]
fn session_flag_missing_transcript_trips_unmeasured() {
    let fx = Fixture::new("notx");
    let (code, v) = fx.circuit(&format!("flow-{SESSION}"), Some(SESSION));
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], UNMEASURED, "{v}");
}

#[test]
fn session_flag_run_id_mismatch_trips_unmeasured() {
    let fx = Fixture::new("mismatch");
    fx.write_transcript();
    let (code, v) = fx.circuit("flow-OTHER", Some(SESSION));
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], UNMEASURED, "{v}");
}

#[test]
fn no_session_flag_trips_unmeasured_control() {
    let fx = Fixture::new("nosess");
    fx.write_transcript();
    let (code, v) = fx.circuit(&format!("flow-{SESSION}"), None);
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], UNMEASURED, "{v}");
}
