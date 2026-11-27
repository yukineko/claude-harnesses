#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-22 in a subagent of condukt run
//! run-20261122-052625-87637, main 7e0d6f24b..745450fc6): `f=<literal> && perl
//! -0pi -e '<program>' $f` is denied with "`perl` rewrites in place `$f`, whose
//! path depends on an expansion blastguard cannot resolve", although `f=` is a
//! literal assignment earlier in the same `&&` chain (memory: text-determined
//! values are resolved and judged statically). The original command was
//! `cd <wt>/crates/tdd/tests && f=backlog_..._undetermined.rs && perl -0pi -e
//! 's|...|...|s; ...' $f; sed -n 70,90p $f; ...` (relay bgr-f4a13c5381b6).
//!
//! What THIS source tree (main 745450fc6) does instead, observed by running the
//! tests below: BOTH the literal-path form (`perl -pi -e '...' m.json`) and the
//! `f=m.json && perl -pi ... $f` form are denied with "`perl`: option -pi not
//! modelled -- blastguard cannot judge what it would do (Table B.5:
//! Undetermined)". The reported "depends on an expansion" reason was NOT
//! reproduced here (it came from the installed binary of the session). So the
//! observed, asserted defect is: a repo-relative `perl -pi -e` in-place edit,
//! with a literal path or a literal-assigned variable, is denied in subagent
//! mode. The older control `control_perl_single_quoted_without_dollar_dot_is_allowed`
//! in backlog_20261114_perl_single_quoted_dollar_dot_fp.rs already asserts the
//! literal form and fails the same way at this rev.
//!
//! Readings: (a) FP -- an in-place substitution on a cwd file is judgeable;
//! (b) intended -- Table B.5 deliberately leaves perl option parsing
//! Undetermined and the old controls are stale. Not distinguished here.
//!
//! Controls (GREEN): a variable set from a command substitution is not Allow.
//!
//! DOES NOT PROVE: the exact original command (`-0pi`, `;`-joined `sed -n ...
//! $f`); main mode; the "depends on an expansion" reason.

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
        "bg-perlvar-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    std::fs::write(dir.join("wt/m.json"), "{\"version\": \"0.2.578\"}\n").unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "perl-literal-var-path-20261122",
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

#[test]
#[ignore = "RED(a92a82b1): subagent perl -pi in-place edit denied as option -pi not modelled"]
fn perl_in_place_literal_path_is_allowed() {
    let home = unique_home("ctl1");
    let c = "perl -pi -e 's/0\\.2\\.578/0.2.579/' m.json";
    let (d, r) = run_as(c, &home, true);
    assert_eq!(d, "allow", "{c:?} -> {d} | {r}");
}

#[test]
fn control_perl_var_from_cmdsub_is_not_allowed() {
    let home = unique_home("ctl2");
    let c = "f=$(echo /etc/passwd) && perl -0pi -e 's/x/y/' $f";
    let (d, r) = run_as(c, &home, true);
    assert_ne!(d, "allow", "{c:?} -> {d} | {r}");
}

#[test]
#[ignore = "RED(a92a82b1): subagent perl -pi in-place edit via literal-assigned variable denied as option -pi not modelled"]
fn perl_in_place_on_literal_assigned_variable_is_allowed() {
    let home = unique_home("red");
    let c = "f=m.json && perl -pi -e 's/0\\.2\\.578/0.2.579/' $f";
    let (d, r) = run_as(c, &home, true);
    assert_eq!(d, "allow", "{c:?} -> {d} | {r}");
}
