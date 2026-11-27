//! RED then GREEN for the verifier FAIL on fp-30136835-r (1b84867e fix): the
//! stand-in line that replaces a python-stdin heredoc body must not collide
//! with the heredoc delimiter. With delimiter `pass` the stand-in `pass` closed
//! the heredoc itself, the body came back EMPTY (not the stand-in), and the
//! re-judging recursed on a text that gained one line per round: 21s release /
//! 71s debug against a 10s hook timeout (a lost verdict), and a false Ask on a
//! benign `ls` tail. Required: bounded time, harmful tail Deny, benign tail
//! Allow (as on base).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_as(command: &str, subagent: bool) -> (String, String, Duration) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-standin-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    let home = Home(dir);
    let mut p = serde_json::json!({
        "session_id": "standin-collision-20261127",
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
    let t = Instant::now();
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let took = t.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new(), took);
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        took,
    )
}

const LIMIT: Duration = Duration::from_secs(2);

/// Every (case, mode) must return `want` within [`LIMIT`].
fn expect(cases: &[(&str, String)], want: &str) {
    let mut bad = Vec::new();
    for (id, c) in cases {
        for sub in [false, true] {
            let (d, r, took) = run_as(c, sub);
            if d != want || took > LIMIT {
                bad.push(format!(
                    "[{id}] subagent={sub}: {d} in {took:?} (want {want} within {LIMIT:?}) {r}"
                ));
            }
        }
    }
    assert!(bad.is_empty(), "expected {want}:\n{}", bad.join("\n"));
}

#[test]
fn delimiter_pass_harmful_tail_is_deny_in_time() {
    expect(
        &[
            (
                "pass-hosts",
                "python3 - <<'pass'\nprint(1)\npass\necho x > /etc/hosts".to_string(),
            ),
            (
                "pass-uneven-rm",
                "python3 -I - <<'pass'\nprint('''it's''')\npass\nrm -rf /etc".to_string(),
            ),
            (
                "pass-procsub",
                "python3 - <<'pass'\nprint(1)\npass\n{ bash <(echo 'rm -rf /etc'); }".to_string(),
            ),
        ],
        "deny",
    );
}

#[test]
fn delimiter_pass_benign_tail_stays_allow() {
    expect(
        &[
            (
                "pass-uneven-ls",
                "python3 -I - <<'pass'\nprint('''it's''')\npass\nls".to_string(),
            ),
            (
                "pass-ls",
                "python3 - <<'pass'\nprint(1)\npass\nls".to_string(),
            ),
        ],
        "allow",
    );
}

/// Other delimiters that can collide with a stand-in line.
#[test]
fn other_colliding_delimiters_harmful_tail_is_deny_in_time() {
    expect(
        &[
            (
                "pass-hash",
                "python3 - <<'pass #'\nprint('''it's''')\npass #\necho x > /etc/hosts".to_string(),
            ),
            (
                "EOF-body-mentions-EOF",
                "python3 - <<'EOF'\nprint(\"EOF\")\nprint('''o'k''')\nEOF\necho x > /etc/hosts"
                    .to_string(),
            ),
            (
                "tab-pass",
                "python3 - <<-'pass'\n\tprint('''it's''')\n\tpass\necho x > /etc/hosts".to_string(),
            ),
        ],
        "deny",
    );
}
