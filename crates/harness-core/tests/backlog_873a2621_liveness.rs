#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent tests for backlog 873a2621 (user ruling 2026-10-04, design D).
//!
//! A session counts as a PEER only if it holds a LIVE entry (heartbeat within the
//! 1800s TTL) in condukt's claim registry (`claims.json`). Its footprint still
//! comes from its transcript. A registry that cannot be read/parsed means NO
//! exclusion (the restrictive direction).
//!
//! # Fixture mechanism (no internal signature is assumed)
//!
//! The registry lives at `<HOME>/.condukt/state/<project_key(main_worktree_root(dir))>/claims.json`
//! (`condukt::Config::load` -> `harness_core::config::base_dir("condukt")/state`,
//! `claim.rs::project_dir`). The tests swap `$HOME` to a temp dir (serialised by a
//! mutex) and write `claims.json` under the key for the process cwd AND for the
//! repo root the fixtures use, so the tests hold whichever of the two the
//! implementation keys on. Entry points exercised keep today's public signatures:
//! `peer_edit_footprint_in(projects, me)` and `peer_edit_footprint_within(projects, me, window)`.

use harness_core::projkey::{main_worktree_root, project_key};
use harness_core::transcript::{peer_edit_footprint_in, peer_edit_footprint_within};
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static ENV: Mutex<()> = Mutex::new(());
const WINDOW: Duration = Duration::from_secs(24 * 3600);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn edit_line(file_path: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{"file_path":"{file_path}","old_string":"a","new_string":"b"}}}}]}}}}"#
    )
}

fn transcript(projects: &Path, session: &str, file: &str) {
    let dir = projects.join("slug");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{session}.jsonl")), edit_line(file) + "\n").unwrap();
}

fn claim_json(session: &str, heartbeat_at: i64) -> String {
    format!(
        r#"{{"/work/a.rs":{{"run_id":"run-{session}","session_id":"{session}","pid":1,"claimed_at":{h},"heartbeat_at":{h}}},"task_claims":{{}}}}"#,
        h = heartbeat_at
    )
}

/// Every `claims.json` location an implementation could key on.
fn registry_paths(home: &Path, repo_root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![repo_root.to_path_buf()];
    dirs.push(std::env::current_dir().unwrap());
    let mut out = Vec::new();
    for d in dirs {
        if let Determination::Known(k) = main_worktree_root(&d) {
            out.push(
                home.join(".condukt/state")
                    .join(project_key(&k))
                    .join("claims.json"),
            );
        }
    }
    out
}

struct Fixture {
    _g: std::sync::MutexGuard<'static, ()>,
    old_home: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
    projects: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        let projects = tmp.path().join("projects");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        Fixture {
            _g: g,
            old_home,
            _tmp: tmp,
            projects,
            home,
            repo,
        }
    }
    fn write_registry(&self, body: &str) {
        let paths = registry_paths(&self.home, &self.repo);
        assert!(!paths.is_empty(), "no registry location could be derived");
        for p in paths {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }
    fn write_unreadable_registry(&self) {
        // claims.json is a directory -> read fails (EISDIR).
        for p in registry_paths(&self.home, &self.repo) {
            std::fs::create_dir_all(&p).unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        match &self.old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

const FILE: &str = "/work/peer_edited.rs";

fn both(fx: &Fixture) -> [std::collections::BTreeSet<String>; 2] {
    [
        peer_edit_footprint_in(&fx.projects, "me"),
        peer_edit_footprint_within(&fx.projects, "me", WINDOW),
    ]
}

/// (b) positive control: fresh transcript + live registry entry -> peer.
#[test]
fn live_registry_entry_makes_a_fresh_transcript_a_peer() {
    let fx = Fixture::new();
    transcript(&fx.projects, "peer-live", FILE);
    fx.write_registry(&claim_json("peer-live", now()));
    for fp in both(&fx) {
        assert!(
            fp.contains(FILE),
            "a session with a fresh transcript AND a live claim-registry entry must be a peer \
             (positive control); got {fp:?}"
        );
    }
}

/// (a) fresh transcript, session absent from the registry -> not a peer.
#[test]
fn fresh_transcript_without_registry_entry_is_not_a_peer() {
    let fx = Fixture::new();
    transcript(&fx.projects, "peer-unregistered", FILE);
    // A registry exists and is live, but names a DIFFERENT session.
    fx.write_registry(&claim_json("someone-else", now()));
    for fp in both(&fx) {
        assert!(
            fp.is_empty(),
            "transcript mtime is not liveness: a session with no live registry entry must not \
             exclude files; got {fp:?}"
        );
    }
}

/// (a, variant) no registry file at all (nobody holds a claim) -> not a peer.
#[test]
fn fresh_transcript_with_no_registry_file_is_not_a_peer() {
    let fx = Fixture::new();
    transcript(&fx.projects, "peer-unregistered", FILE);
    for fp in both(&fx) {
        assert!(fp.is_empty(), "no registry => no live peer; got {fp:?}");
    }
}

/// (c) registry entry whose heartbeat is older than the 1800s TTL -> not a peer.
#[test]
fn stale_heartbeat_registry_entry_is_not_a_peer() {
    let fx = Fixture::new();
    transcript(&fx.projects, "peer-stale-hb", FILE);
    fx.write_registry(&claim_json("peer-stale-hb", now() - 3600));
    for fp in both(&fx) {
        assert!(
            fp.is_empty(),
            "a claim whose heartbeat is 1h old (TTL 1800s) is a dead holder, not a peer; got {fp:?}"
        );
    }
}

/// (d) corrupt registry -> NO exclusion (restrictive), even with a fresh transcript.
#[test]
fn corrupt_registry_yields_no_exclusion() {
    let fx = Fixture::new();
    transcript(&fx.projects, "peer-live", FILE);
    fx.write_registry("{ this is not json");
    for fp in both(&fx) {
        assert!(
            fp.is_empty(),
            "an unparseable registry is 'cannot determine liveness' -> no exclusion; got {fp:?}"
        );
    }
}

/// (d, variant) registry present but unreadable -> NO exclusion.
#[test]
fn unreadable_registry_yields_no_exclusion() {
    let fx = Fixture::new();
    transcript(&fx.projects, "peer-live", FILE);
    fx.write_unreadable_registry();
    for fp in both(&fx) {
        assert!(
            fp.is_empty(),
            "an unreadable registry must not be read as 'live peer'; got {fp:?}"
        );
    }
}
