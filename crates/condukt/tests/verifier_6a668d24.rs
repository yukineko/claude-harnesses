#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier tests for backlog 6a668d24, driven through the real
//! `condukt state is-claimed` binary (the public path the incident went
//! through).
//!
//! A `/flow` (`--stateless`) task claim whose heartbeat never moved past
//! `claimed_at`, ~51 days old, and whose owning session has no transcript
//! anywhere under a readable `~/.claude/projects`, must stop being reported as
//! claimed. Every state where the absence could NOT be observed must stay
//! claimed (CLAUDE.md §3), and so must a claim with a fresh heartbeat.
//!
//! `HARNESS_PROGRESS_WINDOW_SECS=0` collapses the multi-sample window so the
//! reap path runs without a 90s wait; it still needs two samples.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HK: &str = "3358a647274b19d0";
const SID: &str = "dead-session-0001";
/// 1218.6h, the incident's age.
const AGE: i64 = 4_386_960;

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        restore_modes(&self.base);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn restore_modes(dir: &Path) {
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if std::fs::symlink_metadata(&p)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                restore_modes(&p);
            }
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?} failed: {o:?}");
}

fn fx(tag: &str) -> Fx {
    let base = std::env::temp_dir().join(format!("condukt-v6a668d24-{}-{tag}", std::process::id()));
    restore_modes(&base);
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    Fx { base, repo, home }
}

impl Fx {
    fn projects(&self) -> PathBuf {
        self.home.join(".claude").join("projects")
    }
    /// A readable store with another session's transcript in it (so the store
    /// is not trivially empty) and no transcript for `SID`.
    fn populated_store(&self) -> PathBuf {
        let slug = self.projects().join("-Users-someone-proj");
        std::fs::create_dir_all(&slug).unwrap();
        std::fs::write(slug.join("other-session.jsonl"), "{}\n").unwrap();
        slug
    }
    fn condukt(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "verifier-caller")
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .output()
            .expect("spawn condukt")
    }
    fn claim(&self, session: &str) {
        let o = self.condukt(&[
            "state",
            "claim-task",
            "--run",
            &format!("flow-{session}"),
            "--session",
            session,
            "--stateless",
            "--title",
            "incident",
            "--hashkey",
            HK,
        ]);
        assert!(o.status.success(), "precondition: claim-task: {o:?}");
    }
    fn claims_json(&self) -> PathBuf {
        find(&self.home, "claims.json").expect("claims.json written by claim-task")
    }
    /// Backdate the claim to the incident shape: claimed_at == heartbeat_at,
    /// AGE seconds ago (never heartbeated).
    fn backdate(&self) {
        let p = self.claims_json();
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let shown = v.to_string();
        let c = v["task_claims"][HK]
            .as_object_mut()
            .unwrap_or_else(|| panic!("task claim {HK} missing: {shown}"));
        assert_eq!(c["stateless"], serde_json::Value::Bool(true), "{shown}");
        c.insert("claimed_at".into(), (now - AGE).into());
        c.insert("heartbeat_at".into(), (now - AGE).into());
        std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }
    /// Run is-claimed `n` times (1s apart) and return every exit code.
    fn is_claimed_n(&self, n: usize) -> Vec<Option<i32>> {
        let mut codes = Vec::new();
        for i in 0..n {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(1100));
            }
            let o = self.condukt(&["state", "is-claimed", "--hashkey", HK]);
            codes.push(o.status.code());
        }
        codes
    }
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

fn chmod(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Every sample must report claimed (exit 0) — never 1 (free).
fn assert_stays_claimed(codes: &[Option<i32>], why: &str) {
    assert!(
        codes.iter().all(|c| *c == Some(0)),
        "{why}: the claim must stay claimed at every sample, got exit codes {codes:?}"
    );
}

/// THE INCIDENT. RED at base (claim kept forever: exit 0 at every sample),
/// GREEN at tip (reaped once two samples saw the absent transcript frozen).
#[test]
fn dead_stateless_claim_with_absent_transcript_is_not_claimed_anymore() {
    let f = fx("dead");
    f.populated_store();
    f.claim(SID);
    f.backdate();
    let codes = f.is_claimed_n(3);
    assert_eq!(
        codes[0],
        Some(0),
        "the first sample only anchors the window and must not reap ({codes:?})"
    );
    assert_eq!(
        codes.last().copied().flatten(),
        Some(1),
        "a dead /flow claim with a determinately absent transcript must stop being \
         reported as claimed ({codes:?})"
    );
}

/// Same, but the store is EMPTY (read fully, nothing in it). Still an
/// observed absence.
#[test]
fn dead_claim_with_empty_but_readable_store_is_reaped() {
    let f = fx("dead-empty");
    std::fs::create_dir_all(f.projects()).unwrap();
    f.claim(SID);
    f.backdate();
    let codes = f.is_claimed_n(3);
    assert_eq!(codes.last().copied().flatten(), Some(1), "{codes:?}");
}

#[test]
fn fresh_heartbeat_with_absent_transcript_stays_claimed() {
    let f = fx("fresh");
    f.populated_store();
    f.claim(SID);
    assert_stays_claimed(&f.is_claimed_n(3), "fresh heartbeat");
}

#[test]
fn missing_projects_store_stays_claimed() {
    let f = fx("nostore");
    std::fs::create_dir_all(f.home.join(".claude")).unwrap();
    f.claim(SID);
    f.backdate();
    assert_stays_claimed(&f.is_claimed_n(3), "no ~/.claude/projects");
}

#[test]
fn dangling_projects_symlink_stays_claimed() {
    let f = fx("dangling-store");
    std::fs::create_dir_all(f.home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(f.base.join("unmounted-volume"), f.projects()).unwrap();
    f.claim(SID);
    f.backdate();
    assert_stays_claimed(&f.is_claimed_n(3), "projects is a dangling symlink");
}

#[test]
fn unlistable_projects_store_stays_claimed() {
    let f = fx("unlistable");
    f.populated_store();
    f.claim(SID);
    f.backdate();
    let projects = f.projects();
    chmod(&projects, 0o000);
    assert!(
        std::fs::read_dir(&projects).is_err(),
        "fixture precondition: 0o000 must make the store unlistable for this user"
    );
    let codes = f.is_claimed_n(3);
    chmod(&projects, 0o755);
    assert_stays_claimed(&codes, "projects dir unlistable");
}

/// The slug dir is listable from the parent but not searchable, so stat of
/// `<slug>/<id>.jsonl` fails with EACCES — a lookup error other than NotFound.
#[test]
fn unsearchable_project_dir_stays_claimed() {
    let f = fx("unsearchable-slug");
    let slug = f.populated_store();
    f.claim(SID);
    f.backdate();
    chmod(&slug, 0o000);
    let probe = std::fs::metadata(slug.join(format!("{SID}.jsonl")));
    assert!(
        matches!(&probe, Err(e) if e.kind() != std::io::ErrorKind::NotFound),
        "fixture precondition: candidate stat must fail with a non-NotFound error ({probe:?})"
    );
    let codes = f.is_claimed_n(3);
    chmod(&slug, 0o755);
    assert_stays_claimed(&codes, "a project dir that cannot be searched");
}

/// The transcript IS there, but in an unsearchable dir: must not be read as
/// absent.
#[test]
fn present_but_unreachable_transcript_stays_claimed() {
    let f = fx("present-unreachable");
    let slug = f.populated_store();
    std::fs::write(slug.join(format!("{SID}.jsonl")), "{}\n").unwrap();
    f.claim(SID);
    f.backdate();
    chmod(&slug, 0o000);
    let codes = f.is_claimed_n(3);
    chmod(&slug, 0o755);
    assert_stays_claimed(&codes, "transcript present but unreachable");
}

/// A slug entry that is a symlink loop: stat fails with ELOOP (not NotFound).
#[test]
fn symlink_loop_project_entry_stays_claimed() {
    let f = fx("loop");
    f.populated_store();
    let a = f.projects().join("-loop-a");
    let b = f.projects().join("-loop-b");
    std::os::unix::fs::symlink(&b, &a).unwrap();
    std::os::unix::fs::symlink(&a, &b).unwrap();
    let probe = std::fs::metadata(&a);
    assert!(
        matches!(&probe, Err(e) if e.kind() != std::io::ErrorKind::NotFound),
        "fixture precondition: a symlink loop must stat with a non-NotFound error ({probe:?})"
    );
    f.claim(SID);
    f.backdate();
    assert_stays_claimed(&f.is_claimed_n(3), "a project entry that is a symlink loop");
}

/// Session ids that are not a single path component cannot name a transcript
/// file, so nothing about their absence was observed.
#[test]
fn session_id_with_path_separator_stays_claimed() {
    for (i, sid) in ["sub/dead-session", "..", "../escape"].iter().enumerate() {
        let f = fx(&format!("badsid-{i}"));
        f.populated_store();
        f.claim(sid);
        f.backdate();
        assert_stays_claimed(&f.is_claimed_n(3), &format!("session id {sid:?}"));
    }
}

/// The projects store is a symlink to a real, readable dir with no transcript:
/// that IS an observed absence (control for the dangling-symlink case).
#[test]
fn symlinked_readable_store_without_transcript_is_reaped() {
    let f = fx("symlinked-store");
    let real = f.base.join("real-projects").join("-Users-x");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(f.home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(f.base.join("real-projects"), f.projects()).unwrap();
    f.claim(SID);
    f.backdate();
    let codes = f.is_claimed_n(3);
    assert_eq!(codes.last().copied().flatten(), Some(1), "{codes:?}");
}
