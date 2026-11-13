#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `backlog add` must land the ledger row on `main` (by MERGE, pushed to
//! origin) BEFORE it files the GitHub issue (user ruling: "backlog addの時点で
//! 台帳も更新される。mainにいく、issueができる").
//!
//! Order pinned: (1) row written, (2) row on the primary tree's `main` and on
//! origin's `main`, (3) only then `gh issue create`. If (2) cannot be
//! confirmed, no issue is created and `add` exits non-zero (fail-closed).
//!
//! Hermetic: `remote.origin.url` reads as GitHub, `remote.origin.pushurl`
//! points at a local bare repo, `gh` is a stub that records argv plus what the
//! ledger looked like on main / origin at the instant `issue create` ran.
mod common;

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

fn tool_path(name: &str) -> PathBuf {
    for d in std::env::var("PATH").unwrap().split(':') {
        let p = PathBuf::from(d).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("no {name} on PATH");
}

struct Env {
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    primary: PathBuf,
    ghlog: PathBuf,
    title: String,
}

fn git_in(cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let (ok, out, err) = git_in(cwd, args);
    assert!(ok, "git {args:?} in {} failed: {err}", cwd.display());
    out
}

fn setup(tag: &str, pushurl_ok: bool) -> Env {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("bl-addmain-{tag}-{}-{nanos}", std::process::id()));
    let (home, bin, repo, bare, ghlog) = (
        root.join("home"),
        root.join("bin"),
        root.join("repo"),
        root.join("origin.git"),
        root.join("gh.log"),
    );
    for d in [&home, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    common::linked_checkout(&repo);
    let primary = root.join("repo.main");
    // The linked worktree is detached; give it a real, non-main branch.
    git_ok(&repo, &["checkout", "-q", "-b", "work/add-test"]);
    common::commit_repro(&repo);

    // bare origin, seeded with main
    std::fs::create_dir_all(&bare).unwrap();
    git_ok(&bare, &["init", "-q", "--bare", "-b", "main"]);
    git_ok(&primary, &["push", "-q", bare.to_str().unwrap(), "main"]);
    git_ok(
        &primary,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    let pushurl = if pushurl_ok {
        bare.display().to_string()
    } else {
        root.join("does-not-exist.git").display().to_string()
    };
    git_ok(&primary, &["config", "remote.origin.pushurl", &pushurl]);

    for t in ["git", "grep", "cat", "sh"] {
        symlink(tool_path(t), bin.join(t)).unwrap();
    }
    symlink("/bin/bash", bin.join("bash")).unwrap();
    let exec = |name: &str, body: &str| {
        std::fs::write(bin.join(name), body).unwrap();
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    exec("condukt", "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n");
    let _ = Command::new(bin.join("condukt"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let title = format!("addmain-{tag}-{nanos}");
    let stub = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = issue ] && [ \"$2\" = create ]; then\n\
         \u{20} if git -C {primary} show main:.backlog/tasks.toml 2>/dev/null | grep -q '{title}'; then echo MAIN_HAS_ROW=yes >> {log}; else echo MAIN_HAS_ROW=no >> {log}; fi\n\
         \u{20} if git --git-dir {bare} show main:.backlog/tasks.toml 2>/dev/null | grep -q '{title}'; then echo ORIGIN_HAS_ROW=yes >> {log}; else echo ORIGIN_HAS_ROW=no >> {log}; fi\n\
         \u{20} echo https://github.com/o/r/issues/701\n\
         fi\nexit 0\n",
        log = ghlog.display(),
        primary = primary.display(),
        bare = bare.display(),
        title = title,
    );
    exec("gh", &stub);

    Env {
        root,
        home,
        bin,
        repo,
        primary,
        ghlog,
        title,
    }
}

fn run_add(e: &Env) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args([
            "add",
            "--title",
            &e.title,
            "--project",
            ".",
            "--repro-test",
            common::COMMITTED_REPRO,
        ])
        .current_dir(&e.repo)
        .env_clear()
        .env("HOME", &e.home)
        .env("PATH", &e.bin)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn ghlog(e: &Env) -> String {
    std::fs::read_to_string(&e.ghlog).unwrap_or_default()
}

fn cleanup(e: &Env) {
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
#[ignore = "RED: backlog add creates the issue before the row is on main (fix pending)"]
fn issue_is_created_only_after_the_row_is_on_main_and_pushed() {
    let e = setup("order", true);
    let (code, stdout, stderr) = run_add(&e);
    assert_eq!(
        code, 0,
        "add must succeed\nstdout: {stdout}\nstderr: {stderr}"
    );
    let id = common::parse_added(&stdout);

    let log = ghlog(&e);
    assert_eq!(
        log.matches("issue create").count(),
        1,
        "exactly one issue create expected; gh.log:\n{log}"
    );
    assert!(
        log.contains("MAIN_HAS_ROW=yes"),
        "row must be on main BEFORE gh issue create; gh.log:\n{log}"
    );
    assert!(
        log.contains("ORIGIN_HAS_ROW=yes"),
        "row must be pushed to origin main BEFORE gh issue create; gh.log:\n{log}"
    );

    let main_ledger = git_ok(&e.primary, &["show", "main:.backlog/tasks.toml"]);
    assert!(main_ledger.contains(&id), "main ledger lacks {id}");
    assert!(
        main_ledger.contains("issue_number = 701"),
        "row on main must carry the issue number (else sync files a duplicate):\n{main_ledger}"
    );

    let parents = git_ok(&e.primary, &["rev-list", "--parents", "-n1", "main"]);
    assert_eq!(
        parents.split_whitespace().count(),
        3,
        "main's tip must be a merge commit (§8); got: {parents}"
    );

    let status = git_ok(&e.primary, &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "primary tree must be clean:\n{status}"
    );
    assert!(
        !e.primary.join(".git/MERGE_HEAD").exists(),
        "no MERGE_HEAD may remain"
    );
    cleanup(&e);
}

#[test]
#[ignore = "RED: backlog add creates the issue before the row is on main (fix pending)"]
fn no_issue_when_integration_into_main_is_refused() {
    let e = setup("refused", true);
    // A foreign in-progress, conflicting merge in the primary tree.
    std::fs::write(e.primary.join("clash.txt"), "base\n").unwrap();
    git_ok(&e.primary, &["add", "clash.txt"]);
    git_ok(&e.primary, &["commit", "-q", "-m", "base clash"]);
    git_ok(&e.primary, &["branch", "other"]);
    std::fs::write(e.primary.join("clash.txt"), "main side\n").unwrap();
    git_ok(&e.primary, &["commit", "-q", "-am", "main side"]);
    git_ok(&e.primary, &["checkout", "-q", "other"]);
    std::fs::write(e.primary.join("clash.txt"), "other side\n").unwrap();
    git_ok(&e.primary, &["commit", "-q", "-am", "other side"]);
    git_ok(&e.primary, &["checkout", "-q", "main"]);
    let (merged_ok, _, _) = git_in(&e.primary, &["merge", "--no-ff", "other"]);
    assert!(!merged_ok, "fixture: the foreign merge must conflict");
    let merge_head = e.primary.join(".git/MERGE_HEAD");
    assert!(merge_head.exists(), "fixture: MERGE_HEAD must exist");
    let before = std::fs::read_to_string(&merge_head).unwrap();

    let (code, stdout, stderr) = run_add(&e);
    let log = ghlog(&e);
    assert_ne!(
        code, 0,
        "add must fail when main integration is refused\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !log.contains("issue create"),
        "no issue may be created when integration is refused; gh.log:\n{log}"
    );
    assert_eq!(
        std::fs::read_to_string(&merge_head).ok().as_deref(),
        Some(before.as_str()),
        "foreign MERGE_HEAD must be left untouched"
    );
    assert!(
        stderr.contains("main"),
        "stderr must say why (mention main): {stderr}"
    );
    cleanup(&e);
}

#[test]
#[ignore = "RED: backlog add creates the issue before the row is on main (fix pending)"]
fn no_issue_when_push_fails() {
    let e = setup("pushfail", false);
    let (code, stdout, stderr) = run_add(&e);
    let log = ghlog(&e);
    assert_ne!(
        code, 0,
        "add must fail when the push to origin fails\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !log.contains("issue create"),
        "no issue may be created when the push fails; gh.log:\n{log}"
    );
    cleanup(&e);
}
