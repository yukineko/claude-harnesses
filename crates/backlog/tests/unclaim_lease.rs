//! `backlog unclaim <ID>`: release the caller's own LIVE lease early.
//!
//! Written by an independent test author BEFORE the implementation exists
//! (CLAUDE.md §2(a)/(b)); the implementer must not edit these asserts to fit.
//!
//! Contract under test:
//!  * releases the LIVE lease on ID held by the calling session
//!    (`CLAUDE_CODE_SESSION_ID` == lease.session_id): exit 0, the tracked
//!    `.backlog/tasks.toml` bytes unchanged, the task is immediately
//!    claimable again via `next --claim` and no longer `claimed` in
//!    `list --json`;
//!  * REFUSES (non-zero exit, ledger file byte-identical) when
//!      (a) no live lease for the id (never claimed, or lease aged out),
//!      (b) the lease belongs to another session,
//!      (c) the lease has no session_id (holder undeterminable),
//!      (d) CLAUDE_CODE_SESSION_ID unset or empty in the unclaiming process,
//!      (e) ledger unreadable/unparseable or its lock not acquired;
//!  * every refusal says "NOT released" (case-sensitive) on stderr, and
//!    stdout does not claim success;
//!  * works from the PRIMARY checkout (ledger is outside the repo; must not
//!    hit the store-write main-tree guard).
//!
//! REQUIRED stderr keywords (case-insensitive substring), which the
//! implementer must match in the respective messages:
//!   (a) "no live lease"   (b) "another session"   (c) "no session"
//!   (d) "CLAUDE_CODE_SESSION_ID"   (e) "ledger"
//! plus "NOT released" (case-sensitive) in every refusal.
//!
//! Fixture mirrors `claim_lease_release.rs`: real binary, HOME pinned to a temp
//! dir, real git repo with a committed RED repro and committed tasks.toml.
//! `CLAUDE_CODE_SESSION_ID` is pinned explicitly on every invocation.
#![allow(dead_code)]

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Mirror of `store::CLAIM_STALE_SECS`.
const CLAIM_STALE_SECS: i64 = 3600;

const S1: &str = "sess-owner-1111";
const S2: &str = "sess-other-2222";

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn unique_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-unclaim-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

/// `session`: `Some(s)` pins CLAUDE_CODE_SESSION_ID=s; `None` removes it.
fn run_as(args: &[&str], cwd: &Path, home: &Path, session: Option<&str>) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backlog"));
    match session {
        Some(s) => cmd.env("CLAUDE_CODE_SESSION_ID", s),
        None => cmd.env_remove("CLAUDE_CODE_SESSION_ID"),
    };
    let mut child = harness_core::git_env::scrub(&mut cmd)
        .env("PATH", common::path_with_condukt_shim())
        .args(args)
        .env("HOME", home)
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut s) = child.stdin.take() {
        let _ = s.write_all(b"");
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git_cmd(args: &[&str], cwd: &Path, home: &Path) -> std::process::Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
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
        .expect("git is available")
}

fn git(args: &[&str], cwd: &Path, home: &Path) -> String {
    let out = git_cmd(args, cwd, home);
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Fixture {
    home: PathBuf,
    /// Linked worktree A (where adds / claims happen).
    a: PathBuf,
    /// Second linked worktree B, same committed tasks.
    b: PathBuf,
    /// The primary (main) checkout.
    primary: PathBuf,
}

impl Fixture {
    fn store_bytes(&self, checkout: &Path) -> Vec<u8> {
        std::fs::read(checkout.join(".backlog").join("tasks.toml")).expect("tasks.toml readable")
    }

    /// Every `*.json` directly under `$HOME/.backlog/claims/`.
    fn ledger_files(&self) -> Vec<PathBuf> {
        let dir = self.home.join(".backlog").join("claims");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => panic!("cannot read {}: {e}", dir.display()),
        };
        entries
            .map(|e| e.expect("claims dir entry readable").path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect()
    }

    fn ledger_path(&self) -> PathBuf {
        let files = self.ledger_files();
        assert_eq!(files.len(), 1, "exactly one ledger expected: {files:?}");
        files[0].clone()
    }
}

fn fixture(tag: &str) -> Fixture {
    let root = unique_root(tag);
    let home = root.join("home");
    let a = root.join("repo");
    let primary = root.join("primary");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&primary).unwrap();

    // Store writes are refused in a primary checkout, so adds run in a linked
    // worktree A of a hidden primary tree.
    git(&["init", "-q", "-b", "main"], &primary, &home);
    git(&["config", "user.name", "t"], &primary, &home);
    git(
        &["config", "user.email", "t@example.invalid"],
        &primary,
        &home,
    );
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

    let project = a.to_str().unwrap().to_string();
    for (title, prio) in [("First task", "p0"), ("Second task", "p1")] {
        let (code, _, err) = run_as(
            &[
                "add",
                "--title",
                title,
                "--project",
                &project,
                "--priority",
                prio,
                "--repro-test",
                "bash tests/repro.sh",
            ],
            &a,
            &home,
            None,
        );
        assert_eq!(code, 0, "fixture add must succeed: {err}");
    }
    git(&["add", ".backlog/tasks.toml"], &a, &home);
    git(&["commit", "-q", "-m", "seed backlog"], &a, &home);

    let b = root.join("wt");
    git(
        &["worktree", "add", "-q", "-b", "side", b.to_str().unwrap()],
        &a,
        &home,
    );
    Fixture {
        home,
        a,
        b,
        primary,
    }
}

fn json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON ({e}), got: {stdout}"))
}

fn sess(f: &Fixture, cwd: &Path, args: &[&str], s: Option<&str>) -> (i32, String, String) {
    run_as(args, cwd, &f.home, s)
}

/// Claim as session `s` in `cwd`; returns the claimed task id.
fn claim_as(f: &Fixture, cwd: &Path, s: Option<&str>) -> String {
    let (code, out, err) = sess(f, cwd, &["next", "--claim"], s);
    assert_eq!(code, 0, "claim must succeed: stdout={out} stderr={err}");
    json(&out)["id"]
        .as_str()
        .expect("claim JSON has id")
        .to_string()
}

fn list_json(f: &Fixture, cwd: &Path) -> Vec<serde_json::Value> {
    let (code, out, err) = sess(f, cwd, &["list", "--json"], None);
    assert_eq!(code, 0, "list --json must succeed: stderr={err}");
    json(&out).as_array().expect("array").clone()
}

/// Rewrite the single ledger entry's JSON via `edit`.
fn edit_ledger(f: &Fixture, edit: impl FnOnce(&mut serde_json::Value)) {
    let p = f.ledger_path();
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    edit(&mut v);
    std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
}

/// Assert a refusal: non-zero, ledger bytes identical, stderr carries the
/// keyword and "NOT released", stdout claims no success.
fn assert_refused(f: &Fixture, res: (i32, String, String), ledger_before: &[u8], keyword: &str) {
    let (code, out, err) = res;
    assert_ne!(code, 0, "must refuse: stdout={out} stderr={err}");
    assert_eq!(
        std::fs::read(f.ledger_path()).unwrap(),
        ledger_before,
        "ledger must be byte-identical after a refusal"
    );
    assert!(
        err.to_lowercase().contains(&keyword.to_lowercase()),
        "stderr must contain {keyword:?}: {err}"
    );
    assert!(
        err.contains("NOT released"),
        "stderr must say NOT released: {err}"
    );
    assert!(
        !out.to_lowercase().contains("released") || out.contains("NOT released"),
        "stdout must not claim success: {out}"
    );
}

#[test]
fn owner_unclaim_releases_lease_and_task_is_reclaimable() {
    let f = fixture("own");
    let id = claim_as(&f, &f.a, Some(S1));
    let store_before = f.store_bytes(&f.a);
    let (code, out, err) = sess(&f, &f.a, &["unclaim", &id], Some(S1));
    assert_eq!(
        code, 0,
        "owner unclaim must succeed: stdout={out} stderr={err}"
    );
    assert_eq!(
        f.store_bytes(&f.a),
        store_before,
        "tracked store must be unchanged"
    );
    let rows = list_json(&f, &f.a);
    let row = rows
        .iter()
        .find(|r| r["id"] == id.as_str())
        .expect("row listed");
    assert_ne!(row["status"], "claimed", "no longer claimed: {row}");
    assert_eq!(row["status"], "pending", "stays pending: {row}");
    let again = claim_as(&f, &f.b, Some(S2));
    assert_eq!(
        again, id,
        "released task (p0) is claimable again by another session"
    );
}

#[test]
fn owner_unclaim_works_from_primary_checkout() {
    let f = fixture("primary");
    let id = claim_as(&f, &f.a, Some(S1));
    assert!(f.primary.join(".git").is_dir(), "fixture: primary checkout");
    let (code, out, err) = sess(&f, &f.primary, &["unclaim", &id], Some(S1));
    assert_eq!(
        code, 0,
        "unclaim from the primary tree must not hit the store-write guard: stdout={out} stderr={err}"
    );
    let again = claim_as(&f, &f.b, Some(S2));
    assert_eq!(again, id, "task claimable after release from primary");
}

#[test]
fn refuses_when_never_claimed() {
    let f = fixture("never");
    // Create a ledger via a claim of the p0 task, then unclaim the OTHER id.
    let claimed = claim_as(&f, &f.a, Some(S1));
    let rows = list_json(&f, &f.a);
    let other = rows
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .find(|i| *i != claimed)
        .unwrap();
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &other], Some(S1));
    assert_refused(&f, res, &before, "no live lease");
}

#[test]
fn refuses_when_lease_is_stale() {
    let f = fixture("stale");
    let id = claim_as(&f, &f.a, Some(S1));
    let old = now_unix() - CLAIM_STALE_SECS - 600;
    edit_ledger(&f, |v| {
        v["entries"][0]["claimed_at"] = old.into();
        v["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("heartbeat_at");
    });
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &id], Some(S1));
    assert_refused(&f, res, &before, "no live lease");
}

#[test]
fn refuses_other_sessions_lease() {
    let f = fixture("other");
    let id = claim_as(&f, &f.a, Some(S1));
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &id], Some(S2));
    assert_refused(&f, res, &before, "another session");
}

#[test]
fn refuses_lease_without_session_id() {
    let f = fixture("nosess");
    let id = claim_as(&f, &f.a, Some(S1));
    edit_ledger(&f, |v| {
        v["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("session_id");
    });
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &id], Some(S1));
    assert_refused(&f, res, &before, "no session");
}

#[test]
fn refuses_when_caller_session_unset() {
    let f = fixture("unset");
    let id = claim_as(&f, &f.a, Some(S1));
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &id], None);
    assert_refused(&f, res, &before, "CLAUDE_CODE_SESSION_ID");
}

#[test]
fn refuses_when_caller_session_empty() {
    let f = fixture("empty");
    let id = claim_as(&f, &f.a, Some(S1));
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &id], Some(""));
    assert_refused(&f, res, &before, "CLAUDE_CODE_SESSION_ID");
}

#[test]
fn refuses_when_ledger_unparseable() {
    let f = fixture("garbage");
    let id = claim_as(&f, &f.a, Some(S1));
    std::fs::write(f.ledger_path(), b"{ this is not json").unwrap();
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &id], Some(S1));
    assert_refused(&f, res, &before, "ledger");
}

#[test]
fn refuses_when_ledger_lock_held() {
    let f = fixture("locked");
    let id = claim_as(&f, &f.a, Some(S1));
    let lp = f.ledger_path().with_extension("lock");
    std::fs::write(&lp, b"held-by-test").unwrap(); // fresh mtime => not stale
    let before = std::fs::read(f.ledger_path()).unwrap();
    let res = sess(&f, &f.a, &["unclaim", &id], Some(S1));
    let _ = std::fs::remove_file(&lp);
    assert_refused(&f, res, &before, "ledger");
}
