#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for spec `bg-exec-fd-tracking` (specs/bg-exec-fd-tracking.toml),
//! acceptance items of R1..R8 (R1's first item lives in
//! `backlog_20261114_exec_fd_herestring_then_shell_reads_fd.rs`; R8's last item
//! is an existing regression test and is not duplicated; R9 is process, not
//! behaviour). One #[test] per acceptance item, asserting main and subagent.
//!
//! PROVES (when green): the cross-statement `exec N<<< literal` / quoted
//! here-doc fd tracking verdicts stated by the acceptance text, and that the
//! out-of-model forms never resolve to allow.
//!
//! DOES NOT PROVE: fd numbers/readers/shapes not named in an acceptance item,
//! unquoted here-doc bodies with other expansions, real execution of the
//! commands (verdicts only, via the hook binary), or R9 (docs/version).
//! Equality / "not looser than" items prove consistency between two verdicts,
//! not that either verdict is the right one in absolute terms.
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-execfd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "spec-bg-exec-fd-tracking",
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

const HARM: &str = "chmod 777 /etc/hosts";

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unknown decision {other}"),
    }
}

/// Assert every cmd is deny in main and subagent; collects all failures.
fn deny_both(cmds: &[String]) {
    let home = unique_home("deny");
    let mut fails = Vec::new();
    for cmd in cmds {
        for sub in [false, true] {
            let (d, r) = run_as(cmd, &home, sub);
            if d != "deny" {
                fails.push(format!(
                    "subagent={sub}: {cmd:?} => {d} ({r}) expected deny"
                ));
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// Assert verdict(a) == verdict(b) per mode.
fn same_verdict(a: &str, b: &str) {
    let home = unique_home("eq");
    let mut fails = Vec::new();
    for sub in [false, true] {
        let (da, ra) = run_as(a, &home, sub);
        let (db, rb) = run_as(b, &home, sub);
        if da != db {
            fails.push(format!(
                "subagent={sub}: {a:?} => {da} ({ra}) but {b:?} => {db} ({rb})"
            ));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// Assert every cmd is ask in main and deny in subagent; collects failures.
fn ask_main_deny_sub(cmds: &[String]) {
    let home = unique_home("ask");
    let mut fails = Vec::new();
    for cmd in cmds {
        let (d, r) = run_as(cmd, &home, false);
        if d != "ask" {
            fails.push(format!("main: {cmd:?} => {d} ({r}) expected ask"));
        }
        let (d, r) = run_as(cmd, &home, true);
        if d != "deny" {
            fails.push(format!("subagent: {cmd:?} => {d} ({r}) expected deny"));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

// ---- R1 ----

#[test]
#[ignore = "RED: 2026-11-14 newline/&&/|| separated harm is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r1_newline_andand_oror_separators_deny() {
    let cmds: Vec<String> = ["\n", " && ", " || "]
        .iter()
        .map(|s| format!("exec 3<<< '{HARM}'{s}bash <&3"))
        .collect();
    deny_both(&cmds);
}

#[test]
#[ignore = "RED: 2026-11-14 bash /dev/fd/3 harm is Allow in main and subagent (expected Deny) — remove the ignore when fixed"]
fn r1_dev_fd_3_deny() {
    deny_both(&[format!("exec 3<<< '{HARM}'; bash /dev/fd/3")]);
}

#[test]
#[ignore = "RED: 2026-11-14 sh/zsh <&5 harm is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r1_fd5_sh_and_zsh_deny() {
    deny_both(&[
        format!("exec 5<<< '{HARM}'; sh <&5"),
        format!("exec 5<<< '{HARM}'; zsh <&5"),
    ]);
}

#[test]
#[ignore = "RED: 2026-11-14 fd-read echo hi is ask/deny but inline here-string is allow — remove the ignore when fixed"]
fn r1_echo_hi_verdict_equals_inline_herestring() {
    same_verdict("exec 3<<< 'echo hi'; bash <&3", "bash <<< 'echo hi'");
}

// ---- R2 ----

#[test]
fn r2_quoted_heredoc_fd_harm_deny() {
    deny_both(&[format!("exec 3<<'EOF'\n{HARM}\nEOF\nbash <&3")]);
}

#[test]
#[ignore = "RED: 2026-11-14 fd-read quoted heredoc echo hi is ask/deny but inline heredoc is allow — remove the ignore when fixed"]
fn r2_quoted_heredoc_fd_echo_equals_inline_heredoc() {
    same_verdict(
        "exec 3<<'EOF'\necho hi\nEOF\nbash <&3",
        "bash <<'EOF'\necho hi\nEOF",
    );
}

// ---- R3 ----

#[test]
#[ignore = "RED: 2026-11-14 python3 <&3 harm is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r3_python3_reader_deny() {
    deny_both(&[
        r#"exec 3<<< 'import os; os.chmod("/etc/hosts", 0o777)'; python3 <&3"#.to_string(),
    ]);
}

#[test]
#[ignore = "RED: 2026-11-14 node <&3 harm is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r3_node_reader_deny() {
    deny_both(&[
        r#"exec 3<<< 'require("fs").chmodSync("/etc/hosts", 0o777)'; node <&3"#.to_string(),
    ]);
}

// ---- R4 ----

#[test]
#[ignore = "RED: 2026-11-14 reassigned-to-harm fd read is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r4_reassign_latest_harm_deny() {
    deny_both(&[format!("exec 3<<< 'echo hi'; exec 3<<< '{HARM}'; bash <&3")]);
}

#[test]
#[ignore = "RED: 2026-11-14 reassigned-to-echo-hi fd read is ask/deny but inline is allow — remove the ignore when fixed"]
fn r4_reassign_latest_benign_equals_inline() {
    same_verdict(
        &format!("exec 3<<< '{HARM}'; exec 3<<< 'echo hi'; bash <&3"),
        "bash <<< 'echo hi'",
    );
}

#[test]
#[ignore = "RED: 2026-11-14 independent fd 4 harm read is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r4_independent_fds_deny() {
    deny_both(&[format!("exec 3<<< 'echo hi'; exec 4<<< '{HARM}'; bash <&4")]);
}

// ---- R5 ----

#[test]
#[ignore = "RED: 2026-11-14 close-then-read of harm fd is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r5_close_after_harm_deny() {
    deny_both(&[format!("exec 3<<< '{HARM}'; exec 3<&-; bash <&3")]);
}

#[test]
fn r5_close_after_benign_ask_main_deny_subagent() {
    ask_main_deny_sub(&["exec 3<<< 'echo hi'; exec 3<&-; bash <&3".to_string()]);
}

// ---- R6 ----

#[test]
#[ignore = "RED: 2026-11-14 brace-wrapped exec harm is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r6_exec_inside_brace_then_reader_deny() {
    deny_both(&[format!("{{ exec 3<<< '{HARM}'; }}; bash <&3")]);
}

#[test]
#[ignore = "RED: 2026-11-14 brace-wrapped reader harm is Ask in main (expected Deny) — remove the ignore when fixed"]
fn r6_reader_inside_brace_deny() {
    deny_both(&[format!("exec 3<<< '{HARM}'; {{ bash <&3; }}")]);
}

#[test]
fn r6_brace_reader_not_looser_than_unbraced() {
    let home = unique_home("r6");
    let mut fails = Vec::new();
    for sub in [false, true] {
        let (db, rb) = run_as("exec 3<<< 'echo hi'; { bash <&3; }", &home, sub);
        let (du, ru) = run_as("exec 3<<< 'echo hi'; bash <&3", &home, sub);
        if rank(&db) < rank(&du) {
            fails.push(format!(
                "subagent={sub}: braced => {db} ({rb}) looser than unbraced => {du} ({ru})"
            ));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

// ---- R7 ----

fn r7_forms(body: &str) -> Vec<String> {
    vec![
        format!("( exec 3<<< '{body}'; bash <&3 )"),
        format!("f() {{ exec 3<<< '{body}'; bash <&3; }}; f"),
        format!("true && exec 3<<< '{body}'; bash <&3"),
        format!("if true; then exec 3<<< '{body}'; fi; bash <&3"),
        format!("exec 3<<EOF\n{body}\nEOF\nbash <&3"),
        format!("exec 3<<< '{body}'; exec 4<&3; bash <&4"),
        format!("exec 3<<< '{body}'; ( bash <&3 )"),
        format!("exec 3<<< '{body}'; g() {{ bash <&3; }}; g"),
    ]
}

#[test]
#[ignore = "RED: 2026-11-14 out-of-model literal-harm forms are Ask in main (expected Deny) — remove the ignore when fixed"]
fn r7_out_of_model_forms_with_literal_harm_deny() {
    deny_both(&r7_forms(HARM));
}

#[test]
fn r7_out_of_model_forms_benign_ask_main_deny_subagent() {
    ask_main_deny_sub(&r7_forms("echo hi"));
}

#[test]
fn r7_unquoted_heredoc_with_expansion_ask_main_deny_subagent() {
    ask_main_deny_sub(&["exec 3<<EOF\nchmod 777 $T\nEOF\nbash <&3".to_string()]);
}

// ---- R8 ----

#[test]
fn r8_exec_file_fd_equals_stdin_file() {
    same_verdict("exec 3< x.sh; bash <&3", "bash < x.sh");
}

#[test]
fn r8_exec_dup_stdin_equals_bash_dup_stdin() {
    same_verdict("exec 3<&0; bash <&3", "bash <&0");
}
