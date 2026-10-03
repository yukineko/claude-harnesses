#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 57ab8322: `gh_probe` maps a `gh` that SPAWNED but did not finish
//! within `GH_TIMEOUT` (20s) to `None`, and `github::decide_issue_create` labels
//! `None` as "gh CLI not found". The gh binary demonstrably exists (it ran and
//! hung), so the recorded reason is false.
//!
//! Fixture: a repo whose origin is github.com, real `git`, and a `gh` shim that
//! sleeps past GH_TIMEOUT. `backlog add` must still not claim gh is absent.
//! Takes ~20s (the production timeout is a const, not injectable).
//!
//! Written by an independent auditor, not an implementer.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
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

#[test]
#[ignore = "backlog 57ab8322: open defect, remove ignore when fixed"]
fn hung_gh_is_not_reported_as_gh_not_found() {
    let root = std::env::temp_dir().join(format!(
        "backlog-57ab8322-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (home, repo, bin) = (root.join("home"), root.join("repo"), root.join("bin"));
    for d in [&home, &repo, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    let repo = std::fs::canonicalize(&repo).unwrap();
    let git = real_git();
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    ] {
        assert!(
            Command::new(&git)
                .args(a)
                .current_dir(&repo)
                .env("HOME", &home)
                .status()
                .unwrap()
                .success(),
            "git {a:?} failed: fixture is void"
        );
    }
    std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, "#!/bin/sh\nsleep 40\nexit 0\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let proj = repo.to_str().unwrap().to_string();
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(["add", "--title", "hung gh probe", "--project", &proj])
        .env("HOME", &home)
        .env(
            "PATH",
            format!(
                "{}:{}:/bin:/usr/bin",
                bin.display(),
                common::condukt_shim_dir().display()
            ),
        )
        .current_dir(&repo)
        .stdin(Stdio::null())
        .output()
        .expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("GitHub issue NOT created"),
        "precondition: the hung gh must surface as a degraded mirror; out={stdout:?} err={stderr:?}"
    );
    assert!(
        !stderr.contains("gh CLI not found"),
        "gh exists (it spawned and hung past GH_TIMEOUT) but backlog recorded \
         'gh CLI not found': err={stderr:?}"
    );
}
