#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 9778927f (follow-up): after a subshell group `( ... )` that changed
//! directory closes, the real cwd is the OUTER one, so the cwd after the group
//! must be UNKNOWN and a later relative write must never be allowed.
//!
//! An adversarial verifier found shapes where the group boundary is
//! mis-tracked -- a `(` inside a comment, a here-doc body, `${x:-(}`, or a
//! nested-quoted command substitution, and redirects attached directly to the
//! group `( ... ) > file`. In bash every one of these writes to `/etc/hosts`.
//! Each must be judged NOT allow (ask or deny). Only verdicts are tested here,
//! never the mechanism.
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
        "bg-subshell-miscount-9778927f-{}-{}",
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
        "session_id": "subshell-miscount-9778927f",
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
                violations.push(format!("  {cmd:?}: expected {want}, got BROKEN: {why}"));
            }
            Outcome::Decided { decision, reason } => {
                let ok = match expect {
                    Expect::Deny => decision == "deny",
                    Expect::NotAllow => decision == "ask" || decision == "deny",
                    Expect::Allow => decision == "allow",
                };
                if !ok {
                    violations.push(format!(
                        "  {cmd:?}: expected {want}, got decision={decision} reason={reason:?}"
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

/// A `(` inside a comment or inside `${x:-(}` must not be counted as a group
/// opener that keeps the inner `cd /tmp` alive past the real `)`.
#[test]
fn paren_in_comment_or_param_expansion_does_not_hide_group_close() {
    check_all(&[
        (
            "cd /etc && ( cd /tmp\n# (\n) && echo x > hosts",
            Expect::NotAllow,
        ),
        (
            "cd /etc && ( cd /tmp; echo ${x:-(} ) && echo x > hosts",
            Expect::NotAllow,
        ),
        (
            "cd /etc && ( cd /tmp\necho ${x:-(}\n) && echo x > hosts",
            Expect::NotAllow,
        ),
        (
            "cd /etc && ( cd /tmp; true; echo ${x:-(}; ) && echo x > hosts",
            Expect::NotAllow,
        ),
    ]);
}

/// A `(` inside a here-doc body (quoted or unquoted delimiter) is data, not a
/// group opener.
#[test]
fn paren_in_heredoc_body_does_not_hide_group_close() {
    check_all(&[
        (
            "cd /etc && ( cd /tmp; cat <<E\n(\nE\n) && echo x > hosts",
            Expect::NotAllow,
        ),
        (
            "cd /etc && ( cd /tmp; cat <<'E'\n(\nE\n) && echo x > hosts",
            Expect::NotAllow,
        ),
        (
            "cd /etc && ( cd /tmp && cat <<'E'\n(\nE\n) && echo x > hosts",
            Expect::NotAllow,
        ),
    ]);
}

/// A `(` inside nested quotes within a command substitution is data.
#[test]
fn paren_in_nested_quoted_substitution_does_not_hide_group_close() {
    check_all(&[(
        "cd /etc && ( cd /tmp; echo \"$(echo \"(\")\" ) && echo x > hosts",
        Expect::NotAllow,
    )]);
}

/// A redirect attached to the group itself is opened by the OUTER shell, in
/// the outer cwd (/etc) -- not in the group's inner cwd.
#[test]
fn redirect_attached_to_group_is_not_judged_in_inner_cwd() {
    check_all(&[
        ("cd /etc && ( cd /tmp; true ) > hosts", Expect::NotAllow),
        ("cd /etc && ( cd /tmp; true ) >> hosts", Expect::NotAllow),
        ("cd /etc && ( cd /tmp; true ) > ./hosts", Expect::NotAllow),
        ("cd /etc && ( cd /tmp ; true ) 2> hosts", Expect::NotAllow),
    ]);
}

/// Group redirect with NO outer cd: the brief that commissioned this file
/// listed it as an already-holding control ("must be not allow"), but it was
/// observed ALLOW at 5f731056 (session cwd = this crate dir). Kept as its own
/// test, as specified, so the disagreement stays visible rather than being
/// resolved by judgment here.
#[test]
fn group_redirect_without_outer_cd_is_not_allowed() {
    check_all(&[("( cd /tmp ) > hosts", Expect::NotAllow)]);
}

/// Controls: the harness observes deny; a group without cd keeps the outer
/// cwd; benign subshell cds stay allowed.
#[test]
fn controls_hold() {
    check_all(&[
        ("cd /etc && echo x > hosts", Expect::Deny),
        ("cd /etc && ( true ) && echo x > hosts", Expect::Deny),
        ("(cd sub; make); touch stamp", Expect::Allow),
        ("(cd /tmp) && ls", Expect::Allow),
    ]);
}
