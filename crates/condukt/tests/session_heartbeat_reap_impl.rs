// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! IMPLEMENTER-WRITTEN tests (backlog 491f6e94). Written by the same agent
//! that implemented `condukt session-heartbeat` and `condukt worktree reap`,
//! so they carry the author's blind spots (CLAUDE.md §2 (a)); the independent
//! RED tests for the reaper are section 9 of `worktree_reconcile.rs`.
//!
//! What these pin that the independent tests do not:
//! - the heartbeat refresh (nothing refreshed a registration before; without
//!   it a live session ages out and its worktree becomes reapable mid-work);
//! - its scope (own session / own cwd worktree, own bucket only) and rate limit;
//! - that a failed refresh is surfaced as a non-zero exit, not swallowed;
//! - untracked-only work counts as dirty even under
//!   `status.showUntrackedFiles=no`;
//! - reap prints every kept worktree with a reason.
//!
//! Real git, real built binary, temp `$HOME`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

const BUCKET: &str = "session-worktrees";

struct Fx {
    repo: PathBuf,
    wt_base: PathBuf,
    home: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("condukt-hbimpl-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let base = {
            std::fs::create_dir_all(&base).unwrap();
            base.canonicalize().unwrap()
        };
        let repo = base.join("repo");
        let wt_base = base.join("worktrees");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        run_git(&repo, &["add", "seed.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);
        Fx {
            repo,
            wt_base,
            home,
        }
    }

    /// A session-* worktree whose one commit is merged into main.
    fn merged_session_wt(&self, name: &str) -> PathBuf {
        let wt = self.wt_base.join(name);
        run_git(
            &self.repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", name],
        );
        std::fs::write(wt.join(format!("{name}.txt")), "w\n").unwrap();
        run_git(&wt, &["add", &format!("{name}.txt")]);
        run_git(&wt, &["commit", "-q", "-m", "work"]);
        run_git(&self.repo, &["merge", "-q", "--no-ff", "-m", "merge", name]);
        wt
    }

    fn record_path(&self, bucket: &str, file: &str) -> PathBuf {
        self.home
            .join(".backlog")
            .join("drivers")
            .join(bucket)
            .join(format!("{file}.driver"))
    }

    fn write_record(&self, bucket: &str, file: &str, sid: &str, project: &Path, hb: i64) {
        let p = self.record_path(bucket, file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let v = serde_json::json!({
            "session_id": sid,
            "pid": 1,
            "project": project.to_string_lossy(),
            "registered_at": hb,
            "heartbeat_at": hb,
        });
        std::fs::write(p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    fn heartbeat_of(&self, bucket: &str, file: &str) -> i64 {
        let raw = std::fs::read_to_string(self.record_path(bucket, file)).unwrap();
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["heartbeat_at"]
            .as_i64()
            .unwrap()
    }

    fn heartbeat(&self, sid: &str, cwd: &Path) -> Output {
        let mut child = Command::new(bin())
            .arg("session-heartbeat")
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                serde_json::json!({
                    "session_id": sid,
                    "cwd": cwd.to_string_lossy(),
                    "hook_event_name": "PostToolUse",
                    "tool_name": "Read",
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn reap(&self) -> Output {
        Command::new(bin())
            .args(["worktree", "reap"])
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .unwrap()
    }

    fn reap_settled(&self) -> Output {
        let _ = self.reap();
        self.reap()
    }
}

#[test]
fn heartbeat_refreshes_own_session_record_and_nothing_else() {
    let f = Fx::new("scope");
    let wt = f.merged_session_wt("session-own00001");
    let other = f.merged_session_wt("session-oth00002");
    let old = now() - 5000;
    // Own record, found by session id even though cwd is the main tree.
    f.write_record(BUCKET, "own", "sess-own", &wt, old);
    // Another session's record for another worktree: must not be touched.
    f.write_record(BUCKET, "other", "sess-other", &other, old);
    // A backlog project bucket record carrying OUR session id (a /flow driver
    // registration): must not be touched — autoflow/daily read those.
    f.write_record("0123456789abcdef", "flow", "sess-own", &f.repo, old);

    let out = f.heartbeat("sess-own", &f.repo);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.heartbeat_of(BUCKET, "own") >= now() - 5,
        "own record refreshed"
    );
    assert_eq!(
        f.heartbeat_of(BUCKET, "other"),
        old,
        "other session untouched"
    );
    assert_eq!(
        f.heartbeat_of("0123456789abcdef", "flow"),
        old,
        "backlog project bucket untouched"
    );
}

#[test]
fn heartbeat_refreshes_the_record_naming_the_cwd_worktree() {
    let f = Fx::new("cwd");
    let wt = f.merged_session_wt("session-cwd00001");
    let old = now() - 5000;
    f.write_record(BUCKET, "rec", "sess-someone-else", &wt, old);
    // cwd is a SUBDIRECTORY of the worktree: the git top level is what matches.
    let sub = wt.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = f.heartbeat("sess-me", &sub);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(f.heartbeat_of(BUCKET, "rec") >= now() - 5);
}

#[test]
fn heartbeat_is_rate_limited() {
    let f = Fx::new("rate");
    let wt = f.merged_session_wt("session-rate0001");
    let recent = now() - 10;
    f.write_record(BUCKET, "own", "sess-own", &wt, recent);
    let before = std::fs::read(f.record_path(BUCKET, "own")).unwrap();
    let out = f.heartbeat("sess-own", &wt);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read(f.record_path(BUCKET, "own")).unwrap(),
        before,
        "a record younger than 60s is not rewritten"
    );
}

#[cfg(unix)]
#[test]
fn heartbeat_write_failure_exits_nonzero_with_reason() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fx::new("fail");
    let wt = f.merged_session_wt("session-fail0001");
    f.write_record(BUCKET, "own", "sess-own", &wt, now() - 5000);
    let dir = f.record_path(BUCKET, "own").parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let out = f.heartbeat("sess-own", &wt);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success(), "a failed refresh must not exit 0");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("session heartbeat"),
        "stderr names the failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The property the heartbeat exists for: an aged-out registration that a
/// working session keeps heartbeating is NOT reaped. Control: the same shape
/// without the heartbeat IS reaped (so the keep is caused by the heartbeat).
#[test]
fn heartbeat_keeps_a_working_sessions_worktree_out_of_reap() {
    let f = Fx::new("keeps");
    let working = f.merged_session_wt("session-work0001");
    let idle = f.merged_session_wt("session-idle0002");
    let old = now() - 5000;
    f.write_record(BUCKET, "working", "sess-working", &working, old);
    f.write_record(BUCKET, "idle", "sess-idle", &idle, old);

    let _ = f.reap();
    let hb = f.heartbeat("sess-working", &working);
    assert!(
        hb.status.success(),
        "{}",
        String::from_utf8_lossy(&hb.stderr)
    );
    let out = f.reap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    assert!(working.is_dir(), "heartbeated worktree must be kept");
    assert!(!idle.exists(), "control: the idle one is reaped");
    // Its registration is cleaned up with it; the working one's stays.
    assert!(!f.record_path(BUCKET, "idle").exists());
    assert!(f.record_path(BUCKET, "working").exists());
}

#[test]
fn untracked_only_work_is_dirty_even_when_status_hides_untracked() {
    let f = Fx::new("untracked");
    let wt = f.merged_session_wt("session-untr0001");
    run_git(&f.repo, &["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(wt.join("new-untracked.txt"), "work\n").unwrap();
    f.write_record(BUCKET, "rec", "sess-x", &wt, 0);
    let _ = f.reap_settled();
    assert!(
        wt.join("new-untracked.txt").is_file(),
        "untracked work must survive"
    );
}

#[test]
fn reap_reports_every_worktree_with_a_reason() {
    let f = Fx::new("report");
    let gone = f.merged_session_wt("session-gone0001");
    let noreg = f.merged_session_wt("session-noreg002");
    f.write_record(BUCKET, "gone", "sess-gone", &gone, 0);
    let out = f.reap_settled();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains(&format!("removed {}", gone.display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("kept (undetermined) {}", noreg.display())),
        "an unregistered worktree is reported as undetermined: {text}"
    );
    assert!(text.contains("kept") && text.contains("primary"), "{text}");
    assert!(
        text.contains("reap: removed 1, kept 2 (1 undetermined), failed 0"),
        "{text}"
    );
}
