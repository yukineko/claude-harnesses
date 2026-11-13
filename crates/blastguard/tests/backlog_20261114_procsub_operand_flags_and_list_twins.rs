#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-14 by the flow orchestrator probing target/debug/blastguard
//! 0.2.531 at 9715946d; same verdicts at base 0e726e66), written by an independent
//! author, not the implementer of 6df87a5c.
//!
//! RED 1 (system-harm fail-open): a shell fed `chmod 777 /etc/hosts` through a
//! process substitution that is a NON-first operand is Allow in both modes:
//! `bash <(echo ...) foo` (script args), `bash -x <(...)`, `source <(...) foo`,
//! `bash -o pipefail <(...)`, `bash -O extglob <(...)`, `bash --rcfile f <(...)`
//! (value-taking flags), and `bash <&3 3< <(...)` (fd dup of a procsub-fed fd).
//!
//! RED 2 (false positive): `bash <(echo hi; echo there)` and
//! `bash < <(echo hi; echo there)` run a statically provable harmless list but are
//! now Ask (Deny in subagent mode); both were Allow at base.
//!
//! Asserted: RED 1 shapes are DENY, RED 2 shapes are ALLOW, main and subagent mode.
//! Controls (not ignored, currently pass): `bash <(echo 'chmod 777 /etc/hosts')` and
//! `bash 0< <(...)` are Deny; `bash < <(echo hi)`, `diff <(sort a) <(sort b)`,
//! `bash script.sh < <(echo y)` are Allow; `bash < <(cat f)` is not Allow.
//!
//! DOES NOT PROVE: other producers (`base64 -d`, `xxd -r`), other fd numbers or
//! redirect spellings, `eval`/`.` builtins, or that lists with other separators
//! (`&&`, newline) or non-echo members are judged correctly.

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
        "bg-procsubop-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "procsub-operand-20261114",
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

/// Run every shape in both modes; return the (shape x mode) runs whose decision
/// does not satisfy `ok`.
fn run_all(shapes: &[(&str, &str)], tag: &str, ok: impl Fn(&str) -> bool) -> Vec<String> {
    let home = unique_home(tag);
    let mut failures = Vec::new();
    for (label, c) in shapes {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            println!("[{label}] subagent={subagent} {c:?} -> {d} | {r}");
            if !ok(&d) {
                failures.push(format!("[{label}] subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    failures
}

const RED1_SHAPES: &[(&str, &str)] = &[
    (
        "bash-procsub-arg",
        "bash <(echo 'chmod 777 /etc/hosts') foo",
    ),
    (
        "bash-x-procsub-arg",
        "bash -x <(echo 'chmod 777 /etc/hosts') foo",
    ),
    (
        "source-procsub-arg",
        "source <(echo 'chmod 777 /etc/hosts') foo",
    ),
    (
        "bash-o-pipefail",
        "bash -o pipefail <(echo 'chmod 777 /etc/hosts')",
    ),
    (
        "bash-O-extglob",
        "bash -O extglob <(echo 'chmod 777 /etc/hosts')",
    ),
    (
        "bash-rcfile",
        "bash --rcfile f <(echo 'chmod 777 /etc/hosts')",
    ),
    (
        "bash-fd-dup",
        "bash <&3 3< <(echo 'chmod 777 /etc/hosts')",
    ),
];

const RED2_SHAPES: &[(&str, &str)] = &[
    ("list-operand", "bash <(echo hi; echo there)"),
    ("list-stdin", "bash < <(echo hi; echo there)"),
];

// test-weakening-justified: new RED for an unfixed system-harm fail-open
// (2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 procsub operand with args/value flags/fd dup is Allow — remove the ignore when fixed"]
fn procsub_operand_with_args_value_flags_or_fd_dup_is_denied() {
    let failures = run_all(RED1_SHAPES, "red1", |d| d == "deny");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) runs were NOT Deny:\n{}",
        failures.len(),
        RED1_SHAPES.len() * 2,
        failures.join("\n")
    );
}

// test-weakening-justified: new RED for an unfixed false positive (2026-11-14);
// the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 harmless static echo list in procsub fed to a shell is Ask/Deny, was Allow — remove the ignore when fixed"]
fn harmless_static_list_procsub_to_shell_is_allowed() {
    let failures = run_all(RED2_SHAPES, "red2", |d| d == "allow");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) runs were NOT Allow:\n{}",
        failures.len(),
        RED2_SHAPES.len() * 2,
        failures.join("\n")
    );
}

#[test]
fn control_plain_procsub_programs_are_denied() {
    let failures = run_all(
        &[
            ("operand", "bash <(echo 'chmod 777 /etc/hosts')"),
            ("fd0", "bash 0< <(echo 'chmod 777 /etc/hosts')"),
        ],
        "ctl-deny",
        |d| d == "deny",
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn control_harmless_procsubs_are_allowed() {
    let failures = run_all(
        &[
            ("stdin-echo", "bash < <(echo hi)"),
            ("diff", "diff <(sort a) <(sort b)"),
            ("script-stdin", "bash script.sh < <(echo y)"),
        ],
        "ctl-allow",
        |d| d == "allow",
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn control_unprovable_procsub_program_is_not_allowed() {
    let failures = run_all(&[("cat-f", "bash < <(cat f)")], "ctl-cat", |d| d != "allow");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
