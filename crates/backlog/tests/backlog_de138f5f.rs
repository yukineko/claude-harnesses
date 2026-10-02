#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog de138f5f: `git_remote_origin_url` maps EVERY failure of
//! `git config --get remote.origin.url` (crash, timeout, unreadable config) to
//! the empty string, and `github::decide_issue_close` then reports
//! "remote is not github.com" — a fact nobody observed: the remote was never
//! read.
//!
//! Fixture: a repo whose origin IS github.com; a task with a mirrored issue
//! (#7, created through a fake `gh`). Then `done` runs with a `git` shim that
//! fails ONLY the `remote.origin.url` read. The control (non-ignored) proves
//! the same fixture closes the issue when git answers, so the ignored test's
//! RED is the remote-read collapse and nothing else.
//!
//! Written by an independent auditor, not an implementer.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn real_git() -> PathBuf {
    for d in std::env::var("PATH").unwrap_or_default().split(':') {
        let p = PathBuf::from(d).join("git");
        if p.is_file() {
            return p;
        }
    }
    panic!("no git on PATH: fixture is void");
}

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-de138f5f-{}-{}-{}",
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

fn write_exe(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fx {
    home: PathBuf,
    repo: PathBuf,
    /// PATH dir: real git + a `gh` that always succeeds.
    ok_bin: PathBuf,
    /// PATH dir: a `git` that fails the remote read + the same `gh`.
    bad_bin: PathBuf,
}

fn fx(tag: &str) -> Fx {
    let root = unique(tag);
    let f = Fx {
        home: root.join("home"),
        repo: root.join("repo"),
        ok_bin: root.join("ok_bin"),
        bad_bin: root.join("bad_bin"),
    };
    for d in [&f.home, &f.repo, &f.ok_bin, &f.bad_bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    let git = real_git();
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    ] {
        let ok = Command::new(&git)
            .args(a)
            .current_dir(&f.repo)
            .env("HOME", &f.home)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {a:?} failed: fixture is void");
    }
    // `gh issue create` prints an issue URL; `gh issue close` succeeds.
    let gh = "#!/bin/sh\n\
              if [ \"$2\" = create ]; then echo https://github.com/o/r/issues/7; fi\n\
              exit 0\n";
    write_exe(&f.ok_bin.join("gh"), gh);
    write_exe(&f.bad_bin.join("gh"), gh);
    std::os::unix::fs::symlink(&git, f.ok_bin.join("git")).unwrap();
    write_exe(
        &f.bad_bin.join("git"),
        &format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
               if [ \"$a\" = remote.origin.url ]; then\n\
                 echo 'fatal: simulated config read failure' >&2; exit 128\n\
               fi\n\
             done\n\
             exec '{}' \"$@\"\n",
            git.display()
        ),
    );
    f
}

fn run(f: &Fx, args: &[&str], path: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", &f.home)
        .env("PATH", path)
        .current_dir(&f.repo)
        .stdin(Stdio::null())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Add a task whose issue is mirrored as #7.
fn add_mirrored(f: &Fx) -> String {
    let proj = f.repo.to_str().unwrap().to_string();
    let (rc, out, err) = run(
        f,
        &["add", "--title", "mirrored task", "--project", &proj],
        &f.ok_bin,
    );
    assert_eq!(rc, 0, "precondition: add must succeed; out={out} err={err}");
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .unwrap_or_else(|| panic!("no 'added:' line in {out:?}"))
        .trim()
        .to_string();
    let store = std::fs::read_to_string(f.repo.join(".backlog/tasks.toml")).unwrap();
    assert!(
        store.contains("issue_number = 7"),
        "precondition: the fake gh must have mirrored the task as #7; store={store}"
    );
    id
}

#[test]
fn control_done_closes_the_issue_when_the_remote_is_readable() {
    let f = fx("control");
    let id = add_mirrored(&f);
    let (rc, out, err) = run(&f, &["done", &id], &f.ok_bin);
    assert_eq!(rc, 0, "done failed: out={out} err={err}");
    assert!(
        out.contains("closed issue #7"),
        "control: a readable github remote must close #7; out={out:?} err={err:?}"
    );
}

#[test]
#[ignore = "backlog de138f5f: open defect, remove ignore when fixed"]
fn unreadable_remote_is_not_reported_as_a_non_github_remote() {
    let f = fx("unreadable");
    let id = add_mirrored(&f);
    let (_rc, out, err) = run(&f, &["done", &id], &f.bad_bin);
    let all = format!("{out}\n{err}");
    assert!(
        !all.contains("remote is not github.com"),
        "git could not read remote.origin.url (exit 128), yet backlog recorded the \
         unobserved fact 'remote is not github.com': out={out:?} err={err:?}"
    );
}
