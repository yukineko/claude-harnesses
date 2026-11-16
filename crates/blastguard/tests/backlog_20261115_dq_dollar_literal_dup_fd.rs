#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, side finding F12 observed 2026-11-15 on base 0.2.603):
//! `bash <&"$"fd` is observed ask in main and deny in subagent mode, with the
//! reason "its stdin is duplicated from `"$"fd`, an fd spelled with an
//! expansion". The literal-spelling twin `bash <&'$fd'` is allow in both
//! modes, and the real expansion `bash <&"$fd"` is ask/deny.
//!
//! Two readings: (1) real defect: in bash, `"$"` is a double-quoted lone `$`,
//! which is a literal `$` (nothing follows it inside the quotes to expand);
//! adjacent word `fd` concatenates, so the word is the literal text `$fd`
//! and contains no expansion; the verdict must match `bash <&'$fd'` (allow);
//! (2) intended: any `$` in a double-quoted word is treated as an expansion
//! to stay conservative. Tell apart: a lone `$` before the closing quote is
//! not an expansion, and the project rule is that text-determined values are
//! resolved and judged (a literal word is resolved, not undetermined);
//! the equal-by-bash-semantics `'$fd'` is allow -> reading (1). The same
//! word is not a bash fd either way (`$fd` is not a number), which is
//! outside this test.
//!
//! Asserted: allow in main and subagent mode.
//!
//! DOES NOT PROVE: the verdict of the literal word `$fd` when the shell
//! would later reject it, other `"$"x` spellings (`"$"1`, `"$"$x`), or the
//! reason text. Control only shows that the real expansion `"$fd"` is not
//! allow in either mode.
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
        "bg-side1115-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261115",
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

#[test]
fn dq_dollar_literal_word_is_allow() {
    let cmd = r#"bash <&"$"fd"#;
    let home = unique_home("a");
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "allow", "main; cmd={cmd:?} reason={mr}");
    assert_eq!(s, "allow", "subagent; cmd={cmd:?} reason={sr}");
}

#[test]
fn literal_twin_single_quoted_is_allow() {
    // Observed allow/allow at HEAD: the verdict the F12 spelling must match.
    let cmd = "bash <&'$fd'";
    let home = unique_home("twin");
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "allow", "main; cmd={cmd:?} reason={mr}");
    assert_eq!(s, "allow", "subagent; cmd={cmd:?} reason={sr}");
}

#[test]
fn control_real_expansion_in_double_quotes_is_not_allow() {
    let cmd = r#"bash <&"$fd""#;
    let home = unique_home("ctl");
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_ne!(m, "allow", "main; cmd={cmd:?} reason={mr}");
    assert_ne!(s, "allow", "subagent; cmd={cmd:?} reason={sr}");
}
