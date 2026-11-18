#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin (2026-11-18, relay bgr-d085f75f9fa7): a quoted heredoc written to
//! a `.json` / `.jsonl` DATA file, whose body is a JSON string that mentions
//! `python3 -c "import sys;print(1)"`, is refused with
//!
//! > `python3` is invoked with an inline-eval flag, and its program cannot
//! > be tokenised as Python (an unterminated string or an unbalanced
//! > bracket) — blastguard cannot tell its code from its strings, so it
//! > cannot vouch for it
//!
//! Nothing runs the file; the command is only `cat > F <<'EOF' … EOF`.
//! The body line is read as shell, split at the `;` that sits inside the
//! JSON string, and the fragment `python3 -c \"import sys` is judged as an
//! inline-eval program.
//!
//! Expected: Allow. Grounds:
//!   * user ruling 2026-11-08 (memory `heredoc-body-by-target-language`):
//!     only a `.sh` or extension-less target has its quoted-heredoc body
//!     judged as shell; other targets are not read as shell lines unless the
//!     same command runs the file.
//!   * the identical body written to a `.txt` file is Allow today (control),
//!     so the extension alone flips the verdict.
//!   * memory `false-positives-not-tolerated`.
//!
//! Observed at origin/main 1dfa60681 (blastguard 0.2.699), debug binary:
//! `.jsonl` and `.json` targets → main `ask` (subagent `deny`); `.txt` →
//! `allow`; the same body without `import sys;` → `allow`.
//!
//! DOES NOT PROVE: the `.sh` target (where the body IS shell, the quoted
//! word `"python3 -c \"import sys;print(1)\""` is a single command word; the
//! `.sh` case is also `ask` today and is not pinned here); bodies a later
//! part of the same command runs.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "hd-json-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base, home }
}

fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "hd-json-20261118",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &fx.proj(), command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

/// `cat > <name> <<'EOF'` with one JSON-string body line.
fn heredoc(name: &str, body: &str) -> String {
    format!("cat > {name} <<'EOF'\n{body}\nEOF")
}

const BODY: &str = r#""python3 -c \"import sys;print(1)\"""#;

// ---------------------------------------------------------------- controls

/// Control: the same body to a `.txt` file is Allow.
#[test]
fn control_txt_target_allows() {
    allow("txt", &heredoc("cases.txt", BODY));
}

/// Control: a body without the `;` inside the JSON string is Allow.
#[test]
fn control_jsonl_without_semicolon_allows() {
    allow(
        "nosemi",
        &heredoc("cases.jsonl", r#""python3 -c \"print(1)\"""#),
    );
}

// --------------------------------------------------------------------- RED

#[test]
#[ignore = "RED: quoted heredoc .jsonl data body read as shell → python3 -c fragment refused (FP, bgr-d085f75f9fa7)"]
fn jsonl_target_allows() {
    allow("jsonl", &heredoc("cases.jsonl", BODY));
}

#[test]
#[ignore = "RED: quoted heredoc .json data body read as shell → python3 -c fragment refused (FP, bgr-d085f75f9fa7)"]
fn json_target_allows() {
    allow("json", &heredoc("cases.json", BODY));
}
