#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Hit 3 (backlog main.rs:1643): gh_probe Undetermined => None.
//! Force: repo whose origin is github.com, PATH containing only `git` (no `gh`).
mod common;

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

fn git_path() -> PathBuf {
    for d in std::env::var("PATH").unwrap().split(':') {
        let p = PathBuf::from(d).join("git");
        if p.exists() {
            return p;
        }
    }
    panic!("no git");
}

struct Env {
    home: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
}

fn setup(tag: &str) -> Env {
    let t = std::env::temp_dir().join(format!("vn-gh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(git_path(), bin.join("git")).unwrap();
    let g = |a: &[&str]| {
        assert!(Command::new("git")
            .args(a)
            .current_dir(&repo)
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", &home)
            .status()
            .unwrap()
            .success());
    };
    common::linked_checkout(&repo);
    g(&["remote", "add", "origin", "https://github.com/o/r.git"]);
    // Close-evidence fixture: only a `pending` row (a REPRODUCED, committed
    // repro test) is live work that `sync` mirrors; an `unconfirmed` one is
    // not, which would leave sync with nothing to do. Commit a failing repro
    // script and expose `bash` (the allowlisted runner) on PATH — still no `gh`.
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("tests/repro_yes.sh"),
        "echo 'bug present'; exit 1\n",
    )
    .unwrap();
    g(&["add", "tests/repro_yes.sh"]);
    g(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t.t",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "repro",
    ]);
    symlink("/bin/bash", bin.join("bash")).unwrap();
    Env { home, bin, repo }
}

fn bl(e: &Env, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .current_dir(&e.repo)
        .env_clear()
        .env("HOME", &e.home)
        .env(
            "PATH",
            format!(
                "{}:{}",
                e.bin.display(),
                common::condukt_shim_dir().display()
            ),
        )
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn gh_absent_on_sync_apply_is_nonzero() {
    let e = setup("sync");
    let (c, o, er) = bl(
        &e,
        &[
            "add",
            "--title",
            "verify gh absent sync",
            "--project",
            ".",
            "--repro-test",
            "bash tests/repro_yes.sh",
        ],
    );
    eprintln!("ADD code={c} stdout={o:?} stderr={er:?}");
    let (c, o, er) = bl(&e, &["sync", "--apply"]);
    eprintln!("SYNC code={c} stdout={o:?} stderr={er:?}");
    assert_ne!(c, 0, "gh unavailable must not look like a completed sync");
    assert!(
        er.contains("FAILED") || er.contains("did not complete"),
        "{er:?}"
    );
}

#[test]
fn gh_absent_on_add_is_reported() {
    let e = setup("add");
    let (c, o, er) = bl(
        &e,
        &["add", "--title", "verify gh absent add", "--project", "."],
    );
    eprintln!("ADD code={c} stdout={o:?} stderr={er:?}");
    assert_eq!(c, 0);
    let all = format!("{o}{er}").to_lowercase();
    assert!(
        all.contains("gh")
            || all.contains("github")
            || all.contains("local-only")
            || all.contains("issue"),
        "add silently dropped the degraded GitHub mirror: stdout={o:?} stderr={er:?}"
    );
}
