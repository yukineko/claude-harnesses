#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-17 twice in subagents; relays
//! bgr-fac0e88ff878 (8160 B command) and bgr-4bcd8c5f1c96 (4777 B command),
//! both subagent deny / main ask).
//!
//! Shape: `cd <literal dir> && python3 - <<'EOF'` (or `python3 /dev/stdin`)
//! whose Python program rewrites a Rust source file with `s.replace('''…''',
//! '''…''')` calls. The Rust text inside the Python strings holds doc comments
//! with many backtick spans (`[`queue`]`, `` `askrelay mark-executed` ``),
//! `\\"` escapes and parentheses. Verbatim reason (main ask / subagent deny):
//!   command is too complex to analyse within the safety budget — blastguard
//!   cannot vouch for it either way
//!
//! This is NOT the 2026-11-16 parse cap (> 8 KiB / depth 16 → Deny): both
//! observed commands are under 8192 bytes, and the parse cap is not wired yet
//! (measured 2026-11-17 on main 0.2.681: a 9623-byte `cat > n.txt <<'EOF'`
//! heredoc is Allow). The reason is the legacy `detect_bash` node budget
//! (`detect/budget.rs` `MAX_ANALYSIS_NODES = 2_000`), whose doc comment says
//! "Ordinary commands use fewer than a dozen visits; the cap is three orders of
//! magnitude above that, so it cannot be reached by real work". Both relays are
//! real work that reached it, so that doc comment and the behaviour disagree.
//!
//! Fixture: `support/budget_fp_20261117_python_body.txt` is the
//! bgr-4bcd8c5f1c96 command after a chunked line delta-debug plus per-line
//! truncation, keeping "safety budget" in the main verdict (2216 bytes incl.
//! the `cd` line). The shrink made the Python syntactically broken on purpose
//! (truncated lines); blastguard does not parse Python today, and the program
//! is never executed by this test.
//! Synthetic bodies did NOT reproduce (all Allow, measured 2026-11-17): up to
//! 320 repeats of `.map(|s| (*s).to_string())`, `a = b; c = d && e || f | g`,
//! `x = 1`; up to 200 repeats of string literals with `;`/`&&`/`|`/`(…)`/`{…}`;
//! 20 line continuations; 64 repeats of balanced backtick spans in a
//! triple-quoted string. So the fan-out trigger is not a single repeated token.
//!
//! Readings: (a) defect — a quoted python stdin program is re-read as shell
//! text, and those re-readings fan out until the node budget is spent; (b)
//! intended — budget exhaustion is designed to Ask (D4). (b) covers what to do
//! WHEN the budget runs out, not that a 2 KiB benign program spends it: the
//! module doc calls that unreachable. Structural fix: S6 retires the string
//! re-readings; until S5 the SUBSET.md B.5 python row is Undetermined → Ask, so
//! this goes GREEN only after S5 + S6.
//!
//! DOES NOT PROVE: which re-reading fans out (not identified); that the 8160 B
//! relay (bgr-fac0e88ff878) shares the same internal path (same reason, same
//! shape class, separate shrink not finished); anything about commands over
//! 8 KiB (those are Deny by the 2026-11-16 ruling once the cap is wired).
//!
//! RED written by the triage agent, which did not write the blastguard code
//! under test; `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

const BODY: &str = include_str!("support/budget_fp_20261117_python_body.txt");

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
        "bg-pybudget-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("sub")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pybudget-20261117",
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

/// `cd <HOME>/wt/sub && python3 - <<'EOF'` + `body` (which ends with `EOF`).
fn command(home: &Home, body: &str) -> String {
    let dir = home.0.join("wt").join("sub");
    format!("cd {} && python3 - <<'EOF'\n{body}", dir.display())
}

#[test]
fn fixture_is_under_the_parse_cap() {
    let home = unique_home("len");
    assert!(command(&home, BODY).len() < 8192);
}

#[test]
#[ignore = "RED: 2026-11-17 (S6; needs S5) a 2 KiB quoted python stdin program exhausts the legacy node budget (too complex to analyse) — remove when fixed"]
fn quoted_python_stdin_program_does_not_exhaust_the_budget() {
    let home = unique_home("fp");
    let cmd = command(&home, BODY);
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: a 2 KiB quoted python program is ordinary work; reason={r}"
        );
    }
}

#[test]
fn control_same_body_removing_a_system_tree_is_not_allowed() {
    let home = unique_home("c1");
    let body = format!(
        "import shutil\nshutil.rmtree('/usr')\n{}",
        BODY.trim_end_matches('\n')
    );
    let cmd = command(&home, &body);
    let (d, r) = run_as(&cmd, &home, false);
    assert_ne!(d, "allow", "main: reason={r}");
    let (d, r) = run_as(&cmd, &home, true);
    assert_eq!(d, "deny", "subagent: reason={r}");
}

#[test]
fn control_small_python_stdin_removing_a_system_tree_stays_denied() {
    let home = unique_home("c2");
    let cmd = command(&home, "import shutil\nshutil.rmtree('/usr')\nEOF");
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        assert_eq!(d, "deny", "subagent={subagent}: reason={r}");
    }
}
