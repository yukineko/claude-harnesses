//! backlog 486e83ca: a claim lease must not lapse under a LIVE holder.
//!
//! Before the fix, a `next --claim` lease in `~/.backlog/claims/<slug>.json`
//! stopped excluding its task once `claimed_at` was older than
//! `CLAIM_STALE_SECS` (3600 s) — purely by age. A session working one task for
//! more than an hour silently lost its claim and a second session could
//! `next --claim` the same task (double dispatch).
//!
//! Contract pinned here (real binary, `HOME` pinned to a temp dir — the real
//! `~/.backlog` is never touched):
//!   * the claimer's session id (`CLAUDE_CODE_SESSION_ID`) is recorded on the
//!     lease, and `backlog driver heartbeat --session-id S --project P` — the
//!     call `/flow` already makes every loop cycle — refreshes the leases held
//!     by S, so a lease past `CLAIM_STALE_SECS` whose holder heartbeated within
//!     the window still excludes;
//!   * a lease with no refresh (or refreshed only by ANOTHER session, or whose
//!     last refresh itself is older than the window) is reclaimable, so a
//!     crashed claimer cannot strand its task forever (anti-vacuity controls);
//!   * an older binary's entry (no session id / heartbeat fields) still parses
//!     and ages by `claimed_at` exactly as before;
//!   * an unreadable liveness field is never read as "dead": the claim is
//!     refused rather than the task re-handed out, and a heartbeat that cannot
//!     refresh the leases says so with a non-zero exit.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Mirror of `store::CLAIM_STALE_SECS`.
const CLAIM_STALE_SECS: i64 = 3600;
const TITLE: &str = "Long running task 486e83ca";

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn unique_root(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "backlog-486e83ca-{}-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the real binary. `session` is the `CLAUDE_CODE_SESSION_ID` the process
/// sees; `None` REMOVES it (the test runner itself may run inside a session).
fn run(args: &[&str], cwd: &Path, home: &Path, session: Option<&str>) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backlog"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.env("PATH", common::path_with_condukt_shim())
        .args(args)
        .env("HOME", home)
        .env_remove("BACKLOG_DISABLE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(s) = session {
        cmd.env("CLAUDE_CODE_SESSION_ID", s);
    }
    let out = cmd.output().expect("binary runs");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn git(args: &[&str], cwd: &Path, home: &Path) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("git is available");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fixture {
    home: PathBuf,
    /// A linked worktree (store writes are refused in a primary checkout).
    a: PathBuf,
}

impl Fixture {
    fn project(&self) -> &str {
        self.a.to_str().unwrap()
    }

    fn ledger_path(&self) -> PathBuf {
        let dir = self.home.join(".backlog").join("claims");
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        assert_eq!(files.len(), 1, "exactly one project ledger: {files:?}");
        files[0].clone()
    }

    fn edit_ledger(&self, f: impl Fn(&mut serde_json::Map<String, serde_json::Value>)) {
        let p = self.ledger_path();
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let entries = v["entries"].as_array_mut().expect("ledger has entries");
        assert!(!entries.is_empty(), "fixture: ledger must hold a lease");
        for e in entries {
            f(e.as_object_mut().unwrap());
        }
        std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    /// Age every lease past the stale window (the holder has been working the
    /// task for more than an hour).
    fn age_claims(&self) {
        let old = now_unix() - CLAIM_STALE_SECS - 600;
        self.edit_ledger(|e| {
            e.insert("claimed_at".into(), serde_json::json!(old));
        });
    }

    fn claim(&self, session: Option<&str>) {
        let o = run(
            &["next", "--claim", "--project", self.project()],
            &self.a,
            &self.home,
            session,
        );
        assert_eq!(o.code, 0, "claim must succeed: {} {}", o.stdout, o.stderr);
        assert!(
            o.stdout.contains(TITLE),
            "claim must hand out the task: {}",
            o.stdout
        );
    }

    fn heartbeat(&self, session: &str) -> Out {
        run(
            &[
                "driver",
                "heartbeat",
                "--session-id",
                session,
                "--project",
                self.project(),
            ],
            &self.a,
            &self.home,
            Some(session),
        )
    }

    /// Plain `next` (pure read, still skips live leases).
    fn next(&self) -> Out {
        run(
            &["next", "--project", self.project()],
            &self.a,
            &self.home,
            None,
        )
    }

    /// A SECOND session asking to claim.
    fn claim_by_other(&self) -> Out {
        run(
            &["next", "--claim", "--project", self.project()],
            &self.a,
            &self.home,
            Some("other-session"),
        )
    }
}

fn fixture(tag: &str) -> Fixture {
    let root = unique_root(tag);
    let home = root.join("home");
    let primary = root.join("primary");
    let a = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&primary).unwrap();
    git(&["init", "-q", "-b", "main"], &primary, &home);
    git(
        &["commit", "-q", "--allow-empty", "-m", "init"],
        &primary,
        &home,
    );
    git(
        &["worktree", "add", "-q", "-b", "trunk", a.to_str().unwrap()],
        &primary,
        &home,
    );
    std::fs::create_dir_all(a.join("tests")).unwrap();
    std::fs::write(
        a.join("tests/repro.sh"),
        "#!/bin/bash\necho 'bug present'\nexit 1\n",
    )
    .unwrap();
    git(&["add", "--", "tests/repro.sh"], &a, &home);
    git(&["commit", "-q", "-m", "repro"], &a, &home);
    let o = run(
        &[
            "add",
            "--title",
            TITLE,
            "--project",
            a.to_str().unwrap(),
            "--priority",
            "p1",
            "--repro-test",
            "bash tests/repro.sh",
        ],
        &a,
        &home,
        None,
    );
    assert_eq!(o.code, 0, "fixture add must succeed: {}", o.stderr);
    Fixture { home, a }
}

fn assert_excluded(f: &Fixture, why: &str) {
    let n = f.next();
    assert_eq!(n.code, 0, "{why}: plain next must succeed: {}", n.stderr);
    assert!(
        !n.stdout.contains(TITLE),
        "{why}: plain next re-offered the leased task: {}",
        n.stdout
    );
    let c = f.claim_by_other();
    assert!(
        !c.stdout.contains(TITLE),
        "{why}: a second session was handed the leased task: {}",
        c.stdout
    );
}

fn assert_reclaimable(f: &Fixture, why: &str) {
    let n = f.next();
    assert_eq!(n.code, 0, "{why}: plain next must succeed: {}", n.stderr);
    assert!(
        n.stdout.contains(TITLE),
        "{why}: the dead lease still excluded its task: {} {}",
        n.stdout,
        n.stderr
    );
    let c = f.claim_by_other();
    assert_eq!(c.code, 0, "{why}: reclaim must succeed: {}", c.stderr);
    assert!(
        c.stdout.contains(TITLE),
        "{why}: a second session could not reclaim the dead lease: {}",
        c.stdout
    );
}

/// THE defect: a holder that heartbeats within the window keeps its lease past
/// CLAIM_STALE_SECS. Modelled as: claimed, heartbeated (as `/flow` does every
/// cycle), and the claim itself is now more than an hour old while the last
/// heartbeat is recent.
#[test]
fn live_holder_heartbeat_keeps_lease_past_stale_window() {
    let f = fixture("live");
    f.claim(Some("holder-session"));
    let hb = f.heartbeat("holder-session");
    assert_eq!(hb.code, 0, "heartbeat must succeed: {}", hb.stderr);
    f.age_claims();
    assert_excluded(&f, "holder heartbeated within the window");
}

/// A heartbeat does not REVIVE a lease that already expired: once it stopped
/// excluding, another session may have reclaimed the task.
#[test]
fn heartbeat_does_not_revive_an_expired_lease() {
    let f = fixture("revive");
    f.claim(Some("holder-session"));
    f.age_claims();
    let hb = f.heartbeat("holder-session");
    assert_eq!(hb.code, 0, "heartbeat must succeed: {}", hb.stderr);
    assert_reclaimable(&f, "the lease expired before the heartbeat");
}

/// Anti-vacuity: the same aged lease with NO refresh is reclaimable.
#[test]
fn unrefreshed_old_lease_is_reclaimable() {
    let f = fixture("dead");
    f.claim(Some("holder-session"));
    f.age_claims();
    assert_reclaimable(&f, "no heartbeat at all");
}

/// Anti-vacuity: another session's heartbeat does not keep this lease alive.
#[test]
fn another_sessions_heartbeat_does_not_refresh_the_lease() {
    let f = fixture("othershb");
    f.claim(Some("holder-session"));
    let hb = f.heartbeat("unrelated-session");
    assert_eq!(hb.code, 0, "heartbeat must succeed: {}", hb.stderr);
    f.age_claims();
    assert_reclaimable(&f, "only an unrelated session heartbeated");
}

/// A holder that heartbeated and THEN died ages out one window after its last
/// heartbeat: a crashed claimer cannot strand the task forever.
#[test]
fn holder_whose_last_heartbeat_aged_out_is_reclaimable() {
    let f = fixture("hbaged");
    f.claim(Some("holder-session"));
    let hb = f.heartbeat("holder-session");
    assert_eq!(hb.code, 0, "heartbeat must succeed: {}", hb.stderr);
    f.age_claims();
    assert_excluded(&f, "fresh heartbeat");
    let old = now_unix() - CLAIM_STALE_SECS - 600;
    f.edit_ledger(|e| {
        assert!(
            e.contains_key("heartbeat_at"),
            "the refreshed lease must record heartbeat_at: {e:?}"
        );
        e.insert("claimed_at".into(), serde_json::json!(old - 3600));
        e.insert("heartbeat_at".into(), serde_json::json!(old));
    });
    assert_reclaimable(&f, "last heartbeat older than the window");
}

/// A claim made with no session id cannot be refreshed by anyone and ages by
/// claimed_at as before.
#[test]
fn sessionless_claim_ages_by_claimed_at() {
    let f = fixture("nosession");
    f.claim(None);
    let hb = f.heartbeat("holder-session");
    assert_eq!(hb.code, 0, "heartbeat must succeed: {}", hb.stderr);
    f.age_claims();
    assert_reclaimable(&f, "a lease with no recorded holder session");
}

/// Backward compatibility: an older binary's entry (no session_id /
/// heartbeat_at) still parses; fresh it excludes, aged it is reclaimable.
#[test]
fn legacy_entry_without_liveness_fields_still_parses_and_ages() {
    let f = fixture("legacy");
    f.claim(Some("holder-session"));
    f.edit_ledger(|e| {
        e.remove("session_id");
        e.remove("heartbeat_at");
    });
    assert_excluded(&f, "fresh legacy lease");
    f.age_claims();
    assert_reclaimable(&f, "aged legacy lease");
}

/// Fail closed: an unreadable liveness field is never read as "dead". The
/// claim is REFUSED (non-zero, nothing handed out), not re-offered.
#[test]
fn unparseable_heartbeat_field_refuses_rather_than_reclaims() {
    let f = fixture("garbage");
    f.claim(Some("holder-session"));
    f.age_claims();
    f.edit_ledger(|e| {
        e.insert("heartbeat_at".into(), serde_json::json!("not-a-time"));
    });
    let c = f.claim_by_other();
    assert!(
        !c.stdout.contains(TITLE),
        "a lease with an unreadable heartbeat was handed out again: {}",
        c.stdout
    );
    assert_ne!(
        c.code, 0,
        "an unreadable ledger must refuse, not answer: {} {}",
        c.stdout, c.stderr
    );
}

/// A heartbeat that cannot refresh the leases must not report success: the
/// lease would silently lapse while the driver believes it is protected.
#[test]
fn heartbeat_reports_failure_when_ledger_unreadable() {
    let f = fixture("hbfail");
    f.claim(Some("holder-session"));
    std::fs::write(f.ledger_path(), "{ not json").unwrap();
    let hb = f.heartbeat("holder-session");
    assert_ne!(
        hb.code, 0,
        "heartbeat over an unparseable ledger reported success: {} {}",
        hb.stdout, hb.stderr
    );
    assert!(
        hb.stderr.contains("lease"),
        "the failure must name the claim leases: {}",
        hb.stderr
    );
}
