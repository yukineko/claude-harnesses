#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-20 on this tree, blastguard
//! 0.2.744; first reported 2026-11-19 on 0.2.738 at main 5a27f8ecd): a `cp` whose
//! destination reaches /etc through `..` segments is Allow, in a loop and
//! without one. Fixture: hermetic HOME, cwd = HOME/wt/a/b/c/d, `..` chain sized
//! to the cwd's canonical depth + 2 so it always reaches `/`.
//!   for f in ../..(to /)../etc; do cp a $f/hosts; done   -> allow   (RED)
//!   cp a ../..(to /)../etc/hosts                          -> allow   (RED)
//!   for f in <same>/etc; do echo x > $f/hosts; done       -> deny    (control)
//!   echo x > <same>/etc/hosts                             -> deny    (control)
//!   for f in /etc; do cp a $f/hosts; done                 -> deny    (control)
//!   for f in /etc/hosts; do echo x > $f; done             -> deny    (control)
//!   cd /; for f in etc; do echo x > $f/hosts; done        -> deny    (control)
//! Depth sweep (this fixture, canonical depth 12): the redirect forms deny from
//! 11 `..` (reaches /private/etc) up and are Allow below that, correctly (they
//! land in /private/var/...); the `cp` forms are Allow at every count 11..14.
//!
//! The 2026-11-19 report also listed the loop REDIRECT form as Allow with 10
//! `..`. Not reproduced: with a chain that reaches `/` it is Deny; 10 `..` from a
//! cwd deeper than 10 components does not reach /etc (reading (c): fixture, not
//! a defect), so that form is kept only as a non-ignored control.
//!
//! Readings for the cp Allow: (a) real defect: the `cp` destination is not
//! resolved against the cwd before the system-directory check, while the
//! redirect target is; (b) intended: `cp` destinations are judged only when
//! absolute. Tell apart: (b) would also allow `cp a /etc/hosts`, but the
//! absolute loop form denies, and a destination that resolves into /etc is system
//! harm however it is spelled (standing ruling: harmful targets are Deny through
//! any wrapper), so (a).
//!
//! DOES NOT PROVE: other writers (`mv`, `tee`, `install`, `dd of=`) behave the
//! same, other loop-variable spellings, the subagent verdict, or the verdict
//! of a relative path that reaches /etc without `..` (not probed).
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-dotdot-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Fixture: cwd = HOME/wt/a/b/c/d, and a `..` chain as long as the cwd's
/// canonical depth + 2, so it reaches `/` whatever the temp dir is.
fn deep_cwd_and_dots(home: &Home) -> (std::path::PathBuf, String) {
    let deep = home.0.join("wt").join("a").join("b").join("c").join("d");
    std::fs::create_dir_all(&deep).unwrap();
    let real = deep.canonicalize().unwrap();
    let depth = real.components().count() - 1;
    let dots = vec![".."; depth + 2].join("/");
    (deep, dots)
}

fn run_in(command: &str, home: &Home, cwd: &std::path::Path) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "dotdot-20261120",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
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

fn verdict(tag: &str, build: impl Fn(&str) -> String) -> (String, String, String) {
    let home = unique_home(tag);
    let (deep, dots) = deep_cwd_and_dots(&home);
    let cmd = build(&format!("{dots}/etc"));
    let (m, r) = run_in(&cmd, &home, &deep);
    (m, r, cmd)
}

#[test]
fn dotdot_loop_value_cp_into_etc_is_denied() {
    let (m, r, cmd) = verdict("cp-loop", |p| {
        format!("for f in {p}; do cp a $f/hosts; done")
    });
    assert_eq!(m, "deny", "writes /etc/hosts; command={cmd} reason={r}");
}

#[test]
fn dotdot_cp_into_etc_no_loop_is_denied() {
    let (m, r, cmd) = verdict("cp-plain", |p| format!("cp a {p}/hosts"));
    assert_eq!(m, "deny", "writes /etc/hosts; command={cmd} reason={r}");
}

#[test]
fn control_dotdot_loop_value_redirect_is_denied() {
    let (m, r, cmd) = verdict("c-redir", |p| {
        format!("for f in {p}; do echo x > $f/hosts; done")
    });
    assert_eq!(m, "deny", "command={cmd} reason={r}");
}

#[test]
fn control_dotdot_redirect_no_loop_is_denied() {
    let (m, r, cmd) = verdict("c-redir-plain", |p| format!("echo x > {p}/hosts"));
    assert_eq!(m, "deny", "command={cmd} reason={r}");
}

#[test]
fn control_absolute_loop_value_cp_is_denied() {
    let (m, r, cmd) = verdict("c-abs-cp", |_| {
        "for f in /etc; do cp a $f/hosts; done".into()
    });
    assert_eq!(m, "deny", "command={cmd} reason={r}");
}

#[test]
fn control_absolute_loop_value_redirect_is_denied() {
    let (m, r, cmd) = verdict("c-abs", |_| {
        "for f in /etc/hosts; do echo x > $f; done".into()
    });
    assert_eq!(m, "deny", "command={cmd} reason={r}");
}

#[test]
fn control_cd_root_relative_loop_value_redirect_is_denied() {
    let (m, r, cmd) = verdict("c-cd", |_| {
        "cd /; for f in etc; do echo x > $f/hosts; done".into()
    });
    assert_eq!(m, "deny", "command={cmd} reason={r}");
}

#[test]
fn control_loop_dotdot_value_inside_tree_is_not_denied() {
    let (m, r, cmd) = verdict("c-in", |_| {
        "for f in ../../x; do cp a $f/hosts; done".into()
    });
    assert_ne!(
        m, "deny",
        "stays inside the scratch tree; command={cmd} reason={r}"
    );
}
