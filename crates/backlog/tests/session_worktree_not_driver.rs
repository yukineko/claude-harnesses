//! backlog 491f6e94: a session-worktree registration is NOT a /flow driver.
//!
//! `scripts/session-worktree-init.py` registers the worktree it creates as a
//! `*.driver` record under `$HOME/.backlog/drivers/`. `backlog lock status`
//! with no `--project` scans every bucket there, and `daily` stands down while
//! it reports an active driver, so a live session-worktree registration must
//! not make it report one. These tests drive the real producer (the python
//! hook) and the real `backlog` binary in an isolated `$HOME`; they say
//! nothing about WHERE the record is stored, only what backlog reports.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-swnd-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Isolated HOME plus a main-checkout repo. Returns (home, main_repo).
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    (home, repo)
}

/// Run the real SessionStart producer from the MAIN checkout so it creates and
/// registers a session worktree. Returns the created worktree path.
fn register_session_worktree(home: &Path, repo: &Path, session_id: &str) -> PathBuf {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/session-worktree-init.py");
    assert!(script.is_file(), "producer missing: {}", script.display());
    let payload = format!(
        "{{\"session_id\":\"{session_id}\",\"cwd\":{}}}",
        serde_json::to_string(repo.to_str().unwrap()).unwrap()
    );
    let mut child = Command::new("python3")
        .arg(&script)
        .env("HOME", home)
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("python3 spawns");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !stdout.contains("could not register"),
        "producer failed to register: {stdout}"
    );
    let short = session_id.split('-').next().unwrap();
    let wt = repo
        .parent()
        .unwrap()
        .join(".repo-worktrees")
        .join(format!("session-{short}"));
    assert!(
        wt.is_dir(),
        "producer did not create {}: {stdout}",
        wt.display()
    );
    // Prove the registration really landed, so a RED below cannot be a false
    // RED caused by "nothing was written".
    assert!(driver_files(home) >= 1, "registration did not land");
    wt
}

/// Count `*.driver` files anywhere under HOME/.backlog/drivers.
fn driver_files(home: &Path) -> usize {
    fn walk(p: &Path, n: &mut usize) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, n);
                } else if path.extension().is_some_and(|x| x == "driver") {
                    *n += 1;
                }
            }
        }
    }
    let mut n = 0;
    walk(&home.join(".backlog").join("drivers"), &mut n);
    n
}

fn backlog(home: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .env("PATH", common::path_with_condukt_shim())
        .args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("backlog runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// RED at 88137602: the registration is counted as a driver.
#[test]
fn live_session_worktree_registration_is_not_an_active_driver() {
    let (home, repo) = fixture("noproj");
    register_session_worktree(&home, &repo, "aaaa1111-0000-0000-0000-000000000001");

    let (code, out, err) = backlog(&home, &repo, &["lock", "status"]);
    assert_eq!(code, 0, "lock status failed: {err}");
    assert_eq!(
        out, "none",
        "a session-worktree registration must not make `backlog lock status` report an active \
         driver (daily would stand down forever); got: {out}"
    );
}

/// Same, seen through `driver status`: no live drivers from a
/// session-worktree record.
#[test]
fn driver_status_counts_no_live_driver_for_session_worktree_record() {
    let (home, repo) = fixture("drvstatus");
    register_session_worktree(&home, &repo, "bbbb2222-0000-0000-0000-000000000002");

    let (code, out, err) = backlog(&home, &repo, &["driver", "status"]);
    assert_eq!(code, 0, "driver status failed: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("driver status is JSON");
    assert_eq!(
        v["undetermined"], false,
        "registry must stay readable: {out}"
    );
    assert_eq!(
        v["count"], 0,
        "session-worktree record counted as a driver: {out}"
    );
    assert_eq!(v["active"], false, "{out}");
}

/// Control: a real `driver register` still makes the no-project scan active,
/// even with a session-worktree record present.
#[test]
fn real_driver_still_makes_lock_status_active() {
    let (home, repo) = fixture("control");
    register_session_worktree(&home, &repo, "cccc3333-0000-0000-0000-000000000003");

    let proj = repo.to_str().unwrap();
    let (code, _, err) = backlog(
        &home,
        &repo,
        &[
            "driver",
            "register",
            "--session-id",
            "flow-real",
            "--project",
            proj,
        ],
    );
    assert_eq!(code, 0, "driver register failed: {err}");

    let (code, out, err) = backlog(&home, &repo, &["lock", "status"]);
    assert_eq!(code, 0, "lock status failed: {err}");
    assert_ne!(out, "none", "a real driver must be reported; got none");
    assert!(
        out.contains("flow-real"),
        "the real driver must be the one reported, not the worktree record: {out}"
    );
}

/// Control: `lock status --project <main repo>` is unaffected by the
/// session-worktree record (none), and still sees a real driver of that repo.
#[test]
fn project_scoped_status_ignores_session_worktree_record_but_sees_real_driver() {
    let (home, repo) = fixture("scoped");
    register_session_worktree(&home, &repo, "dddd4444-0000-0000-0000-000000000004");
    let proj = repo.to_str().unwrap();

    let (code, out, err) = backlog(&home, &repo, &["lock", "status", "--project", proj]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out, "none",
        "session-worktree record leaked into the repo's bucket: {out}"
    );

    let (code, _, err) = backlog(
        &home,
        &repo,
        &[
            "driver",
            "register",
            "--session-id",
            "flow-real",
            "--project",
            proj,
        ],
    );
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = backlog(&home, &repo, &["lock", "status", "--project", proj]);
    assert_ne!(out, "none", "real driver of the project must be reported");
}
