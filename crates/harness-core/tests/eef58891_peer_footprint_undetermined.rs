#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent tests for backlog eef58891 (user ruling: cannot-determine peer
//! liveness must be PROPAGATED, not collapsed to an empty set).
//!
//! Contract under test:
//!
//! 1. `peer_edit_footprint_for`, `peer_edit_footprint_within` and
//!    `peer_edit_footprint_in` return `Determination<BTreeSet<String>>`.
//! 2. condukt's claim registry unparseable or unreadable -> `_for` is
//!    `Determination::Undetermined`, NOT `Known(empty)`.
//! 3. `std::env::current_dir()` failing -> `_within` is `Undetermined`.
//! 4. `attribute_from_transcript` with a readable own transcript but an
//!    unparseable registry -> `Attribution::Undetermined { why }` (non-empty
//!    `why`), not a `Narrowed` that is indistinguishable from "no peers".
//!
//! Positive controls (must stay green before and after the change): a valid
//! registry naming a live peer yields that peer's file; an absent registry
//! yields Known(empty) / Narrowed.
//!
//! Fixture mechanics follow `backlog_873a2621_liveness.rs`: `$HOME` is swapped
//! to a temp dir and the registry lives at
//! `<HOME>/.condukt/state/<project_key(main_worktree_root(root))>/claims.json`.
//! HOME and cwd are process-global, so EVERY test here holds one static mutex
//! and restores HOME / HARNESS_SANDBOX_HOME / cwd in a Drop guard.
//!
//! The `attribute_from_transcript` tests (section B) use no Determination-typed
//! footprint API, so they compile against the pre-change implementation too and
//! give a behavioural RED there.

use harness_core::attribution::{attribute_from_transcript, Attribution};
use harness_core::projkey::{main_worktree_root, project_key};
use harness_core::verdict::Determination;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static ENV: Mutex<()> = Mutex::new(());
const WINDOW: Duration = Duration::from_secs(24 * 3600);
const SANDBOX_HOME_ENV: &str = "HARNESS_SANDBOX_HOME";

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

fn claim_json(session: &str, heartbeat_at: i64) -> String {
    format!(
        r#"{{"/work/a.rs":{{"run_id":"run-{session}","session_id":"{session}","pid":1,"claimed_at":{h},"heartbeat_at":{h}}},"task_claims":{{}}}}"#,
        h = heartbeat_at
    )
}

struct Fixture {
    _g: std::sync::MutexGuard<'static, ()>,
    old_home: Option<std::ffi::OsString>,
    old_sandbox: Option<std::ffi::OsString>,
    old_cwd: PathBuf,
    _tmp: tempfile::TempDir,
    home: PathBuf,
    /// The repo root the registry is keyed on (no `.git`, so it is its own
    /// main worktree root).
    repo: PathBuf,
    /// `<tmp>/projects` — the grandparent of every transcript.
    projects: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let old_cwd = std::env::current_dir().expect("cwd readable at fixture start");
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        let projects = tmp.path().join("projects");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(projects.join("slug")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_sandbox = std::env::var_os(SANDBOX_HOME_ENV);
        std::env::set_var("HOME", &home);
        std::env::remove_var(SANDBOX_HOME_ENV);
        assert_eq!(
            harness_core::config::base_dir("condukt"),
            home.join(".condukt"),
            "precondition: HOME swap must redirect condukt's state dir"
        );
        Fixture {
            _g: g,
            old_home,
            old_sandbox,
            old_cwd,
            _tmp: tmp,
            home,
            repo,
            projects,
        }
    }

    fn registry_path(&self) -> PathBuf {
        let key_root = match main_worktree_root(&self.repo) {
            Determination::Known(k) => k,
            Determination::Undetermined(why) => {
                panic!("precondition: main_worktree_root(repo) undetermined: {why}")
            }
        };
        self.home
            .join(".condukt/state")
            .join(project_key(&key_root))
            .join("claims.json")
    }

    fn write_registry(&self, body: &str) {
        let p = self.registry_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }

    fn write_unreadable_registry(&self) {
        // claims.json is a directory -> read fails (EISDIR).
        std::fs::create_dir_all(self.registry_path()).unwrap();
    }

    /// Write `<projects>/slug/<session>.jsonl` with one Edit of `file`.
    fn transcript(&self, session: &str, file: &Path) -> PathBuf {
        let p = self.projects.join("slug").join(format!("{session}.jsonl"));
        std::fs::write(&p, edit_line(&file.to_string_lossy()) + "\n").unwrap();
        p
    }

    /// A real file under the repo (so `canonicalize` resolves it on both sides).
    fn repo_file(&self, rel: &str) -> PathBuf {
        let p = self.repo.join(rel);
        std::fs::write(&p, "x\n").unwrap();
        p
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.old_cwd);
        match &self.old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match &self.old_sandbox {
            Some(s) => std::env::set_var(SANDBOX_HOME_ENV, s),
            None => std::env::remove_var(SANDBOX_HOME_ENV),
        }
    }
}

// ---------------------------------------------------------------------------
// Section A: the footprint functions return Determination (contract 1, 2, 3, 5)
// ---------------------------------------------------------------------------

mod footprint {
    use super::*;
    use harness_core::transcript::{
        peer_edit_footprint_for, peer_edit_footprint_in, peer_edit_footprint_within,
    };

    fn known(d: Determination<BTreeSet<String>>, ctx: &str) -> BTreeSet<String> {
        match d {
            Determination::Known(s) => s,
            Determination::Undetermined(why) => {
                panic!("{ctx}: expected Known, got Undetermined({why})")
            }
        }
    }

    fn assert_undetermined(d: Determination<BTreeSet<String>>, ctx: &str) {
        match d {
            Determination::Undetermined(why) => assert!(
                !why.as_str().trim().is_empty(),
                "{ctx}: Undetermined must carry a reason"
            ),
            Determination::Known(s) => panic!(
                "{ctx}: cannot-determine must be PROPAGATED as Undetermined, but got \
                 Known({s:?}) — indistinguishable from 'liveness determined, no peers'"
            ),
        }
    }

    /// Contract 1: the signatures themselves.
    #[test]
    fn all_three_entry_points_return_a_determination() {
        let fx = Fixture::new();
        let _a: Determination<BTreeSet<String>> =
            peer_edit_footprint_for(&fx.projects, "me", WINDOW, &fx.repo);
        let _b: Determination<BTreeSet<String>> =
            peer_edit_footprint_within(&fx.projects, "me", WINDOW);
        let _c: Determination<BTreeSet<String>> = peer_edit_footprint_in(&fx.projects, "me");
    }

    /// Positive control: valid registry + live peer -> Known({peer file}).
    #[test]
    fn for_live_peer_is_known_with_its_file() {
        let fx = Fixture::new();
        let f = fx.repo_file("peer.rs");
        fx.transcript("peer", &f);
        fx.write_registry(&claim_json("peer", now()));
        let fp = known(
            peer_edit_footprint_for(&fx.projects, "me", WINDOW, &fx.repo),
            "live peer",
        );
        assert!(
            fp.contains(&*f.to_string_lossy()),
            "a live peer's edited file must be in the footprint; got {fp:?}"
        );
    }

    /// Positive control: no registry file at all -> Known(empty) (nobody holds
    /// a claim; that IS a determination).
    #[test]
    fn for_absent_registry_is_known_empty() {
        let fx = Fixture::new();
        let f = fx.repo_file("peer.rs");
        fx.transcript("peer", &f);
        assert!(!fx.registry_path().exists(), "precondition: no registry");
        let fp = known(
            peer_edit_footprint_for(&fx.projects, "me", WINDOW, &fx.repo),
            "absent registry",
        );
        assert!(fp.is_empty(), "absent registry => no live peer; got {fp:?}");
    }

    /// Contract 2: unparseable registry -> Undetermined.
    #[test]
    fn for_unparseable_registry_is_undetermined() {
        let fx = Fixture::new();
        let f = fx.repo_file("peer.rs");
        fx.transcript("peer", &f);
        fx.write_registry("not json");
        assert_undetermined(
            peer_edit_footprint_for(&fx.projects, "me", WINDOW, &fx.repo),
            "unparseable registry",
        );
    }

    /// Contract 2: unreadable registry (claims.json is a directory) -> Undetermined.
    #[test]
    fn for_unreadable_registry_is_undetermined() {
        let fx = Fixture::new();
        let f = fx.repo_file("peer.rs");
        fx.transcript("peer", &f);
        fx.write_unreadable_registry();
        assert!(
            fx.registry_path().is_dir(),
            "precondition: claims.json is a directory"
        );
        assert_undetermined(
            peer_edit_footprint_for(&fx.projects, "me", WINDOW, &fx.repo),
            "unreadable registry",
        );
    }

    /// Contract 3: current_dir() fails -> `_within` (and `_in`) Undetermined.
    #[test]
    fn within_with_no_cwd_is_undetermined() {
        let fx = Fixture::new();
        let f = fx.repo_file("peer.rs");
        fx.transcript("peer", &f);
        // A registry that WOULD make the peer live, so a Known answer is not
        // vacuously empty-for-another-reason (keyed on the vanished cwd it
        // cannot be found anyway; the point is the cwd failure itself).
        fx.write_registry(&claim_json("peer", now()));

        let gone = fx._tmp.path().join("vanishing-cwd");
        std::fs::create_dir_all(&gone).unwrap();
        std::env::set_current_dir(&gone).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        assert!(
            std::env::current_dir().is_err(),
            "precondition: on this platform current_dir() must fail once the cwd is \
             removed; got {:?} — without that, this test proves nothing",
            std::env::current_dir()
        );

        let within = peer_edit_footprint_within(&fx.projects, "me", WINDOW);
        let in_ = peer_edit_footprint_in(&fx.projects, "me");
        // Restore cwd before asserting (Drop also does this).
        std::env::set_current_dir(&fx.old_cwd).unwrap();

        assert_undetermined(within, "_within with no cwd");
        assert_undetermined(in_, "_in with no cwd");
    }
}

// ---------------------------------------------------------------------------
// Section B: attribute_from_transcript (contract 4). Uses no Determination-typed
// footprint API, so it compiles against the pre-change implementation as well.
// ---------------------------------------------------------------------------

mod attribution {
    use super::*;

    /// Common setup: my transcript edits `mine.rs`, a peer transcript edits
    /// `peer.rs`; both are changed files.
    fn setup(fx: &Fixture) -> (String, Vec<String>) {
        let mine = fx.repo_file("mine.rs");
        let peer = fx.repo_file("peer.rs");
        let me = fx.transcript("me", &mine);
        fx.transcript("peer", &peer);
        assert_eq!(
            me.parent().and_then(Path::parent).and_then(Path::file_name),
            Some(std::ffi::OsStr::new("projects")),
            "precondition: my transcript's grandparent must be named `projects`"
        );
        (
            me.to_string_lossy().into_owned(),
            vec!["mine.rs".to_string(), "peer.rs".to_string()],
        )
    }

    /// Contract 4: own transcript readable, registry unparseable -> Undetermined.
    #[test]
    fn unparseable_registry_makes_attribution_undetermined() {
        let fx = Fixture::new();
        let (me, changed) = setup(&fx);
        fx.write_registry("not json");
        match attribute_from_transcript(&fx.repo, &me, &changed) {
            Attribution::Undetermined { why } => assert!(
                !why.trim().is_empty(),
                "Undetermined must carry a non-empty why"
            ),
            other => panic!(
                "registry unparseable => peer liveness cannot be determined; that must \
                 surface as Attribution::Undetermined, not be collapsed into a Narrowed \
                 that reads as 'no peers'. got {other:?}"
            ),
        }
    }

    /// Contract 4 (variant): unreadable registry -> Undetermined.
    #[test]
    fn unreadable_registry_makes_attribution_undetermined() {
        let fx = Fixture::new();
        let (me, changed) = setup(&fx);
        fx.write_unreadable_registry();
        match attribute_from_transcript(&fx.repo, &me, &changed) {
            Attribution::Undetermined { why } => assert!(
                !why.trim().is_empty(),
                "Undetermined must carry a non-empty why"
            ),
            other => panic!(
                "registry unreadable => peer liveness cannot be determined; expected \
                 Attribution::Undetermined, got {other:?}"
            ),
        }
    }

    /// Positive control: valid registry with a live peer -> Narrowed, peer's
    /// file excluded, mine kept.
    #[test]
    fn live_peer_registry_narrows_and_excludes_peer_file() {
        let fx = Fixture::new();
        let (me, changed) = setup(&fx);
        fx.write_registry(&claim_json("peer", now()));
        match attribute_from_transcript(&fx.repo, &me, &changed) {
            Attribution::Narrowed { keep, excluded } => {
                assert_eq!(
                    excluded,
                    vec!["peer.rs".to_string()],
                    "live peer's file must be excluded"
                );
                assert_eq!(keep, vec!["mine.rs".to_string()]);
            }
            other => panic!("valid registry with live peer must narrow; got {other:?}"),
        }
    }

    /// Control: registry absent -> Narrowed with no exclusion (Known empty
    /// peers is a determination, not Undetermined).
    #[test]
    fn absent_registry_narrows_without_exclusion() {
        let fx = Fixture::new();
        let (me, changed) = setup(&fx);
        assert!(!fx.registry_path().exists(), "precondition: no registry");
        match attribute_from_transcript(&fx.repo, &me, &changed) {
            Attribution::Narrowed { keep, excluded } => {
                assert!(excluded.is_empty(), "no live peer => no exclusion");
                assert_eq!(keep, changed);
            }
            other => panic!("absent registry is Known(no peers) => Narrowed; got {other:?}"),
        }
    }
}
