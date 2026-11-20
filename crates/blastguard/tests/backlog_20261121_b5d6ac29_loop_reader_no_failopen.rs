#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! No-fail-open guard for backlog b5d6ac29 (`data_loop_landing`): a quoted
//! here-document fed to a compound `while read` loop is treated as data ONLY
//! when the loop merely prints the line. Every case below hands the same
//! dangerous body (`exec 3<<< 'echo hi'; bash <&3`, or `rm -rf ~`) to
//! something that RUNS it — a real shell, `eval`, a bare `$c` command word,
//! `bash -c`, an output pipe into a shell, a written script that is then run,
//! a per-line `| sh`, a trailing `source /dev/stdin`, or an echo operand that
//! evaluates (`${c@P}`, `$(echo $c)`). None may be Allow, in main or subagent.
//!
//! Positive control: the original MINIMAL case (copied verbatim from
//! backlog_20261116_heredoc_data_fd_reader_fp.rs) stays allow in main.
//!
//! DOES NOT PROVE: which pass produces the non-allow decision for each case
//! (only that the decision is not allow); deny vs ask is not distinguished;
//! unquoted here-documents, `until` / `for` loops, other fd numbers and
//! nested compounds are not covered; the `> x.sh && bash x.sh` case may be
//! non-allow solely because of the script-file run, independent of the fix.
//! The `$(echo $c)` case and the plain-text bodies under `${c@P}` do not
//! actually execute the body in bash (word splitting / prompt expansion of
//! text without `$(`); they pin the conservative rule that an evaluating echo
//! operand keeps the body judged, not a proven runtime execution. The
//! `${c@P}` + `$(rm -rf ~)` body IS a real execution path.
//!
//! Written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-b5d6-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "b5d6ac29-loop-reader-no-failopen",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

const BODY: &str = "exec 3<<< 'echo hi'; bash <&3";
const HARM: &str = "rm -rf ~";

/// Copied verbatim from backlog_20261116_heredoc_data_fd_reader_fp.rs.
const MINIMAL: &str =
    "while IFS= read -r c; do echo \"$c\"; done <<'EOF'\nexec 3<<< 'echo hi'; bash <&3\nEOF";

fn with_body(head: &str, body: &str, tail: &str) -> String {
    format!("{head}\n{body}\nEOF{tail}")
}

fn not_allow(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(cmd, &home, false);
    assert_ne!(d, "allow", "main: {cmd:?} => {d} ({r})");
    let (d, r) = run_as(cmd, &home, true);
    assert_ne!(d, "allow", "subagent: {cmd:?} => {d} ({r})");
}

fn both_bodies(tag: &str, head: &str, tail: &str) {
    not_allow(&format!("{tag}-fd"), &with_body(head, BODY, tail));
    not_allow(&format!("{tag}-rm"), &with_body(head, HARM, tail));
}

#[test]
fn control_minimal_stays_allow_in_main() {
    let home = unique_home("ctl");
    let (d, r) = run_as(MINIMAL, &home, false);
    assert_eq!(d, "allow", "main: {MINIMAL:?} => {d} ({r})");
}

#[test]
fn bash_reads_heredoc() {
    both_bodies("bash", "bash <<'EOF'", "");
}

#[test]
fn sh_reads_heredoc() {
    both_bodies("sh", "sh <<'EOF'", "");
}

#[test]
fn loop_eval() {
    both_bodies(
        "eval",
        "while IFS= read -r c; do eval \"$c\"; done <<'EOF'",
        "",
    );
}

#[test]
fn loop_bare_command_word() {
    both_bodies("bare", "while IFS= read -r c; do $c; done <<'EOF'", "");
}

#[test]
fn loop_bash_dash_c() {
    both_bodies(
        "bashc",
        "while IFS= read -r c; do bash -c \"$c\"; done <<'EOF'",
        "",
    );
}

#[test]
fn loop_output_piped_to_bash() {
    both_bodies(
        "pipe",
        "while IFS= read -r c; do echo \"$c\"; done <<'EOF'",
        " | bash",
    );
}

#[test]
fn loop_output_written_then_run() {
    both_bodies(
        "file",
        "while IFS= read -r c; do echo \"$c\"; done <<'EOF'",
        " > x.sh && bash x.sh",
    );
}

#[test]
fn loop_echo_piped_to_sh_per_line() {
    both_bodies(
        "lsh",
        "while IFS= read -r c; do echo \"$c\" | sh; done <<'EOF'",
        "",
    );
}

#[test]
fn loop_extra_source_stdin() {
    both_bodies(
        "src",
        "while IFS= read -r c; do echo \"$c\"; source /dev/stdin; done <<'EOF'",
        "",
    );
}

#[test]
fn loop_echo_prompt_expansion() {
    let head = "while IFS= read -r c; do echo \"${c@P}\"; done <<'EOF'";
    both_bodies("atp", head, "");
    // `${c@P}` performs command substitution on the value: this body RUNS.
    not_allow("atp-cs", &with_body(head, "$(rm -rf ~)", ""));
}

#[test]
fn loop_echo_command_substitution() {
    both_bodies(
        "cs",
        "while IFS= read -r c; do echo \"$(echo $c)\"; done <<'EOF'",
        "",
    );
}
