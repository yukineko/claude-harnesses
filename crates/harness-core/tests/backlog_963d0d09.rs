// Integration test: unwrap/expect/panic are fine here (the workspace lints are
// production-facing).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 963d0d09 reproduction: the peer-activity window is judged from the
//! transcript FILE's mtime, which `rsync` / restore / `touch` re-dates. A
//! transcript whose own records are all a week old is then still a "peer" and
//! its footprint can exclude a file from a gate.
//!
//! Audit-written (not the implementer). Open defect: remove the `ignore` when
//! `peer_edit_footprint_within` judges activity by the records' own
//! `"timestamp"` instead of the file mtime.

use std::path::PathBuf;
use std::time::Duration;

use harness_core::transcript::peer_edit_footprint_within;

// Live claim-registry fixture: HOME is swapped (mutex-serialised) to a private dir
// holding HOME/.condukt/state/<project_key(main_worktree_root(cwd))>/claims.json,
// because `peer_edit_footprint_within` keys the registry by the process cwd.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct LiveRegistry {
    _g: std::sync::MutexGuard<'static, ()>,
    old_home: Option<std::ffi::OsString>,
    home: PathBuf,
}

impl LiveRegistry {
    fn for_cwd(live_sessions: &[&str]) -> LiveRegistry {
        let g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("hc-963d0d09-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut body = String::from("{");
        for (i, s) in live_sessions.iter().enumerate() {
            if i > 0 {
                body.push(',');
            }
            body.push_str(&format!(
                r#""/work/claimed-{i}.rs":{{"run_id":"run-{s}","session_id":"{s}","pid":1,"claimed_at":{now},"heartbeat_at":{now}}}"#
            ));
        }
        body.push('}');
        let cwd = std::env::current_dir().unwrap();
        match harness_core::projkey::main_worktree_root(&cwd) {
            harness_core::verdict::Determination::Known(k) => {
                let dir = home
                    .join(".condukt/state")
                    .join(harness_core::projkey::project_key(&k));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("claims.json"), &body).unwrap();
            }
            _ => panic!("cannot resolve the main worktree root of cwd {cwd:?}"),
        }
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        LiveRegistry {
            _g: g,
            old_home,
            home,
        }
    }
}

impl Drop for LiveRegistry {
    fn drop(&mut self) {
        match &self.old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn fixture(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hc-963d0d09-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn edit_line(file_path: &str, ts: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","timestamp":"{ts}","message":{{"model":"claude-opus-5","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"Edit","input":{{"file_path":"{file_path}","old_string":"a","new_string":"b"}}}}]}}}}"#
    )
}

/// Control: a transcript with fresh mtime AND a fresh record timestamp is a peer.
#[test]
fn control_a_genuinely_active_peer_claims_its_file() {
    let root = fixture("fresh");
    let slug = root.join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    let now = "2099-01-01T00:00:00.000Z"; // content-derived time is irrelevant to today's code
    std::fs::write(
        slug.join("peer.jsonl"),
        edit_line("/tmp/claimed.rs", now) + "\n",
    )
    .unwrap();
    // Re-anchored (user ruling 2026-10-04 design D, backlog 873a2621): a "genuinely
    // active" peer is one holding a LIVE entry in condukt's claim registry. The
    // assertion below is unchanged.
    let _live = LiveRegistry::for_cwd(&["peer"]);
    let got = peer_edit_footprint_within(&root, "me", Duration::from_secs(24 * 3600));
    assert!(
        got.contains("/tmp/claimed.rs"),
        "fresh peer must be admitted: {got:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "backlog 963d0d09: open defect, remove ignore when fixed"]
fn a_transcript_whose_records_are_a_week_old_is_not_a_peer_even_if_its_mtime_is_now() {
    let root = fixture("stale");
    let slug = root.join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    // Every record inside is from 2020; the FILE was just (re)written, exactly
    // what an rsync / restore / touch of the projects store does to its mtime.
    std::fs::write(
        slug.join("old.jsonl"),
        edit_line("/tmp/not-claimed.rs", "2020-01-01T00:00:00.000Z") + "\n",
    )
    .unwrap();
    let got = peer_edit_footprint_within(&root, "me", Duration::from_secs(24 * 3600));
    assert!(
        !got.contains("/tmp/not-claimed.rs"),
        "a session whose own records are years old was admitted as a peer on the strength of a \
         fresh file mtime: {got:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
