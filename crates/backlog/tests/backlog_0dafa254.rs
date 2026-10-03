#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 0dafa254, `cancelled` half. (`claimed` is pinned by
//! `backlog_s03_audit::list_status_claimed_is_a_recognised_status`.)
//!
//! `store.rs` defines `STATUS_CANCELLED` and `sync` treats a cancelled row as
//! a real terminal status (it closes the mirror as "not planned"), yet
//! `task::STATUSES` — documented as all recognised status values — lacks it,
//! so a correct `list --status cancelled` warns "unknown status".
//!
//! Fixture: a store row hand-written with `status = "cancelled"` (the CLI
//! cannot write one, see d8d25af9). Property asserted: the filter shows the row
//! and does not call its status unknown.
//!
//! Written by an independent auditor, not an implementer.

mod common;

use std::process::{Command, Stdio};

#[test]
fn list_status_cancelled_is_a_recognised_status() {
    let root = std::env::temp_dir().join(format!(
        "backlog-0dafa254-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let repo = std::fs::canonicalize(&repo).unwrap();
    common::linked_checkout(&repo);
    // Close-evidence fixture: `add` lands `pending` only with a REPRODUCED,
    // committed repro test (otherwise `unconfirmed`); commit a failing repro
    // script so the row this test rewrites starts `pending` as before.
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("tests/repro_yes.sh"),
        "echo 'bug present'; exit 1\n",
    )
    .unwrap();
    for args in [
        vec!["add", "tests/repro_yes.sh"],
        vec![
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
        ],
    ] {
        assert!(Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
    }
    let run = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
            .env("PATH", common::path_with_condukt_shim())
            .args(args)
            .env("HOME", &home)
            .current_dir(&repo)
            .stdin(Stdio::null())
            .output()
            .expect("binary runs");
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    let (rc, out, err) = run(&[
        "add",
        "--title",
        "abandoned work",
        "--project",
        repo.to_str().unwrap(),
        "--repro-test",
        "bash tests/repro_yes.sh",
    ]);
    assert_eq!(rc, 0, "precondition: add; out={out} err={err}");
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .unwrap()
        .trim()
        .to_string();
    let store = repo.join(".backlog/tasks.toml");
    let body = std::fs::read_to_string(&store).unwrap();
    assert!(body.contains("status = \"pending\""), "store={body}");
    std::fs::write(
        &store,
        body.replace("status = \"pending\"", "status = \"cancelled\""),
    )
    .unwrap();

    let (_rc, out, err) = run(&["list", "--status", "cancelled"]);
    assert!(
        out.contains(&id),
        "precondition: list --status cancelled must show the cancelled row {id}; out={out:?} err={err:?}"
    );
    assert!(
        !err.to_lowercase().contains("unknown status"),
        "list --status cancelled warned 'unknown status' for a status store.rs itself defines: {err:?}"
    );
}
