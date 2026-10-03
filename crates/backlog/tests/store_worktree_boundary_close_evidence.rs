//! Backlog 1e6f00ae, close-evidence writers: `confirm`, `ruling request`,
//! `ruling approve`, `ruling withdraw` write the state store, so from the
//! PRIMARY checkout they must be refused by the primary-tree guard (and leave
//! the store byte-identical); from a LINKED worktree they must get PAST the
//! guard (they may still fail on their own business rules). `ruling list` and
//! `audit-closures` are reads and stay allowed in the primary checkout.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REFUSAL: &str = "inside the primary working tree";

struct Fx {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    linked: PathBuf,
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

/// Committed seed (visible in both checkouts): one task per state the writers
/// act on. `unconf01` unconfirmed, `pend0001` pending, `rule0001` needs-ruling.
fn seed(project: &Path) -> String {
    format!(
        "{}{}{}",
        task("unconf01", "unconfirmed", project),
        task("pend0001", "pending", project),
        task("rule0001", "needs-ruling", project)
    )
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
        linked,
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

fn assert_refused_in_primary(args: &[&str]) {
    let fx = fixture();
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx, &fx.primary, args);
    assert_ne!(
        code, 0,
        "{args:?} from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        err.contains(REFUSAL),
        "{args:?}: stderr must name the primary-tree refusal; out={out:?} err={err:?}"
    );
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "{args:?}: refused write must leave the primary store byte-identical"
    );
    let st = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&fx.primary)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&st.stdout),
        "",
        "{args:?}: primary tree must stay clean"
    );
}

fn assert_gets_past_guard_in_linked(args: &[&str]) {
    let fx = fixture();
    let main_before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx, &fx.linked, args);
    assert!(
        !err.contains(REFUSAL) && !out.contains(REFUSAL),
        "{args:?} from a linked worktree must not hit the primary-tree guard; code={code} out={out:?} err={err:?}"
    );
    assert_eq!(
        main_before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "{args:?}: primary store must be untouched by a worktree run"
    );
}

const CONFIRM: &[&str] = &["confirm", "unconf01", "--repro-test", "cargo test -p x"];
const REQUEST: &[&str] = &[
    "ruling",
    "request",
    "pend0001",
    "--kind",
    "judgment",
    "--rationale",
    "a value call",
];
const APPROVE: &[&str] = &["ruling", "approve", "rule0001"];
const WITHDRAW: &[&str] = &["ruling", "withdraw", "rule0001"];

#[test]
fn confirm_from_primary_is_refused_and_store_unchanged() {
    assert_refused_in_primary(CONFIRM);
}
#[test]
fn ruling_request_from_primary_is_refused_and_store_unchanged() {
    assert_refused_in_primary(REQUEST);
}
#[test]
fn ruling_approve_from_primary_is_refused_and_store_unchanged() {
    assert_refused_in_primary(APPROVE);
}
#[test]
fn ruling_withdraw_from_primary_is_refused_and_store_unchanged() {
    assert_refused_in_primary(WITHDRAW);
}

#[test]
fn confirm_from_linked_worktree_gets_past_the_guard() {
    assert_gets_past_guard_in_linked(CONFIRM);
}
#[test]
fn ruling_request_from_linked_worktree_gets_past_the_guard() {
    assert_gets_past_guard_in_linked(REQUEST);
}
#[test]
fn ruling_approve_from_linked_worktree_gets_past_the_guard() {
    assert_gets_past_guard_in_linked(APPROVE);
}
#[test]
fn ruling_withdraw_from_linked_worktree_gets_past_the_guard() {
    assert_gets_past_guard_in_linked(WITHDRAW);
}

#[test]
fn ruling_list_and_audit_closures_work_from_primary_and_do_not_write() {
    let fx = fixture();
    let before = std::fs::read(store(&fx.primary)).unwrap();
    for args in [&["ruling", "list"][..], &["audit-closures"][..]] {
        let (code, out, err) = run(&fx, &fx.primary, args);
        assert_eq!(code, 0, "{args:?} is a read: out={out:?} err={err:?}");
        assert!(!err.contains(REFUSAL), "{args:?}: err={err:?}");
        assert_eq!(
            before,
            std::fs::read(store(&fx.primary)).unwrap(),
            "{args:?} must not modify the store"
        );
    }
}
