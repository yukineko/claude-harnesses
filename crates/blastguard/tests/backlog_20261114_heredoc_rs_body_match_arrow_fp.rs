#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 by a condukt worker at main
//! 144906a1, blastguard 0.2.523 live; reproduced at 279e5a66 / 0.2.524): a
//! Bash command of the shape `cat > f.rs <<'EOF' <rust body> EOF` followed by a
//! second statement `python3 - <<'EOF' ... EOF` is DENIED with
//! "'> format!(\"{v:.p$}\"' destroys the file's current contents and
//! `format!(\"{v:.p$}\"` contains a shell expansion, so blastguard cannot tell
//! which file this names, ...". The `>` is the arrow of the Rust match arm
//! `b'f' => format!(...)` inside the QUOTED heredoc body, which is data, not a
//! redirect. User rulings: a quoted heredoc body written to a .rs/.py file is
//! not judged as shell unless run in the same command; gate false positives
//! are bugs.
//!
//! Isolated by probing the built binary: the .rs heredoc alone is Allow; the
//! `=>` body plus a following `python3 - <<'EOF'` heredoc statement is Deny
//! (`cd dir &&` prefix and trailing `grep` are irrelevant). The full 732-line
//! original command is also Deny with the same reason.
//!
//! Asserted: both minimal shapes are Allow, main and subagent mode. Controls
//! (not ignored): a real redirect into /etc/hosts and a quoted heredoc that is
//! then executed (`sh x.sh` with a recursive rm) stay Deny.
//!
//! DOES NOT PROVE: that the ordinary-file redirect check is right for other
//! heredoc bodies, that `=>` is the only trigger (the bare `$` in `p$` may
//! also matter), or that a heredoc body written to a .sh file and executed
//! later is handled beyond the one control shape.

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
        "bg-hdarrow-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "heredoc-arrow-20261114",
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

fn verdicts_not(shapes: &[(&str, &str)], tag: &str, want: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut failures = Vec::new();
    for (label, c) in shapes {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            println!("[{label}] subagent={subagent} {c:?} -> {d} | {r}");
            if d != want {
                failures.push(format!("[{label}] subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    failures
}

const FP_SHAPES: &[(&str, &str)] = &[
    (
        "rs-arrow-then-python-heredoc",
        "cat > x.rs <<'EOF'\nb'f' => format!(\"{v:.p$}\"),\nEOF\npython3 - <<'EOF'\nprint(1)\nEOF",
    ),
    (
        "cd-rs-match-then-python-heredoc-then-grep",
        "cd d && cat > src/x.rs <<'EOF'\nfn f(v: f64, p: usize) -> String { match 1 { _ => format!(\"{v:.p$}\") } }\nEOF\npython3 - <<'EOF'\nprint(1)\nEOF\ngrep -n x src/x.rs",
    ),
];

// test-weakening-justified: new RED for an unfixed false positive (2026-11-14);
// the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 quoted heredoc .rs body with a match arrow (=>) plus a following python3 heredoc is denied as a redirect — remove the ignore when fixed"]
fn quoted_heredoc_rs_body_with_match_arrow_is_allowed() {
    let failures = verdicts_not(FP_SHAPES, "fp", "allow");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) runs were NOT Allow:\n{}",
        failures.len(),
        FP_SHAPES.len() * 2,
        failures.join("\n")
    );
}

// Controls: genuine destructive shapes stay Deny.
#[test]
fn control_real_redirects_and_executed_heredoc_stay_denied() {
    let failures = verdicts_not(
        &[
            ("redirect-system-dir", ": > /etc/hosts"),
            (
                "heredoc-sh-then-executed",
                "cat > x.sh <<'EOF'\nrm -rf ~\nEOF\nsh x.sh",
            ),
        ],
        "ctl",
        "deny",
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
