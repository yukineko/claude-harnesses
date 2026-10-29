#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `crates/tdd/src/warned.rs` module doc: "The ledger is per project, not per
//! session". This pins that claim across git worktrees of ONE project: a
//! finding warned (loose mode) from a linked worktree must count as already
//! warned when the same file at the same content is judged from the main
//! checkout. If the ledger is keyed by the hook cwd's checkout instead of the
//! project, the main checkout blocks again and this test is RED.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const IMPL_BODY: &str = "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";

fn git(dir: &Path, args: &[&str]) {
    let s = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(s.success(), "git {args:?}");
}

fn stop(home: &Path, cwd: &Path, session: &str) -> serde_json::Value {
    let payload = serde_json::json!({
        "session_id": session,
        "transcript_path": home.join("no-such-transcript.jsonl").to_string_lossy(),
        "cwd": cwd.to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .arg("gate")
        .current_dir(cwd)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("HARNESS_TRUST_ALL", "1")
        .env_remove("TDD_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(s.trim()).unwrap_or_else(|_| panic!("bad stdout: {s}"))
    }
}

fn blocked(v: &serde_json::Value) -> bool {
    v.get("decision").and_then(|d| d.as_str()) == Some("block")
}

#[test]
fn loose_ledger_written_from_linked_worktree_is_seen_from_main_checkout() {
    let home: PathBuf = std::env::temp_dir().join(format!("tdd-loose-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let main = home.join("project");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["config", "user.email", "t@t.com"]);
    git(&main, &["config", "user.name", "t"]);
    let state = home.join("tdd-state");
    std::fs::write(
        main.join("tdd.toml"),
        format!("state_dir = \"{}\"\nmode = \"loose\"\n", state.display()),
    )
    .unwrap();
    std::fs::write(main.join("seed.rs"), "fn seed() {}\n").unwrap();
    git(&main, &["add", "seed.rs", "tdd.toml"]);
    git(
        &main,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
    );
    let wt = home.join("wt");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    let main = main.canonicalize().unwrap();
    let wt = wt.canonicalize().unwrap();

    // Same file, same content, in both checkouts of the one project.
    std::fs::write(wt.join("a.rs"), IMPL_BODY).unwrap();
    std::fs::write(main.join("a.rs"), IMPL_BODY).unwrap();

    let first = stop(&home, &wt, "sA");
    assert!(blocked(&first), "linked worktree must block first: {first}");

    let second = stop(&home, &main, "sB");
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        !blocked(&second),
        "ledger is documented per project: a.rs at this hash was already warned \
         from the linked worktree, yet the main checkout blocked again: {second}"
    );
}
