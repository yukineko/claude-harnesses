#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog RED (2026-11-27, flow session eaa077f4): a python-stdin heredoc
//! with a ~3000-line body followed by a harmful tail takes ~6s to judge
//! (release build, observed identically on main 00f4f87cf / blastguard 0.2.952
//! and on fp-30136835-r b29342b73 / 0.2.954). hooks.json gives the hook a 10s
//! timeout; a hook that times out does not block the call, so a body a few
//! times larger turns a system-harm Deny into no verdict at all (fail-open).
//! Also observed: with the tail `{ bash <(echo 'rm -rf /etc'); }` the main
//! verdict is Ask, not Deny (sub is Deny).
//!
//! Contract asserted here: the verdict for a harmful tail is Deny in both
//! modes AND it arrives well inside the hook timeout (3s budget, release or
//! debug is the caller's choice — run with --release to match the
//! measurement above).

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

fn run_as(command: &str, subagent: bool) -> (String, Duration) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let home = Home(std::env::temp_dir().join(format!(
        "bg-large-pybody-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )));
    std::fs::create_dir_all(home.0.join("wt")).unwrap();
    let mut p = serde_json::json!({
        "session_id": "backlog-20261127-large-pybody",
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
    let t0 = Instant::now();
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let took = t0.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), took);
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    (
        v["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or("?")
            .to_string(),
        took,
    )
}

fn big_body() -> String {
    (0..3000)
        .map(|i| format!("x{i} = 'it''s {i}' + \"q\"\n"))
        .collect::<String>()
        + "open('out.txt','w').write('done')"
}

#[test]
#[ignore = "RED: ~3000-line python-stdin heredoc body takes ~6s to judge (hook timeout 10s); main verdict for a bash <(...) harmful tail is Ask"]
fn large_python_heredoc_body_harmful_tail_is_deny_fast() {
    let budget = Duration::from_secs(3);
    let mut bad = Vec::new();
    for tail in ["rm -rf /etc", "{ bash <(echo 'rm -rf /etc'); }"] {
        let c = format!("python3 - <<'EOF'\n{}\nEOF\n{tail}", big_body());
        for sub in [false, true] {
            let (d, took) = run_as(&c, sub);
            if d != "deny" || took > budget {
                bad.push(format!("tail={tail:?} sub={sub} -> {d} in {}ms", took.as_millis()));
            }
        }
    }
    assert!(bad.is_empty(), "non-deny or over budget:\n{}", bad.join("\n"));
}

/// Control: the same tails after a small body are Deny quickly today.
#[test]
fn control_small_body_harmful_tail_is_deny_fast() {
    for tail in ["rm -rf /etc", "{ bash <(echo 'rm -rf /etc'); }"] {
        let c = format!("python3 - <<'EOF'\nprint(1)\nEOF\n{tail}");
        for sub in [false, true] {
            let (d, took) = run_as(&c, sub);
            assert_eq!(d, "deny", "tail={tail:?} sub={sub}");
            assert!(took < Duration::from_secs(3), "tail={tail:?} sub={sub} took {took:?}");
        }
    }
}
