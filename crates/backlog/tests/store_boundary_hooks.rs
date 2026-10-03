//! Backlog 1e6f00ae, hook side: the SessionStart requeue WRITES the store, so
//! in the primary working tree it is skipped VISIBLY (stderr + additionalContext),
//! never silently, and never breaks the session (exit 0). In a linked worktree
//! the requeue still runs. (Written by the implementer: it pins the behaviour
//! the oracle file does not cover and has not been independently reviewed.)

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const EXPIRED: &str = "[[task]]\nid = \"exp00001\"\ntitle = \"expired deferral\"\nproject = \"p\"\nstatus = \"failed\"\ntags = []\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\ndefer_until = 1000\n";

fn session_start(cwd: &Path, home: &Path) -> (i32, String, String) {
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"s","cwd":"{}"}}"#,
        cwd.display()
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .arg("session-start")
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let o = c.wait_with_output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn setup(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("backlog-hookbound-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let linked = root.join("linked");
    common::linked_checkout(&linked);
    let primary = root.join("linked.main");
    (home, primary, linked)
}

#[test]
fn session_start_in_primary_skips_the_requeue_visibly_and_does_not_write() {
    let (home, primary, _linked) = setup("p");
    std::fs::create_dir_all(primary.join(".backlog")).unwrap();
    let store = primary.join(".backlog/tasks.toml");
    std::fs::write(&store, EXPIRED).unwrap();
    let (code, out, err) = session_start(&primary, &home);
    assert_eq!(code, 0, "the hook must not break the session: {err}");
    assert_eq!(
        std::fs::read_to_string(&store).unwrap(),
        EXPIRED,
        "nothing written"
    );
    assert!(
        out.contains("requeue SKIPPED"),
        "additionalContext must say so: {out:?}"
    );
    assert!(
        err.contains("requeue SKIPPED"),
        "stderr must say so: {err:?}"
    );
}

#[test]
fn session_start_in_linked_worktree_still_requeues() {
    let (home, _primary, linked) = setup("l");
    std::fs::create_dir_all(linked.join(".backlog")).unwrap();
    let store = linked.join(".backlog/tasks.toml");
    std::fs::write(&store, EXPIRED).unwrap();
    let (code, out, err) = session_start(&linked, &home);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("requeue SKIPPED"), "{out:?}");
    assert_ne!(
        std::fs::read_to_string(&store).unwrap(),
        EXPIRED,
        "requeue must rewrite the expired deferral"
    );
}
