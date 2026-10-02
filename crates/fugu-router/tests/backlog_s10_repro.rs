// Reproduction tests for backlog items audited in shard s10-small-b.
// Each test is RED while its backlog item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fugu-s10-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_in(home: &Path, cwd: &Path, args: &[&str], payload: &str) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// ceb0a9bf: the UserPromptSubmit hook must put `additionalContext` inside
/// `hookSpecificOutput` (with hookEventName). Observed 2026-10-02 with a real
/// `claude -p` run: a top-level `additionalContext` never reaches the model,
/// the nested form does.
#[test]
#[ignore = "backlog ceb0a9bf: open defect, remove ignore when fixed"]
fn backlog_ceb0a9bf_prompt_hook_nests_additional_context() {
    let home = temp_dir("ceb0");
    for title in ["add login api", "add logout api", "fix api bug"] {
        let (rc, _) = run_in(
            &home,
            &home,
            &[
                "record", "--title", title, "--model", "sonnet", "--status", "verified", "--class",
                "feature",
            ],
            "",
        );
        assert_eq!(rc, 0);
    }
    let (code, stdout) = run_in(
        &home,
        &home,
        &["prompt"],
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"add a login feature to the api"}"#,
    );
    assert_eq!(code, 0);
    assert!(!stdout.trim().is_empty(), "sanity: hook produced no output");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        v["hookSpecificOutput"]["additionalContext"].is_string(),
        "additionalContext is not under hookSpecificOutput: {stdout}"
    );
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        serde_json::json!("UserPromptSubmit"),
        "{stdout}"
    );
}

fn seeded_repo(tag: &str) -> PathBuf {
    let dir = temp_dir(tag);
    let git = |args: &[&str]| {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(dir.join("lib.rs"), "pub fn extract_symbols() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "seed"]);
    dir
}

/// 3f3a0e90: "no index exists" and "index exists, nothing matched" must be
/// distinguishable by a downstream reader.
#[test]
#[ignore = "backlog 3f3a0e90: open defect, remove ignore when fixed"]
fn backlog_3f3a0e90_missing_index_differs_from_no_match() {
    let home = temp_dir("3f3a-home");
    // Case A: index never built.
    let no_index = temp_dir("3f3a-noindex");
    let (rc_a, out_a) = run_in(
        &home,
        &no_index,
        &[
            "code-index",
            "search",
            "--query",
            "zzz_nonexistent_symbol",
            "--root",
            no_index.to_str().unwrap(),
        ],
        "",
    );
    // Case B: index built, query matches nothing.
    let repo = seeded_repo("3f3a-built");
    let (rc_b0, _) = run_in(
        &home,
        &repo,
        &["code-index", "build", "--root", repo.to_str().unwrap()],
        "",
    );
    assert_eq!(rc_b0, 0);
    let (rc_b, out_b) = run_in(
        &home,
        &repo,
        &[
            "code-index",
            "search",
            "--query",
            "zzz_nonexistent_symbol",
            "--root",
            repo.to_str().unwrap(),
        ],
        "",
    );
    eprintln!("A (no index): rc={rc_a} out={out_a:?}\nB (no match): rc={rc_b} out={out_b:?}");
    assert!(
        rc_a != rc_b || out_a.trim() != out_b.trim(),
        "absent index and no-match are byte-identical: rc={rc_a} out={out_a:?}"
    );
}
