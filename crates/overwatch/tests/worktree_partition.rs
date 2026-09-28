// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Overwatch worktree partition (user ruling 2026-09-29: key the store on
//! `main_worktree_root`).
//!
//! `store::storage_root` keyed `~/.overwatch/<key>/overwatch/` on
//! `projkey::repo_root(cwd)`, which stops at the first ancestor holding a
//! `.git` ENTRY — for a linked git worktree that is the worktree itself. A
//! lease begun in a linked worktree was therefore invisible to `overwatch
//! status` run from the main checkout (and vice versa), which is exactly where
//! condukt's main-tree guard runs it. These tests drive the REAL binary.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("ow-wtpart-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    // macOS: /var -> /private/var; canonicalize so paths compare stably.
    d.canonicalize().unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git spawn");
    assert!(
        o.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A repo with one commit at `<root>/<name>`.
fn make_repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("f.txt"), "x").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

fn add_worktree(repo: &Path, path: &Path) {
    git(
        repo,
        &["worktree", "add", "-q", "-b", "wt", path.to_str().unwrap()],
    );
}

fn ow(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .current_dir(cwd)
        .env("HOME", home)
        .args(args)
        .output()
        .expect("spawn overwatch")
}

fn begin(home: &Path, cwd: &Path, key: &str) {
    let o = ow(
        home,
        cwd,
        &["begin", "--key", key, "--title", "t", "--session", "sess-x"],
    );
    assert!(
        o.status.success(),
        "begin failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Keys of the sessions roster in `status --json`. Panics if the output is not
/// JSON (a status that cannot be parsed is not "no leases").
fn status_keys(home: &Path, cwd: &Path) -> Vec<String> {
    let o = ow(home, cwd, &["status", "--json"]);
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("status --json not JSON ({e}): {out}"));
    // `sessions` is omitted from the JSON when the roster is empty.
    v.get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .flat_map(|s| s["leases"].as_array().cloned().unwrap_or_default())
        .filter_map(|l| l["key"].as_str().map(str::to_string))
        .collect()
}

fn status_text(home: &Path, cwd: &Path) -> String {
    String::from_utf8_lossy(&ow(home, cwd, &["status"]).stdout).to_string()
}

struct Fx {
    home: PathBuf,
    main: PathBuf,
    wt: PathBuf,
}

fn fx(tag: &str) -> Fx {
    let root = scratch(tag);
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let main = make_repo(&root, "main-co");
    let wt = root.join("linked-wt");
    add_worktree(&main, &wt);
    Fx { home, main, wt }
}

#[test]
fn lease_begun_in_linked_worktree_is_visible_from_main_checkout() {
    let f = fx("wt2main");
    begin(&f.home, &f.wt, "wk");
    let keys = status_keys(&f.home, &f.main);
    assert!(
        keys.contains(&"wk".to_string()),
        "lease begun in linked worktree must be listed from main; got {keys:?}"
    );
    let text = status_text(&f.home, &f.main);
    assert!(
        !text.contains("(none)"),
        "text status from main must not say (none) when a worktree lease exists:\n{text}"
    );
}

#[test]
fn lease_begun_in_main_checkout_is_visible_from_linked_worktree() {
    let f = fx("main2wt");
    begin(&f.home, &f.main, "wk");
    let keys = status_keys(&f.home, &f.wt);
    assert!(
        keys.contains(&"wk".to_string()),
        "lease begun in main must be listed from the linked worktree; got {keys:?}"
    );
}

#[test]
fn subdirectory_of_linked_worktree_resolves_to_same_store() {
    let f = fx("subdir");
    let sub = f.wt.join("a").join("b");
    fs::create_dir_all(&sub).unwrap();
    begin(&f.home, &sub, "wk");
    let keys = status_keys(&f.home, &f.main);
    assert!(
        keys.contains(&"wk".to_string()),
        "lease begun in a linked-worktree subdir must be listed from main; got {keys:?}"
    );
}

// ---- controls: must be green before AND after the fix ----

#[test]
fn control_unrelated_repos_do_not_see_each_others_leases() {
    let root = scratch("unrelated");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let a = make_repo(&root, "repo-a");
    let b = make_repo(&root, "repo-b");
    begin(&home, &a, "only-a");
    assert!(status_keys(&home, &a).contains(&"only-a".to_string()));
    let kb = status_keys(&home, &b);
    assert!(
        !kb.contains(&"only-a".to_string()),
        "unrelated repo must not see repo-a's lease; got {kb:?}"
    );
}

#[test]
fn control_same_checkout_begin_then_status() {
    let f = fx("same");
    begin(&f.home, &f.main, "wk");
    assert!(status_keys(&f.home, &f.main).contains(&"wk".to_string()));
    // and inside a linked worktree, begin+status in that same worktree
    begin(&f.home, &f.wt, "wk2");
    assert!(status_keys(&f.home, &f.wt).contains(&"wk2".to_string()));
}

#[test]
fn control_cwd_outside_any_git_repo_still_works() {
    let root = scratch("nogit");
    let home = root.join("home");
    let plain = root.join("plain");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&plain).unwrap();
    begin(&home, &plain, "ng");
    assert!(status_keys(&home, &plain).contains(&"ng".to_string()));
    // a different non-repo dir is a different project (keyed on cwd itself)
    let other = root.join("other");
    fs::create_dir_all(&other).unwrap();
    assert!(!status_keys(&home, &other).contains(&"ng".to_string()));
}
