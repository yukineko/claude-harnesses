//! Backlog 1e6f00ae, close-evidence: `audit-closures` is a read and stays
//! allowed in the primary checkout without writing the store. (The writers
//! this file used to pin from the primary checkout — `confirm` and `ruling
//! request|approve|withdraw` — were removed by the user rulings of
//! 2026-10-15; `done` / `cancel` are pinned in store_worktree_boundary.rs.)

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REFUSAL: &str = "inside the primary working tree";

struct Fx {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    _linked: PathBuf,
    home: PathBuf,
    shim: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn task(id: &str, status: &str, project: &Path) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"t {id}\"\nproject = \"{}\"\ntags = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\n",
        project.display()
    )
}

/// Committed seed: one pending task.
fn seed(project: &Path) -> String {
    task("pend0001", "pending", project)
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let primary = base.join("primary");
    let home = base.join("home");
    let shim = base.join("shim");
    for d in [&primary, &home, &shim] {
        std::fs::create_dir_all(d).unwrap();
    }
    let condukt = shim.join("condukt");
    std::fs::write(&condukt, "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&condukt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let gh = shim.join("gh");
    std::fs::write(&gh, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&primary, &["init", "-q", "-b", "main"]);
    git(
        &primary,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    std::fs::write(primary.join("README"), "x\n").unwrap();
    std::fs::create_dir_all(primary.join(".backlog")).unwrap();
    std::fs::write(primary.join(".backlog/tasks.toml"), seed(&primary)).unwrap();
    git(&primary, &["add", "-A"]);
    git(&primary, &["commit", "-q", "-m", "init"]);
    let linked = base.join("linked");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    Fx {
        _tmp: tmp,
        primary,
        _linked: linked,
        home,
        shim,
    }
}

fn run(fx: &Fx, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let path = format!(
        "{}:{}",
        fx.shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", &fx.home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("backlog runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn store(root: &Path) -> PathBuf {
    root.join(".backlog").join("tasks.toml")
}

#[test]
fn audit_closures_works_from_primary_and_does_not_write() {
    let fx = fixture();
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let args: &[&str] = &["audit-closures"];
    let (code, out, err) = run(&fx, &fx.primary, args);
    assert_eq!(code, 0, "{args:?} is a read: out={out:?} err={err:?}");
    assert!(!err.contains(REFUSAL), "{args:?}: err={err:?}");
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "{args:?} must not modify the store"
    );
}
