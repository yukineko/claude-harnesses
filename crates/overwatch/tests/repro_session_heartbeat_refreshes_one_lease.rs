#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro (observed live 2026-10-20, session 6d32fd65): a session holding two
//! leases only ever has ONE of them heartbeated by stuckguard's per-tool-call
//! piggyback (`overwatch lease --session S --json` -> `overwatch heartbeat
//! --key <that key>`), because `lease_for_session` returns a single lease
//! (max claimed_at).  The other lease of a LIVE session goes stale and is
//! silently dropped by the next mutator's `reap_stale`.
//!
//! Sandboxed: HOME and the cwd repo are temp dirs; the live ~/.overwatch is
//! never touched.
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
}

fn find(root: &Path, name: &str, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                find(&p, name, out);
            } else if p.file_name().is_some_and(|n| n == name) {
                out.push(p);
            }
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Sb {
    t: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

impl Sb {
    fn new(tag: &str) -> Sb {
        let t = std::env::temp_dir().join(format!("ow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let home = t.join("home");
        let cwd = t.join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(harness_core::git_env::scrub(&mut Command::new("git"))
            .current_dir(&cwd)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        Sb { t, home, cwd }
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .args(args)
            .output()
            .unwrap()
    }
    fn file(&self, name: &str) -> PathBuf {
        let mut v = Vec::new();
        find(&self.home, name, &mut v);
        assert_eq!(v.len(), 1, "fixture: expected one {name}, got {v:?}");
        v.remove(0)
    }
    /// Backdate the heartbeat of `key` to `now - secs_ago`.
    fn backdate(&self, key: &str, secs_ago: i64) {
        let p = self.file("leases.json");
        let mut j: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        j[key]["heartbeat_at"] = serde_json::json!(now() - secs_ago);
        std::fs::write(&p, serde_json::to_string_pretty(&j).unwrap()).unwrap();
    }
    fn heartbeat_of(&self, key: &str) -> Option<i64> {
        let j: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.file("leases.json")).unwrap())
                .unwrap();
        j.get(key)?.get("heartbeat_at")?.as_i64()
    }
    fn begin(&self, key: &str, session: &str) {
        let o = self.run(&["begin", "--key", key, "--title", key, "--session", session]);
        assert!(o.status.success(), "fixture begin {key} failed: {o:?}");
    }
}

impl Drop for Sb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.t);
    }
}

#[test]
fn session_heartbeat_piggyback_refreshes_all_session_leases() {
    let sb = Sb::new("hb-one-lease");
    sb.begin("keyA", "S");
    sb.begin("keyB", "S");
    sb.backdate("keyA", 1000);
    sb.backdate("keyB", 1000);

    // Exactly what stuckguard heartbeat_piggyback does.
    let l = sb.run(&["lease", "--session", "S", "--json"]);
    assert!(l.status.success(), "lease lookup failed: {l:?}");
    let v: serde_json::Value = serde_json::from_slice(&l.stdout).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    let h = sb.run(&["heartbeat", "--key", &key]);
    assert!(h.status.success(), "heartbeat failed: {h:?}");

    let floor = now() - 60;
    let a = sb.heartbeat_of("keyA").expect("keyA still present");
    let b = sb.heartbeat_of("keyB").expect("keyB still present");
    assert!(
        a >= floor && b >= floor,
        "live session S holds two leases but the piggyback refreshed only {key}: keyA heartbeat age={}s, keyB heartbeat age={}s (want both < 60s)",
        now() - a,
        now() - b
    );
}

#[test]
fn stale_lease_reap_by_a_mutator_is_not_silent() {
    let sb = Sb::new("reap-silent");
    sb.begin("keyA", "S");
    sb.begin("keyB", "S");
    let events = sb.file("events.jsonl");
    sb.backdate("keyA", 2000); // beyond LEASE_TTL_SECS (1800)
    let before = std::fs::read_to_string(&events).unwrap_or_default();

    // A mutator (heartbeat on the other key) runs reap_stale and saves.
    let h = sb.run(&["heartbeat", "--key", "keyB"]);
    assert!(h.status.success(), "heartbeat failed: {h:?}");
    assert!(
        sb.heartbeat_of("keyA").is_none(),
        "fixture: expected keyA to have been reaped"
    );

    let after = std::fs::read_to_string(&events).unwrap_or_default();
    let new: Vec<&str> = after.lines().skip(before.lines().count()).collect();
    assert!(
        new.iter().any(|l| l.contains("keyA")),
        "keyA lease was dropped by reap_stale but no event mentioning it was appended to events.jsonl (new lines: {new:?})"
    );
}
