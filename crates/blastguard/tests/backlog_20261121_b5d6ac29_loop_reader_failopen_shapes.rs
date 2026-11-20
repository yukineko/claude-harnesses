#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Untested fail-open shapes for backlog b5d6ac29 (`data_loop_landing`, commit
//! 70574b724). Each shape wraps a read/echo-only `while read` loop over a
//! quoted heredoc in something that makes bash RUN the body: a DEBUG trap that
//! evals each line, xtrace expanding `PS4` (command substitution) after `read`
//! assigns it, `exec > >(bash)`, an outer brace group redirected to a script
//! that is run, an outer subshell piped to `bash`. Shapes 1-5 assert NOT
//! allow in main and subagent.
//!
//! Each shape also runs a CONTROL with `cat <<'EOF'` in place of the loop.
//! For shapes 3-5 the cat control also feeds the body to bash, so it is
//! asserted not-allow too (a cat-control failure means the hole predates the
//! fix). For shapes 1-2 the cat control does NOT run the body in bash (`$c`
//! / `PS4` are never assigned from it), so its verdict is printed only.
//!
//! Shape 6 (nested compounds inside the loop) is INFORMATIONAL: verdicts are
//! printed, nothing asserted.
//!
//! DOES NOT PROVE: which pass produces a non-allow decision; deny vs ask is
//! not distinguished; that bash actually executes the body in each shape is
//! inferred from bash semantics, not run here.
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
        "bg-b5d6s-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "b5d6ac29-loop-reader-failopen-shapes",
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
/// For PS4 (xtrace): only a command substitution in the value executes.
const PS4_BODY: &str = "$(exec 3<<< 'echo hi'; bash <&3)";
const LOOP: &str = "while IFS= read -r c; do echo \"$c\"; done <<'EOF'";
const CAT: &str = "cat <<'EOF'";

fn with_body(head: &str, body: &str, tail: &str) -> String {
    format!("{head}\n{body}\nEOF{tail}")
}

/// Runs `cmd` as main and as subagent, prints both decisions, returns them.
fn verdicts(label: &str, cmd: &str) -> (String, String, String, String) {
    let home = unique_home(label);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    println!("VERDICT {label}: main={m} sub={s} | cmd={cmd:?}");
    println!("  main reason: {mr}");
    println!("  sub  reason: {sr}");
    (m, mr, s, sr)
}

/// Collects (does not panic on) allow decisions so every verdict of a test is
/// printed before the test fails.
fn assert_not_allow(label: &str, cmd: &str) -> Vec<String> {
    let (m, mr, s, sr) = verdicts(label, cmd);
    let mut fails = Vec::new();
    if m == "allow" {
        fails.push(format!("main {label}: {cmd:?} => {m} ({mr})"));
    }
    if s == "allow" {
        fails.push(format!("subagent {label}: {cmd:?} => {s} ({sr})"));
    }
    fails
}

fn finish(fails: Vec<String>) {
    assert!(fails.is_empty(), "allowed:\n{}", fails.join("\n"));
}

#[test]
fn shape1_debug_trap_eval() {
    let pre = "trap 'eval \"$c\"' DEBUG; ";
    let f = assert_not_allow("s1-loop", &with_body(&format!("{pre}{LOOP}"), BODY, ""));
    // Control: cat does not assign $c, so the body is not evaluated; print only.
    verdicts("s1-cat-info", &with_body(&format!("{pre}{CAT}"), BODY, ""));
    finish(f);
}

#[test]
fn shape2_xtrace_ps4() {
    let head = "set -x; while IFS= read -r PS4; do echo \"$PS4\"; done <<'EOF'";
    let mut f = assert_not_allow("s2-loop-cs", &with_body(head, PS4_BODY, ""));
    f.extend(assert_not_allow(
        "s2-loop-rm",
        &with_body(head, "$(rm -rf ~)", ""),
    ));
    // Control: cat does not assign PS4; print only.
    verdicts(
        "s2-cat-info",
        &with_body(&format!("set -x; {CAT}"), PS4_BODY, ""),
    );
    finish(f);
}

#[test]
fn shape3_exec_stdout_to_bash() {
    let pre = "exec > >(bash); ";
    let mut f = assert_not_allow("s3-loop", &with_body(&format!("{pre}{LOOP}"), BODY, ""));
    f.extend(assert_not_allow(
        "s3-cat-control",
        &with_body(&format!("{pre}{CAT}"), BODY, ""),
    ));
    finish(f);
}

#[test]
fn shape4a_brace_group_to_script() {
    let tail = "\n} > x.sh";
    let mut f = assert_not_allow("s4a-loop", &with_body(&format!("{{ {LOOP}"), BODY, tail));
    f.extend(assert_not_allow(
        "s4a-cat-control",
        &with_body(&format!("{{ {CAT}"), BODY, tail),
    ));
    finish(f);
}

#[test]
fn shape4b_brace_group_to_script_then_run() {
    let tail = "\n} > x.sh && bash x.sh";
    let mut f = assert_not_allow("s4b-loop", &with_body(&format!("{{ {LOOP}"), BODY, tail));
    f.extend(assert_not_allow(
        "s4b-cat-control",
        &with_body(&format!("{{ {CAT}"), BODY, tail),
    ));
    finish(f);
}

#[test]
fn shape5_subshell_piped_to_bash() {
    let tail = "\n) | bash";
    let mut f = assert_not_allow("s5-loop", &with_body(&format!("( {LOOP}"), BODY, tail));
    f.extend(assert_not_allow(
        "s5-cat-control",
        &with_body(&format!("( {CAT}"), BODY, tail),
    ));
    finish(f);
}

#[test]
fn shape6_nested_compounds_informational() {
    for (label, head) in [
        (
            "s6-dbracket",
            "while IFS= read -r c; do [[ -n $c ]] && echo \"$c\"; done <<'EOF'",
        ),
        (
            "s6-arith",
            "while IFS= read -r c; do (( 1 )); echo \"$c\"; done <<'EOF'",
        ),
    ] {
        verdicts(label, &with_body(head, BODY, ""));
    }
}
