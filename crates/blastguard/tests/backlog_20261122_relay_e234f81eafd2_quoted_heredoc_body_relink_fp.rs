//! RED (false positive; verbatim relay replay; independent test author,
//! 2026-11-22). Observed in condukt run run-20261122-020202-63344 (main
//! 6ae161ee7 era), relay bgr-e234f81eafd2 (subagent, Deny). The command was
//!   cd <task worktree> && cat > <scratchpad>/new_body.rs <<'EOF'
//!   ... Rust source (comments mention `--no-renames`, `(backlog <inner>)`) ...
//!   EOF
//!   python3 - <<'EOF' ... p='<task worktree>/crates/backlog/src/evidence.rs'
//!   ... open(p,'w').write(s) EOF
//!   grep -n "fn fixed_task_ids" -B3 crates/backlog/src/evidence.rs
//! Verbatim reason: "'> /Users/yuki/.condukt/worktrees/run-20261122-020202-63344-d363a/(':
//! ``--no-renames` lists both sides of a` may create or rename an entry on
//! this path and is not modelled, so blastguard cannot tell which file the
//! write really lands on" -- text INSIDE the quoted heredoc body (a Rust
//! comment, `--no-renames` holds the relink keyword `rename`) is judged as an
//! unmodelled relinking command, and a `(` from body text is read as a
//! redirect target. The body lands in a `.rs` file read as data by python3
//! (memory heredoc-body-by-target-language: a .rs body is not shell; 2026-11-20
//! narrow data reading: the reader is the real cat and nothing executes the
//! landing file).
//!
//! The fixture `support/relay_bgr_e234f81eafd2_20261122_exact.txt` is the exact
//! relayed command; the RED replays its first part verbatim (`cd <wt> && cat >
//! <scratchpad>/new_body.rs <<'EOF' ... EOF`, the part the reason quotes); the
//! only rewrites are the task-worktree path and the scratchpad directory ->
//! this test's cwd. The trailing python3/grep lines are NOT replayed: with
//! them, even a plain body Asks ("the Python program has effects and the
//! directory it runs in is not determined"), a separate question.
//!
//! Readings: (a) FP -- quoted .rs heredoc data and a Python string are judged as
//! shell; (b) intended -- relink.rs deliberately treats any segment holding a
//! relink keyword as unmodelled (same mechanism as 3de4729f, 'rsync' inside a
//! jq filter / commit message). Tell apart: whether the "segment" the relink
//! layer reads is a real shell segment or heredoc body text.
//!
//! DOES NOT PROVE: which body line triggers it (not minimised); the original
//! scratchpad Scratch classification; real execution.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

const EXACT: &str = include_str!("support/relay_bgr_e234f81eafd2_20261122_exact.txt");
const ORIG_WT: &str = "/Users/yuki/.condukt/worktrees/run-20261122-020202-63344-d363a";
const ORIG_SCRATCH: &str =
    "/private/tmp/claude-502/-Users-yuki-src-harness/eaa077f4-02a7-458b-9e0e-c7fc2c5d694b/scratchpad";

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
        "bg-relaye234f-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "relay-e234f81eafd2-20261122",
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

fn unallowed(cmd: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let wt = home.0.join("wt");
    let wt = wt.to_str().unwrap();
    let cmd = cmd.replace(ORIG_WT, wt).replace(ORIG_SCRATCH, wt);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        if d != "allow" {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    bad
}

/// The relayed command up to and including the cat heredoc's closing `EOF`
/// (the part the relayed reason quotes); the python3 / grep lines that follow
/// are dropped because `cd X && cat ...` followed by a python3 line Asks for
/// an unrelated reason ("the directory it runs in is not determined",
/// observed 2026-11-22 on the full-shape control at e84c666da).
fn cat_part() -> &'static str {
    let end = EXACT
        .find("\nEOF\npython3 - <<'EOF'")
        .expect("fixture: cat heredoc terminator present");
    &EXACT[..end + "\nEOF".len()]
}

#[test]
#[ignore = "RED: verbatim relay bgr-e234f81eafd2 quoted .rs heredoc body text judged as an unmodelled relink redirect"]
fn relay_e234f81eafd2_exact_cat_heredoc_is_allow() {
    assert!(
        EXACT.contains(ORIG_WT) && EXACT.contains(ORIG_SCRATCH),
        "fixture: paths present"
    );
    let bad = unallowed(cat_part(), "exact");
    assert!(
        bad.is_empty(),
        "expected Allow for the verbatim relayed cat heredoc:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_same_cat_heredoc_with_plain_body_is_allow() {
    let cmd = format!("cd {ORIG_WT} && cat > {ORIG_SCRATCH}/new_body.rs <<'EOF'\nfn x() {{}}\nEOF");
    let bad = unallowed(&cmd, "ctl");
    assert!(
        bad.is_empty(),
        "control expected Allow:\n{}",
        bad.join("\n")
    );
}
