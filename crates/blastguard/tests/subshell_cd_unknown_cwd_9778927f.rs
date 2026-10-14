#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 9778927f: a `cd` / `pushd` / `popd` inside a subshell group
//! `( ... )` runs in a CHILD shell. After the closing `)` the real cwd is the
//! outer one, so blastguard must not carry the inner directory past the `)`.
//! Decided design: when a subshell group contains a cwd change, the cwd after
//! its `)` is UNKNOWN, so a later RELATIVE write/delete is judged as an
//! unknown-cwd operand -- the verdict must NOT be allow (ask or deny are both
//! acceptable). Only verdicts are tested here, never the mechanism.
//!
//! Controls pin that a subshell WITHOUT a cwd change keeps the outer cwd, that
//! benign subshells stay allowed, and that a `cd` still applies INSIDE its own
//! subshell.
//!
//! These tests only pass command STRINGS to the blastguard hook on stdin; no
//! command under test is ever executed.
//!
//! Anti-vacuity: an empty stdout counts as "allow" ONLY when the binary exited
//! with status 0. Any non-zero exit (crash, panic) or unparseable stdout is
//! reported as a violation for EVERY expectation, including Allow controls, so
//! a crashing binary can never be mistaken for a clean allow nor for a block.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_blastguard")
}

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home() -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-subshell-cd-9778927f-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Outcome of one hook invocation.
enum Outcome {
    /// Exit 0 and a decision (`allow` only for empty stdout or an explicit allow).
    Decided { decision: String, reason: String },
    /// Anything that is not a well-formed judgement: non-zero exit, signal,
    /// unparseable stdout. Never counts as allow nor as a block.
    Broken(String),
}

/// Run the hook as an INTERACTIVE session (an Ask stays an Ask).
fn verdict(command: &str) -> Outcome {
    let home = unique_home();
    let payload = serde_json::json!({
        "session_id": "subshell-cd-9778927f",
        "cwd": env!("CARGO_MANIFEST_DIR"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string();
    let mut child = Command::new(bin())
        .env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Outcome::Broken(format!(
            "binary exit status {:?} (not 0); stdout={:?} stderr={:?}",
            out.status.code(),
            stdout.trim(),
            stderr.trim()
        ));
    }
    if stdout.trim().is_empty() {
        // Only reachable with exit status 0 (checked above).
        return Outcome::Decided {
            decision: "allow".into(),
            reason: String::new(),
        };
    }
    let v: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => {
            return Outcome::Broken(format!("unparseable stdout ({e}): {:?}", stdout.trim()));
        }
    };
    let h = &v["hookSpecificOutput"];
    match h["permissionDecision"].as_str() {
        Some(d) => Outcome::Decided {
            decision: d.to_string(),
            reason: h["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        },
        None => Outcome::Broken(format!(
            "no permissionDecision in stdout: {:?}",
            stdout.trim()
        )),
    }
}

#[derive(Clone, Copy)]
enum Expect {
    Deny,
    NotAllow,
    Allow,
}

/// Judge every case and panic once with ALL violations listed.
fn check_all(cases: &[(&str, Expect)]) {
    assert!(!cases.is_empty(), "check_all called with no cases");
    let mut violations = Vec::new();
    for (cmd, expect) in cases {
        let want = match expect {
            Expect::Deny => "deny",
            Expect::NotAllow => "not allow (ask|deny)",
            Expect::Allow => "allow",
        };
        match verdict(cmd) {
            Outcome::Broken(why) => {
                violations.push(format!("  `{cmd}`: expected {want}, got BROKEN: {why}"));
            }
            Outcome::Decided { decision, reason } => {
                let ok = match expect {
                    Expect::Deny => decision == "deny",
                    Expect::NotAllow => decision == "ask" || decision == "deny",
                    Expect::Allow => decision == "allow",
                };
                if !ok {
                    violations.push(format!(
                        "  `{cmd}`: expected {want}, got decision={decision} reason={reason:?}"
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

/// For each `(subject, reference)` pair, the subject's decision must equal the
/// reference's decision. A broken invocation on EITHER side is a violation, so
/// two crashes can never compare equal.
fn check_same_decision(pairs: &[(&str, &str)]) {
    assert!(
        !pairs.is_empty(),
        "check_same_decision called with no pairs"
    );
    let mut violations = Vec::new();
    for (subject, reference) in pairs {
        let s = verdict(subject);
        let r = verdict(reference);
        match (s, r) {
            (
                Outcome::Decided {
                    decision: sd,
                    reason: sr,
                },
                Outcome::Decided {
                    decision: rd,
                    reason: rr,
                },
            ) => {
                if sd != rd {
                    violations.push(format!(
                        "  `{subject}`: decision={sd} reason={sr:?}\n    but reference `{reference}`: decision={rd} reason={rr:?}"
                    ));
                }
            }
            (Outcome::Broken(why), _) => {
                violations.push(format!("  `{subject}`: BROKEN: {why}"));
            }
            (_, Outcome::Broken(why)) => {
                violations.push(format!("  reference `{reference}`: BROKEN: {why}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

/// Sanity that the harness can observe a deny at all, and that the outer-cwd
/// baselines used below are themselves denied.
#[test]
fn harness_observes_deny_for_plain_outer_cd() {
    check_all(&[
        ("rm -rf /usr/lib", Expect::Deny),
        ("cd /etc && echo x > hosts", Expect::Deny),
    ]);
}

/// Spaced, glued, `;`-continued and newline-separated subshell forms: a cd in
/// the group makes the cwd after `)` unknown.
#[test]
fn subshell_cd_forms_make_cwd_unknown_after_close() {
    check_all(&[
        ("cd /etc && ( cd /tmp ) && echo x > hosts", Expect::NotAllow),
        ("cd /etc && (cd /tmp) && echo x > hosts", Expect::NotAllow),
        (
            "cd /etc && ( cd /tmp; true ) && echo x > hosts",
            Expect::NotAllow,
        ),
        (
            "cd /etc && (\ncd /tmp\n) && echo x > hosts",
            Expect::NotAllow,
        ),
    ]);
}

/// Inner directory SAFER than the outer one: the inner cd must not launder a
/// relative write/delete in the outer (dangerous) directory.
#[test]
fn inner_safer_dir_does_not_launder_outer_relative_ops() {
    check_all(&[
        ("cd /etc && ( cd /tmp ) && echo x > hosts", Expect::NotAllow),
        ("cd /usr && ( cd /tmp ) && rm -rf lib", Expect::NotAllow),
        (
            "cd .githooks && ( cd /tmp ) && rm pre-commit",
            Expect::NotAllow,
        ),
    ]);
}

/// Inner directory MORE dangerous than the outer one: still unknown after `)`.
#[test]
fn inner_dangerous_dir_still_unknown_after_close() {
    check_all(&[
        ("( cd /etc ) && echo x > hosts", Expect::NotAllow),
        ("cd /tmp && (cd /usr) && rm -rf lib", Expect::NotAllow),
    ]);
}

/// pushd inside a subshell is a cwd change too.
#[test]
fn pushd_inside_subshell_makes_cwd_unknown_after_close() {
    check_all(&[(
        "cd /etc && ( pushd /tmp ) && echo x > hosts",
        Expect::NotAllow,
    )]);
}

/// Frozen existing behaviour.
#[test]
fn frozen_githooks_subshell_cd_then_rm_is_not_allowed() {
    check_all(&[("(cd .githooks) && rm pre-commit", Expect::NotAllow)]);
}

/// Control: a subshell WITHOUT a cwd change keeps the outer cwd.
#[test]
fn control_subshell_without_cd_keeps_outer_cwd() {
    check_all(&[("cd /etc && ( echo hi ) && echo x > hosts", Expect::Deny)]);
    check_same_decision(&[
        (
            "cd /etc && ( echo hi ) && echo x > hosts",
            "cd /etc && echo x > hosts",
        ),
        (
            "cd /tmp && ( echo hi ) && rm -rf lib",
            "cd /tmp && rm -rf lib",
        ),
    ]);
}

/// Control: benign subshell cds followed by read-only commands stay allowed.
#[test]
fn control_benign_subshell_cd_stays_allowed() {
    check_all(&[
        ("(cd sub && cargo test) && ls", Expect::Allow),
        ("(cd /tmp) && ls", Expect::Allow),
    ]);
}

/// Control: a cd INSIDE the subshell still applies inside it.
#[test]
fn control_cd_applies_inside_its_own_subshell() {
    check_same_decision(&[("(cd /tmp && rm -rf lib)", "cd /tmp && rm -rf lib")]);
}
