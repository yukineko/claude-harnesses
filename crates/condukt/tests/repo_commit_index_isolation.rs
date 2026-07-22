//! Two REAL concurrent `condukt` processes staging+committing in the SAME
//! primary working tree must not be able to interleave content in the one
//! shared git index.
//!
//! Background (backlog 15b682d2): condukt's single-worktree mode and the
//! small-task fast path implement work directly in the primary working tree.
//! Two sessions sharing one index/working tree is the ONE conflict git cannot
//! resolve by merging — branch isolation does not apply. That path used to be
//! performed by the `/condukt` skill's own shell (`git add <paths> && git
//! commit`), so there was NO in-process site holding
//! `lock::REPO_PRIMARY_LOCK_KEY`; it was serialized only by the coarse upstream
//! `/flow` backlog run.lock, which the move to fine-grained per-task claiming
//! removes.
//!
//! `condukt repo commit --path <p>... -m <msg>` is that in-process site: it
//! performs the whole read-modify-write (index-clean check → `git add` →
//! `git commit`) while holding the repo-scoped primary lock, and REFUSES
//! (non-zero, no commit) when the lock cannot genuinely be held.
//!
//! These tests drive real processes, so they observe the CROSS-PROCESS
//! behaviour — the in-process unit test in `lock.rs` only covers threads.
//! `CONDUKT_REPO_COMMIT_RACE_DELAY_MS` widens the (locked) window between the
//! `git add` and the `git commit` so the interleaving, if it were possible,
//! happens deterministically instead of only under lucky timing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static TRIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_condukt"))
}

fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(repo)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn git")
        .success();
    assert!(ok, "git {args:?} failed in {}", repo.display());
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn init_repo(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(repo, &["add", "base.txt"]);
    git(repo, &["commit", "-m", "init"]);
}

fn spawn_commit(
    repo: &Path,
    home: &Path,
    path: &str,
    msg: &str,
    delay_ms: u64,
) -> std::process::Child {
    Command::new(bin())
        .current_dir(repo)
        .env("HOME", home)
        .env("CONDUKT_REPO_COMMIT_RACE_DELAY_MS", delay_ms.to_string())
        .args(["repo", "commit", "--path", path, "-m", msg])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn condukt repo commit")
}

/// Files touched by the commit whose subject is exactly `subject`.
fn files_of_commit(repo: &Path, subject: &str) -> Vec<String> {
    let sha = git_out(repo, &["log", "--format=%H %s"])
        .lines()
        .find(|l| l.split_once(' ').map(|x| x.1) == Some(subject))
        .map(|l| l.split(' ').next().unwrap().to_string())
        .unwrap_or_else(|| panic!("no commit with subject {subject:?}"));
    git_out(
        repo,
        &["show", "--pretty=format:", "--name-only", sha.as_str()],
    )
    .lines()
    .filter(|l| !l.trim().is_empty())
    .map(|s| s.to_string())
    .collect()
}

fn tmp_dir(tag: &str) -> PathBuf {
    let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "condukt-repo-commit-{tag}-{}-{n}",
        std::process::id()
    ))
}

/// The core isolation proof. Two real processes each stage+commit their OWN
/// file in the SAME working tree at the same time, with the add→commit window
/// deliberately widened. Each resulting commit must contain EXACTLY its own
/// file: no commit may carry the peer's staged content, and both must succeed
/// (they serialize, they do not fight).
#[test]
fn concurrent_repo_commits_never_interleave_staged_content() {
    const TRIALS: u64 = 4;

    for trial in 0..TRIALS {
        let tmp = tmp_dir("race");
        let home = tmp.join("home");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        init_repo(&repo);

        std::fs::write(repo.join("a.txt"), "from A\n").unwrap();
        std::fs::write(repo.join("b.txt"), "from B\n").unwrap();

        // Back-to-back spawn: both processes are alive inside their add→commit
        // window at the same time.
        let a = spawn_commit(&repo, &home, "a.txt", "A", 400);
        let b = spawn_commit(&repo, &home, "b.txt", "B", 400);

        let out_a = a.wait_with_output().expect("wait A");
        let out_b = b.wait_with_output().expect("wait B");

        assert_eq!(
            out_a.status.code(),
            Some(0),
            "trial {trial}: A must commit cleanly under the repo-primary lock; stderr={}",
            String::from_utf8_lossy(&out_a.stderr)
        );
        assert_eq!(
            out_b.status.code(),
            Some(0),
            "trial {trial}: B must commit cleanly under the repo-primary lock; stderr={}",
            String::from_utf8_lossy(&out_b.stderr)
        );

        assert_eq!(
            files_of_commit(&repo, "A"),
            vec!["a.txt".to_string()],
            "trial {trial}: commit A must contain ONLY a.txt (no interleaved peer content)"
        );
        assert_eq!(
            files_of_commit(&repo, "B"),
            vec!["b.txt".to_string()],
            "trial {trial}: commit B must contain ONLY b.txt (no interleaved peer content)"
        );

        // Nothing left staged or uncommitted: the shared index is back to clean.
        assert_eq!(
            git_out(&repo, &["status", "--porcelain"]).trim(),
            "",
            "trial {trial}: the shared working tree/index must end clean"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}

/// Cannot-determine resolves to the RESTRICTIVE side: if the shared index
/// already carries staged content this command did not put there, it cannot
/// tell that content apart from a peer's mid-flight staging, so it must REFUSE
/// (non-zero, no new commit) rather than sweep it into this task's commit.
#[test]
fn foreign_staged_content_is_refused_not_swept_into_the_commit() {
    let tmp = tmp_dir("foreign");
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    init_repo(&repo);

    let before = git_out(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    // Someone else's content is already staged in the shared index.
    std::fs::write(repo.join("foreign.txt"), "not mine\n").unwrap();
    git(&repo, &["add", "foreign.txt"]);

    std::fs::write(repo.join("mine.txt"), "mine\n").unwrap();
    let out = spawn_commit(&repo, &home, "mine.txt", "MINE", 0)
        .wait_with_output()
        .expect("wait");

    assert_ne!(
        out.status.code(),
        Some(0),
        "pre-existing foreign staged content must be refused, not swept in; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        git_out(&repo, &["rev-parse", "HEAD"]).trim(),
        before,
        "a refused commit must not advance HEAD"
    );

    std::fs::remove_dir_all(&tmp).ok();
}

/// An empty `--path` set is a cannot-determine input (which files are this
/// task's?), so it must be refused rather than degrade into a whole-tree
/// `git add -A`-style commit.
#[test]
fn missing_paths_are_refused() {
    let tmp = tmp_dir("nopaths");
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    init_repo(&repo);
    let before = git_out(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    std::fs::write(repo.join("stray.txt"), "stray\n").unwrap();
    let out = Command::new(bin())
        .current_dir(&repo)
        .env("HOME", &home)
        .args(["repo", "commit", "-m", "no paths"])
        .output()
        .expect("spawn");

    assert_ne!(
        out.status.code(),
        Some(0),
        "a commit with no explicit paths must be refused"
    );
    assert_eq!(
        git_out(&repo, &["rev-parse", "HEAD"]).trim(),
        before,
        "a refused commit must not advance HEAD"
    );

    std::fs::remove_dir_all(&tmp).ok();
}
