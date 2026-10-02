// Reproduction tests for backlog items audited in shard s10-small-b.
// Each test is RED while its backlog item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("si-s10-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_in(home: &Path, args: &[&str], payload: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_session-insights"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
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
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assistant_line(n: u64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"2026-09-24T00:00:0{}.000Z\",\
         \"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\
         \"usage\":{{\"input_tokens\":1000,\"output_tokens\":1000}}}}}}\n",
        n % 10
    )
}

/// fa86a52f: a session whose only edits go through Bash must not report
/// `files: 0` (a confident "touched nothing").
#[test]
#[ignore = "backlog fa86a52f: open defect, remove ignore when fixed"]
fn backlog_fa86a52f_bash_only_edits_are_not_reported_as_zero_files() {
    let home = temp_home();
    let payload = r#"{"hook_event_name":"PostToolUse","session_id":"s10-bash","tool_name":"Bash","tool_input":{"command":"echo hi > /tmp/some-file.txt"}}"#;
    let (rc, _, _) = run_in(&home, &["record"], payload);
    assert_eq!(rc, 0);
    let (code, stdout, _) = run_in(&home, &["report", "--session", "s10-bash"], "");
    assert_eq!(code, 0);
    assert!(stdout.contains("tool events: 1"), "sanity: {stdout}");
    assert!(
        !stdout.contains("files: 0"),
        "Bash-only session reported as touching 0 files (should be unknown): {stdout}"
    );
}

/// Builds a vault + config + transcript, runs `stop` `stops` times, then
/// `sessionend`, and returns the record note body.
fn record_note(tag: &str, main_turns: u64, stops: u32, unreadable_sub: bool) -> Option<String> {
    let home = temp_home();
    let vault = home.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        home.join("session-insights.toml"),
        format!(
            "record = true\nobsidian_log = false\nobsidian_vault = \"{}\"\n",
            vault.display()
        ),
    )
    .unwrap();
    let sid = format!("s10-{tag}");
    let tdir = home.join("proj");
    std::fs::create_dir_all(&tdir).unwrap();
    let tp = tdir.join(format!("{sid}.jsonl"));
    let mut body = String::new();
    for i in 0..main_turns {
        body.push_str(&assistant_line(i));
    }
    std::fs::write(&tp, body).unwrap();
    let sub_file = tdir.join(&sid).join("subagents").join("agent-aaa111.jsonl");
    if unreadable_sub {
        std::fs::create_dir_all(sub_file.parent().unwrap()).unwrap();
        std::fs::write(&sub_file, assistant_line(1)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sub_file, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            std::fs::read_to_string(&sub_file).is_err(),
            "injection ineffective (running as root?)"
        );
    }
    let stop_payload = format!(
        "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{sid}\",\"cwd\":\"{}\"}}",
        home.display()
    );
    for _ in 0..stops {
        run_in(&home, &["stop"], &stop_payload);
    }
    let end_payload = format!(
        "{{\"hook_event_name\":\"SessionEnd\",\"session_id\":\"{sid}\",\"cwd\":\"{}\",\"transcript_path\":\"{}\"}}",
        home.display(),
        tp.display()
    );
    let (rc, _, _) = run_in(&home, &["sessionend"], &end_payload);
    assert_eq!(rc, 0);
    let mut stack = vec![vault.clone()];
    let mut found = None;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                found = Some(std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    if unreadable_sub {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sub_file, std::fs::Permissions::from_mode(0o644));
    }
    found
}

fn line_with<'a>(note: &'a str, prefix: &str) -> &'a str {
    note.lines()
        .find(|l| l.trim_start().starts_with(prefix))
        .unwrap_or("<absent>")
}

/// 2faad055: the numeric summary's `turns` and the cost block's per-agent
/// `(N turns)` are the same word for two different quantities.
#[test]
#[ignore = "backlog 2faad055: open defect, remove ignore when fixed"]
fn backlog_2faad055_numeric_turns_and_cost_turns_agree() {
    let note = record_note("turns", 5, 2, false).expect("record note written");
    let numeric = line_with(&note, "- turns:");
    let agent = line_with(&note, "- main:");
    eprintln!("{numeric}\n{agent}\n---\n{note}");
    let n: u64 = numeric
        .trim_start()
        .trim_start_matches("- turns:")
        .trim()
        .parse()
        .unwrap();
    let m: u64 = agent
        .rsplit('(')
        .next()
        .unwrap()
        .trim_end_matches(" turns)")
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        n, m,
        "numeric summary says {n} turns, cost block says {m} turns"
    );
}

/// 285eddc7: when the sub-agent scan is undetermined the turns figure is an
/// under-count; the note body must not present it as a plain number.
#[cfg(unix)]
#[test]
#[ignore = "backlog 285eddc7: open defect, remove ignore when fixed"]
fn backlog_285eddc7_undercounted_turns_are_not_plain_in_note() {
    let note = record_note("under", 3, 0, true).expect("record note written");
    let numeric = line_with(&note, "- turns:");
    eprintln!("{note}");
    let plain = numeric
        .trim_start()
        .trim_start_matches("- turns:")
        .trim()
        .chars()
        .all(|c| c.is_ascii_digit());
    assert!(
        !plain,
        "turns line is a bare number despite unreadable sub-agent: {numeric:?}"
    );
}
