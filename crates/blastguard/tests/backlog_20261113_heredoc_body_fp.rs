#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 2026-11-13: two false positives on quoted-heredoc BODIES (data),
//! observed as verbatim denies on subagent Bash calls.
//!
//! FP-A: `cd <dir> && cat >> src/foo.rs <<'EOF' ... panic!("lock > t, {x}"); ... EOF && cargo test`
//!   was denied with "'> t,' is a RELATIVE redirect after an earlier cd/pushd/popd
//!   whose target directory blastguard could not statically resolve". The `>` is
//!   inside the quoted heredoc body (data), not a redirect.
//!   Asserted here: with a LITERAL, resolvable cd target, the decision must be
//!   "allow" (the real `cat >>` is a relative redirect to a plain cwd .rs file)
//!   and, whatever the decision, the reason must never quote the body text
//!   `> t,`. Controls: a genuinely unresolvable cd (`cd "$SOMEVAR"`) followed by
//!   a real relative redirect is still not allowed.
//!
//! FP-B: `python3 - <<'PY'` (quoted heredoc) whose Python program edits a cwd
//!   .rs file and contains the text `rm -rf *` inside a Python string literal
//!   was denied with "rm with a wildcard can delete many files at once [system
//!   harm ...]". Standing ruling: heredoc bodies fed to an interpreter in the
//!   same command are judged as that interpreter's language; a string literal
//!   holding rm text is data. Asserted: the decision equals the decision for the
//!   same program WITHOUT the rm text (control) and is not "deny". Controls: a
//!   real wildcard-rm shell command after the heredoc is still denied.

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
        "bg-hd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook as a subagent. Returns (decision, reason).
fn run(command: &str, cwd: &std::path::Path, home: &Home) -> (String, String) {
    run_as(command, cwd, home, true)
}

/// `subagent=false` is the main session (asks stay asks); `true` converts asks to denies.
fn run_as(command: &str, cwd: &std::path::Path, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "heredoc-20261113",
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

/// A cwd containing `proj/src/foo.rs`. Returns (home, cwd, proj dir).
fn setup(tag: &str) -> (Home, std::path::PathBuf, std::path::PathBuf) {
    let home = unique_home(tag);
    let cwd = home.0.join("wt");
    let proj = cwd.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("src/foo.rs"), "// MARK\nfn main() {}\n").unwrap();
    (home, cwd, proj)
}

// ---------------------------------------------------------------- FP-A

fn fp_a_cmd(cd_target: &str) -> String {
    format!(
        "cd {cd_target} && cat >> src/foo.rs <<'EOF'\n\
#[cfg(test)]\n\
mod tests {{\n\
    #[test]\n\
    fn lock_ordering() {{\n\
        let (a, t, x) = (2, 1, 0);\n\
        assert!(a > t, \"a must exceed t\");\n\
        if a > t, {{ }}\n\
        panic!(\"lock > t, {{x}}\");\n\
    }}\n\
}}\n\
EOF\n\
cargo test 2>&1 | tail -5"
    )
}

// Guard (GREEN at HEAD, not ignored): the FP-A deny does NOT reproduce when the
// cd target is a resolvable literal dir; it only reproduces with an unresolvable
// cd (see the RED test below).
#[test]
fn a_heredoc_body_gt_is_not_a_relative_redirect_after_resolvable_cd() {
    let (home, cwd, proj) = setup("a1");
    let (d, r) = run(&fp_a_cmd(proj.to_str().unwrap()), &cwd, &home);
    assert!(
        !r.contains("> t,"),
        "reason quotes heredoc BODY text as a redirect: {d} | {r}"
    );
    assert_eq!(
        d, "allow",
        "append of rust test code to cwd .rs file via quoted heredoc: {d} | {r}"
    );
}

// Body-text-only variant: with an unresolvable cd the deny of the real
// `cat >> src/foo.rs` redirect may be legitimate, but the reason must name the
// real redirect target, never quote body text `> t,` as if it were a redirect.
#[test]
#[ignore = "RED: blastguard FP 2026-11-13 A — remove the ignore when fixed"]
fn a_unresolvable_cd_reason_does_not_quote_heredoc_body_as_redirect() {
    let (home, cwd, _proj) = setup("a4");
    let (d, r) = run(&fp_a_cmd("\"$SOMEVAR\""), &cwd, &home);
    assert!(
        !r.contains("> t,"),
        "reason quotes heredoc BODY text as a redirect: {d} | {r}"
    );
}

// Control: the same shape with an UNRESOLVABLE cd and a REAL relative redirect
// (no heredoc body involved) must not be allowed.
#[test]
fn a_control_unresolvable_cd_then_real_relative_redirect_is_not_allowed() {
    let (home, cwd, _proj) = setup("a2");
    let cmd = "cd \"$SOMEVAR\" && echo hello > t.txt";
    let (d, r) = run(cmd, &cwd, &home);
    assert_ne!(
        d, "allow",
        "real relative redirect after unresolvable cd: {d} | {r}"
    );
}

// Control 2: unresolvable cd + `cat >> src/foo.rs <<'EOF'` (real redirect target
// is the relative path) is also not allowed, regardless of body content.
#[test]
fn a_control_unresolvable_cd_then_cat_append_heredoc_is_not_allowed() {
    let (home, cwd, _proj) = setup("a3");
    let (d, r) = run(&fp_a_cmd("\"$SOMEVAR\""), &cwd, &home);
    assert_ne!(
        d, "allow",
        "real `cat >> src/foo.rs` after unresolvable cd: {d} | {r}"
    );
}

// ---------------------------------------------------------------- FP-B

fn fp_b_cmd(doc_line: &str, tail: &str) -> String {
    format!(
        "python3 - <<'PY'\n\
p = 'src/foo.rs'\n\
s = open(p).read()\n\
doc = {doc_line}\n\
s = s.replace('// MARK', doc + '\\n// MARK')\n\
open(p, 'w').write(s)\n\
PY\n\
{tail}"
    )
}

const RM_DOC: &str = "'/// Clean up with `rm -rf *` before running.'";
const PLAIN_DOC: &str = "'/// Clean up before running.'";

#[test]
#[ignore = "RED: blastguard FP 2026-11-13 B — remove the ignore when fixed"]
fn b_python_string_literal_with_rm_text_is_data_not_a_command() {
    let (home, cwd, proj) = setup("b1");
    let (cd, cr) = run_as(&fp_b_cmd(PLAIN_DOC, "true"), &proj, &home, false);
    let (d, r) = run_as(&fp_b_cmd(RM_DOC, "true"), &proj, &home, false);
    let _ = &cwd;
    assert_eq!(
        d, cd,
        "rm text inside a python string literal changed the decision (control: {cd} | {cr}): {d} | {r}"
    );
    assert_ne!(d, "deny", "python string literal with rm text: {d} | {r}");
}

// Control: the same program is not denied without the rm text (so the RED above
// is attributable to the string content alone).
#[test]
fn b_control_same_program_without_rm_text_is_not_denied() {
    let (home, _cwd, proj) = setup("b2");
    let (d, r) = run_as(&fp_b_cmd(PLAIN_DOC, "true"), &proj, &home, false);
    assert_ne!(d, "deny", "program without rm text: {d} | {r}");
}

// Control: a REAL wildcard rm after the heredoc is still denied.
#[test]
fn b_control_real_wildcard_rm_after_heredoc_is_denied() {
    let (home, _cwd, proj) = setup("b3");
    let (d, r) = run_as(&fp_b_cmd(PLAIN_DOC, "rm -rf *"), &proj, &home, false);
    assert_eq!(d, "deny", "real wildcard rm after heredoc: {d} | {r}");
}
