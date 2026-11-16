use super::*;
use std::process::Command;

const CHILD: &str = "GITENV_SITE_CHILD";

fn fx(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    let out = cmd
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn fixture git");
    assert!(
        out.status.success(),
        "fixture git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_child() -> bool {
    std::env::var(CHILD).as_deref() == Ok("1")
}

fn scratch_root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "condukt-gitenv-site-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

/// A repo on `branch` whose HEAD commit has `.backlog/x.txt` = `content`.
fn init_repo(dir: &Path, branch: &str, content: &str) {
    std::fs::create_dir_all(dir.join(".backlog")).unwrap();
    fx(dir, &["init", "-q", "-b", branch]);
    fx(dir, &["config", "user.email", "t@example.com"]);
    fx(dir, &["config", "user.name", "T"]);
    std::fs::write(dir.join(".backlog").join("x.txt"), content).unwrap();
    fx(dir, &["add", "."]);
    fx(dir, &["commit", "-q", "-m", "init"]);
}

/// Re-exec `child_test` with `GIT_DIR=<decoy>/.git` inherited plus `envs`,
/// and fail unless the child ran and passed.
fn run_child(root: &Path, child_test: &str, decoy: &Path, envs: &[(&str, &Path)]) {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let marker = format!("GITENV_SITE_CHILD_OK {child_test}");
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    // Drop the outer run's GIT_* so the decoy GIT_DIR below is the only
    // inherited git variable the child sees.
    harness_core::git_env::scrub(&mut cmd);
    cmd.args([
        "--exact",
        &format!("worktree::inherited_git_env_site_tests::{child_test}"),
        "--test-threads=1",
        "--nocapture",
    ])
    .env(CHILD, "1")
    .env("HOME", &home)
    .env("GIT_DIR", decoy.join(".git"));
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("re-exec test binary");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let log = format!(
        "child status={:?}\nstdout={stdout}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(root);
    assert!(
        out.status.success() && stdout.contains(&marker),
        "child {child_test} did not pass under an inherited GIT_DIR\n{log}"
    );
}

fn env_path(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("{k} unset")))
}

// ---- site: run_git_bounded_with (backs git()/git_env()/run_git_bounded)

#[test]
fn child_git_reads_target_repo_branch() {
    if !is_child() {
        return;
    }
    let repo_a = env_path("GITENV_REPO_A");
    let got = git(&repo_a, &["rev-parse", "--abbrev-ref", "HEAD"]).expect("git on repo A");
    assert_eq!(
        got.trim(),
        "abranch",
        "git() answered about the inherited GIT_DIR repo, not repo A"
    );
    println!("GITENV_SITE_CHILD_OK child_git_reads_target_repo_branch");
}

/// `git()` (run_git_bounded_with) in repo A reports A's branch even with
/// `GIT_DIR` pointing at repo B (branch `bbranch`).
#[test]
fn git_helper_ignores_inherited_git_dir() {
    if is_child() {
        return;
    }
    let root = scratch_root("git");
    let (a, b) = (root.join("a"), root.join("b"));
    init_repo(&a, "abranch", "A-base\n");
    init_repo(&b, "bbranch", "B-base\n");
    run_child(
        &root,
        "child_git_reads_target_repo_branch",
        &b,
        &[("GITENV_REPO_A", &a)],
    );
}

// ---- site: capture_pending_store_diff's `run` closure

#[test]
fn child_capture_store_diff_is_against_target_head() {
    if !is_child() {
        return;
    }
    let repo_a = env_path("GITENV_REPO_A");
    let pending = capture_pending_store_diff(&repo_a, "feat", "abranch")
        .expect("capture on repo A")
        .expect("repo A's feat worktree has a dirty .backlog");
    let patch = std::fs::read_to_string(pending.patch.path()).unwrap();
    assert!(
        patch.contains("-A-base") && patch.contains("+A-new") && !patch.contains("B-base"),
        "store patch was not taken against repo A's HEAD:\n{patch}"
    );
    println!("GITENV_SITE_CHILD_OK child_capture_store_diff_is_against_target_head");
}

/// The store patch is diffed against repo A's HEAD tree (`A-base`), not the
/// inherited GIT_DIR repo's (`B-base`).
#[test]
fn capture_pending_store_diff_ignores_inherited_git_dir() {
    if is_child() {
        return;
    }
    let root = scratch_root("store");
    let (a, b) = (root.join("a"), root.join("b"));
    init_repo(&a, "abranch", "A-base\n");
    init_repo(&b, "bbranch", "B-base\n");
    let wt = root.join("feat-wt");
    fx(
        &a,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
    std::fs::write(wt.join(".backlog").join("x.txt"), "A-new\n").unwrap();
    run_child(
        &root,
        "child_capture_store_diff_is_against_target_head",
        &b,
        &[("GITENV_REPO_A", &a)],
    );
}
