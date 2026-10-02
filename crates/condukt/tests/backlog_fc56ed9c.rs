// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog fc56ed9c: `condukt editgate` carries a verdict (it prints
//! `{"decision":"block",...}`) yet is dispatched through `run_hook`, which
//! `catch_unwind`s and then `exit(0)`s unconditionally. A panic inside the gate
//! therefore becomes a silent exit 0 — indistinguishable from "allow".
//!
//! The panic is induced from outside, without touching production code: the
//! gate's stdout pipe is closed before it prints its block verdict, so
//! `println!` panics with "failed printing to stdout" (Rust ignores SIGPIPE).
//! A fail-closed barrier must surface that as a non-zero exit.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn project_key(root: &Path) -> String {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let full = canon.to_string_lossy();
    let base = canon
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".into());
    let sani: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{}-{:08x}", sani, fnv1a32(&full))
}

fn broken_fixture() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    let wt = repo.path().join("broken_wt");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(
        wt.join("Cargo.toml"),
        "[package]\nname = \"broken_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    std::fs::write(
        wt.join("src").join("lib.rs"),
        "pub fn f() -> i32 {\n    let s: &str = \"nope\";\n    s\n}\n",
    )
    .unwrap();
    let state_dir = home
        .path()
        .join(".condukt")
        .join("state")
        .join(project_key(repo.path()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let run_json = serde_json::json!({
        "run_id": "editgate-fc56",
        "goal": "editgate panic barrier",
        "tasks": [ {"id": "a", "status": "pending", "worktree": wt.to_string_lossy()} ]
    });
    std::fs::write(
        state_dir.join("editgate-fc56.json"),
        serde_json::to_string_pretty(&run_json).unwrap(),
    )
    .unwrap();
    let file = wt.join("src").join("lib.rs");
    let repo_path = repo.path().to_path_buf();
    (home, repo, repo_path, file)
}

#[test]
#[ignore = "backlog fc56ed9c: open defect, remove ignore when fixed"]
fn a_panicking_editgate_does_not_exit_zero() {
    let (home, _repo, repo_path, file) = broken_fixture();
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": file.to_string_lossy() },
    })
    .to_string();
    let mut child = Command::new(BIN)
        .arg("editgate")
        .current_dir(&repo_path)
        .env("HOME", home.path())
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn condukt editgate");
    // Close the read end of stdout BEFORE the gate prints its verdict.
    drop(child.stdout.take());
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(payload.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().expect("wait for condukt editgate");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("panicked"),
        "fixture precondition: the gate must actually panic (EPIPE on its verdict); \
         stderr: {stderr}"
    );
    assert!(
        !out.status.success(),
        "the edit gate panicked while delivering a block verdict, yet exited {:?} — \
         run_hook mapped 'cannot deliver a verdict' to allow. stderr: {stderr}",
        out.status.code()
    );
}
