#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-21): `condukt worktree create --run <R> --topic t3
//! --branch <b>` run with cwd inside a LINKED worktree (made by plain
//! `git worktree add`, not itself listed in `$HOME/.harness/trust.toml`) of a
//! repository whose MAIN working tree IS listed, created the new worktree but
//! did NOT add it to the trust list. t1/t2, created from the main working
//! tree, were listed; t3, created from the linked worktree `run4-integ`, was
//! not. donegate then runs no `[[check]]` in t3 while looking installed.
//!
//! Intended rule, verbatim from scripts/session-worktree-init.py: "It also
//! lists the worktree in the workspace-trust list `~/.harness/trust.toml` ...
//! exactly when the main tree is itself listed — the rule condukt's
//! `worktree::create` applies."
//!
//! Asserted property: creating a worktree from a linked worktree of a repo
//! whose main tree is listed lists the new worktree.
//!
//! Controls: (1) same command from the main tree lists the new worktree
//! (passes today); (2) no escalation: for a repo whose main tree is NOT
//! listed, creating from its main tree or from one of its linked worktrees
//! lists nothing (passes today).
//!
//! DOES NOT PROVE: that the right fix is a main-tree lookup (e.g. via
//! git-common-dir) rather than something else; donegate/tdd behaviour inside
//! the new worktree; session-worktree-init.py's own behaviour.
//!
//! RED written by an independent test author, not the implementer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run_git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(repo: &Path) -> PathBuf {
    std::fs::create_dir_all(repo).unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(repo, &["config", "user.email", "t@t.t"]);
    run_git(repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-q", "-m", "init"]);
    repo.canonicalize().unwrap()
}

fn condukt(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("CLAUDE_CODE_SESSION_ID", "sess-linked-trust-20261121")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_WORKTREE_BASE")
        .env_remove("HARNESS_TRUST_ALL")
        .output()
        .expect("spawn condukt")
}

/// Run `worktree create` from `cwd`; return the canonical created path.
fn create(cwd: &Path, home: &Path, topic: &str, branch: &str) -> PathBuf {
    let out = condukt(
        cwd,
        home,
        &[
            "worktree", "create", "--run", "run-x", "--topic", topic, "--branch", branch,
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "apparatus: create failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let p = PathBuf::from(&printed);
    assert!(
        p.exists(),
        "apparatus: printed path does not exist: {printed:?}"
    );
    p.canonicalize().unwrap()
}

fn listed(home: &Path) -> Vec<PathBuf> {
    let f = home.join(".harness").join("trust.toml");
    let text = std::fs::read_to_string(&f).unwrap_or_default();
    let v: toml::Value = toml::from_str(&text).unwrap_or(toml::Value::Table(Default::default()));
    v.get("trusted")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str())
                .map(|s| PathBuf::from(s).canonicalize().unwrap_or(PathBuf::from(s)))
                .collect()
        })
        .unwrap_or_default()
}

fn list_only(home: &Path, repo: &Path) {
    let d = home.join(".harness");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("trust.toml"),
        format!("trusted = [{:?}]\n", repo.display().to_string()),
    )
    .unwrap();
}

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    linked: PathBuf,
}

fn fixture(list_main: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = init_repo(&root.join("repo"));
    let linked = root.join("elsewhere").join("linked");
    std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "linked",
        ],
    );
    let linked = linked.canonicalize().unwrap();
    if list_main {
        list_only(&home, &repo);
    }
    Fx {
        _tmp: tmp,
        home,
        repo,
        linked,
    }
}

#[test]
fn create_from_linked_worktree_of_listed_repo_lists_new_worktree() {
    let fx = fixture(true);
    assert_eq!(listed(&fx.home), vec![fx.repo.clone()], "apparatus: seed");
    let wt = create(&fx.linked, &fx.home, "t3", "b3");
    assert!(
        listed(&fx.home).contains(&wt),
        "new worktree {wt:?} created from linked worktree {:?} is not in trust.toml; listed={:?}",
        fx.linked,
        listed(&fx.home)
    );
}

#[test]
fn control_create_from_main_tree_of_listed_repo_lists_new_worktree() {
    let fx = fixture(true);
    let wt = create(&fx.repo, &fx.home, "t1", "b1");
    assert!(
        listed(&fx.home).contains(&wt),
        "listed={:?}",
        listed(&fx.home)
    );
}

#[test]
fn control_unlisted_repo_never_lists_new_worktree() {
    let fx = fixture(false);
    let a = create(&fx.repo, &fx.home, "t1", "b1");
    let b = create(&fx.linked, &fx.home, "t2", "b2");
    let l = listed(&fx.home);
    assert!(!l.contains(&a), "escalation from main tree: {l:?}");
    assert!(!l.contains(&b), "escalation from linked worktree: {l:?}");
}
