#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent boundary tests (2026-11-19) for two blastguard false-positive
//! fixes in 0.2.731 (fc304ce5c), written by a tester that did not implement
//! them:
//!
//!   * a0551e29 (`readings/static_bindings.rs`,
//!     `segment::assignment_execution_is_guarded_only_by_and_or`): a literal
//!     assignment after `&&` is bound for later segments of its own `&&`
//!     chain only. Claimed: `||`, `;`, newline, `|`, `&` drop the binding;
//!     `eval` / `source` / `.` clear it; a later non-plain mention of the
//!     name drops it; an unclosed here-doc, an unmodelled compound and an
//!     earlier brace group keep refusing.
//!   * ebbd1074 (`detect/redirect.rs`, `redirect_word_end`): an unquoted
//!     backslash escapes the next byte inside a redirect target word, in the
//!     `>`, `>>` and `>&` scans.
//!
//! Restrictive cases assert: NOT Allow, and the SAME verdict (main and
//! subagent mode) as the unbound `$b` / `$d` twin. Harm cases assert Deny.
//!
//! FAIL-OPENS FOUND. FIXED (ignore removed, RED observed then GREEN):
//!   * `a=x || b=README && git add -- $b` and `true || b=README && git add --
//!     $b` were Allow: the chain map bound an assignment that a `||` right
//!     BEFORE it may skip (`(true || b=…) && git add` runs git add without
//!     ever assigning `b`). Introduced by a0551e29 (unbound is Ask).
//!   * harm form of the same hole: `true || d=tests && rm -rf -- $d` was Ask
//!     in main mode, while the unbound `rm -rf -- $d` is Deny.
//!
//! STILL OPEN (kept as `#[ignore = "RED: …"]`):
//!   * `echo x > a\ b` / `>& a\ b` onto an EXISTING untracked file `a b`,
//!     `echo x > \a` onto existing `a`, and `cat <<'EOF' > out\).txt` onto
//!     existing `out).txt` are Allow: the word extent is right now, but the
//!     existence check uses the escaped spelling, which names no file.
//!   * `echo x > "a b"` (quoted, existing file) is Allow — not caused by
//!     ebbd1074, recorded because it is the same class.
//!
//! DOES NOT PROVE: any binding shape other than the literal ones spelled
//! here; `${b}` / `"$b"` references; `git commit`; other destructive
//! commands than `rm -rf`; redirect forms other than `>`, `>>`, `>&`; the
//! `>|` noclobber form (observed Deny with an empty target name — a separate
//! issue, not asserted here).

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    proj: PathBuf,
    plain: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "git {args:?} failed");
}

/// `proj`: a git repo with committed `README` and an untracked
/// `tests/new_fp.rs`. `plain`: a directory outside any git work tree holding
/// existing files `a b`, `a` and `out).txt`.
fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "fp-fix-boundaries-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    let proj = base.join("src").join("proj");
    std::fs::create_dir_all(proj.join("tests")).unwrap();
    std::fs::write(proj.join("README"), "x\n").unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-q", "-m", "init"]);
    std::fs::write(proj.join("tests/new_fp.rs"), "// x\n").unwrap();
    let plain = base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("a b"), "precious\n").unwrap();
    std::fs::write(plain.join("a"), "precious\n").unwrap();
    std::fs::write(plain.join("out).txt"), "precious\n").unwrap();
    Fx { base, proj, plain }
}

fn decision(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "fp-fix-boundaries-20261119",
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
        .env("HOME", fx.base.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(out.status.success(), "hook exited {:?}", out.status);
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

const UNBOUND_ADD: &str = "git add -- $b";

/// `command` (run in the git project) is not Allow, and in both modes has
/// the same verdict as `twin` (the unbound form).
fn same_as_unbound(command: &str, twin: &str) {
    let fx = fixture("restrict");
    for subagent in [false, true] {
        let (d, r) = decision(&fx, &fx.proj, command, subagent);
        let (td, tr) = decision(&fx, &fx.proj, twin, subagent);
        assert_ne!(d, "allow", "subagent={subagent}: {command:?} was allowed");
        assert_ne!(
            td, "allow",
            "anti-vacuity: unbound twin {twin:?} allowed ({tr})"
        );
        assert_eq!(
            d, td,
            "subagent={subagent}: {command:?} got {d} ({r}) but unbound twin {twin:?} got {td} ({tr})"
        );
    }
}

/// `command` has the same verdict as `literal` in both modes (used where the
/// binding is genuinely determined by the text).
fn same_as_literal(command: &str, literal: &str) {
    let fx = fixture("literal");
    for subagent in [false, true] {
        let (d, r) = decision(&fx, &fx.proj, command, subagent);
        let (ld, lr) = decision(&fx, &fx.proj, literal, subagent);
        assert_eq!(
            d, ld,
            "subagent={subagent}: {command:?} got {d} ({r}) but literal {literal:?} got {ld} ({lr})"
        );
    }
}

fn expect_deny(cwd_plain: bool, command: &str, reason_has: Option<&str>) {
    let fx = fixture("deny");
    let cwd = if cwd_plain { &fx.plain } else { &fx.proj };
    for subagent in [false, true] {
        let (d, r) = decision(&fx, cwd, command, subagent);
        assert_eq!(d, "deny", "subagent={subagent}: {command:?} got {d} ({r})");
        if let Some(needle) = reason_has {
            assert!(
                r.contains(needle),
                "subagent={subagent}: {command:?} reason does not name {needle:?}: {r}"
            );
        }
    }
}

// ---------------------------------------------------------------- a0551e29
// Restrictive boundaries: the binding must NOT reach the git add.

/// `;` after a guarded assignment: `b` may or may not have been assigned.
#[test]
fn semicolon_after_guarded_assignment_drops_binding() {
    same_as_unbound("false && b=README; git add -- $b", UNBOUND_ADD);
}

/// Same, with a true guard: still not a text fact past `;`.
#[test]
fn semicolon_after_and_chain_assignment_drops_binding() {
    same_as_unbound("a=x && b=README; git add -- $b", UNBOUND_ADD);
}

/// `||` after the assignment: git add runs exactly when the left side FAILED.
#[test]
fn or_after_assignment_drops_binding() {
    same_as_unbound("a=x && b=README || true && git add -- $b", UNBOUND_ADD);
}

/// FAIL-OPEN (a0551e29): a `||` right BEFORE the assignment. The list is
/// `(a=x || b=README) && git add -- $b`; `a=x` succeeds, so `b=README` is
/// skipped and git add runs with whatever `$b` the environment holds.
#[test]
fn or_before_assignment_does_not_bind() {
    same_as_unbound("a=x || b=README && git add -- $b", UNBOUND_ADD);
}

/// FAIL-OPEN (a0551e29): `true || b=README` NEVER runs the assignment.
#[test]
fn never_run_assignment_after_or_does_not_bind() {
    same_as_unbound("true || b=README && git add -- $b", UNBOUND_ADD);
}

/// Pipeline stage: the assignment runs in a subshell.
#[test]
fn pipeline_stage_assignment_drops_binding() {
    same_as_unbound("b=README | cat && git add -- $b", UNBOUND_ADD);
}

/// Background job: the assignment runs in a subshell.
#[test]
fn background_assignment_drops_binding() {
    same_as_unbound("b=README & git add -- $b", UNBOUND_ADD);
}

/// A newline after a guarded assignment ends its and-or list.
#[test]
fn newline_after_guarded_assignment_drops_binding() {
    same_as_unbound("true && b=README\ngit add -- $b", UNBOUND_ADD);
}

/// `eval` inside the chain may reassign anything: binding cleared.
#[test]
fn eval_in_chain_clears_binding() {
    same_as_unbound("b=README && eval 'b=/' && git add -- $b", UNBOUND_ADD);
}

/// `source` inside the chain clears the binding.
#[test]
fn source_in_chain_clears_binding() {
    same_as_unbound("b=README && source ./x && git add -- $b", UNBOUND_ADD);
}

/// `.` inside the chain clears the binding.
#[test]
fn dot_in_chain_clears_binding() {
    same_as_unbound("b=README && . ./x && git add -- $b", UNBOUND_ADD);
}

/// Non-plain mention `+=`: `b` is `README/x` now, not `README`.
#[test]
fn append_assignment_in_chain_drops_binding() {
    same_as_unbound("b=README && b+=/x && git add -- $b", UNBOUND_ADD);
}

/// Non-plain mention `read b`.
#[test]
fn read_in_chain_drops_binding() {
    same_as_unbound("b=README && read b && git add -- $b", UNBOUND_ADD);
}

/// An earlier brace group keeps refusing.
#[test]
fn earlier_brace_group_keeps_refusing() {
    same_as_unbound("{ true; } && b=README && git add -- $b", UNBOUND_ADD);
}

/// An earlier unmodelled compound keeps refusing.
#[test]
fn earlier_compound_keeps_refusing() {
    same_as_unbound(
        "if true; then :; fi && b=README && git add -- $b",
        UNBOUND_ADD,
    );
}

/// An unclosed here-document opener keeps refusing.
#[test]
fn unclosed_heredoc_keeps_refusing() {
    same_as_unbound("cat <<EOF && b=README && git add -- $b", UNBOUND_ADD);
}

// Determined cases: the binding IS a text fact; verdict equals the literal.

/// `b=README; false || git add -- $b`: `b=README` is unconditional (first
/// command of its own list), and git add runs because `false` fails, with
/// `b` = README. This is the pre-existing unconditional binding, not the
/// chain map; the right verdict is the literal twin's.
#[test]
fn unconditional_assignment_then_or_list_matches_literal() {
    same_as_literal(
        "b=README; false || git add -- $b",
        "false || git add -- README",
    );
}

/// `eval` before a LATER literal reassignment: `b=README` re-binds after the
/// eval, so `$b` is README at git add. The twin keeps the same shape
/// (including the assignment segment) because the eval verdict itself
/// depends on it: observed 2026-11-19, `a=x && eval 'b=/' && git add --
/// README` is Ask ("`eval` runs shell code built at run time … Table B.5"),
/// while the same line with a `b=README &&` segment is Allow — an eval
/// inconsistency outside a0551e29, not asserted here.
#[test]
fn reassignment_after_eval_matches_literal() {
    same_as_literal(
        "a=x && eval 'b=/' && b=README && git add -- $b",
        "a=x && eval 'b=/' && b=README && git add -- README",
    );
}

/// `&&` followed by a newline is a continuation, not a list end.
#[test]
fn and_then_newline_is_continuation_matches_literal() {
    same_as_literal("b=README &&\ngit add -- $b", "git add -- README");
}

// Harm: a binding must still be judged for harm.

#[test]
fn chain_bound_system_dir_rm_is_deny() {
    expect_deny(false, "d=/etc && rm -rf -- $d", Some("/etc"));
}

#[test]
fn chain_bound_system_dir_after_and_rm_is_deny() {
    expect_deny(false, "a=x && d=/etc && rm -rf $d", Some("/etc"));
}

#[test]
fn chain_bound_home_rm_is_deny() {
    expect_deny(false, "a=x && d=~ && rm -rf $d", None);
}

#[test]
fn chain_bound_root_rm_is_deny() {
    expect_deny(false, "true && d=/ && rm -rf -- $d", None);
}

/// Control: the unbound form is Deny.
#[test]
fn unbound_rm_is_deny() {
    expect_deny(false, "rm -rf -- $d", None);
}

/// FAIL-OPEN, harm form (a0551e29): `d=tests` never runs, so `$d` is the
/// environment's; the unbound form is Deny but this is Ask in main mode.
#[test]
fn never_run_assignment_rm_matches_unbound_deny() {
    expect_deny(false, "true || d=tests && rm -rf -- $d", None);
}

// ---------------------------------------------------------------- ebbd1074

/// The escaped word is read whole and judged: system-dir target, Deny,
/// and the reason names the escaped word, not its prefix `/etc/a`.
#[test]
fn escaped_space_truncating_redirect_names_whole_word() {
    expect_deny(true, "echo x > /etc/a\\ b", Some("/etc/a\\ b"));
}

#[test]
fn escaped_space_append_redirect_names_whole_word() {
    expect_deny(true, "echo x >> /etc/a\\ b", Some("/etc/a\\ b"));
}

#[test]
fn escaped_space_amp_redirect_names_whole_word() {
    expect_deny(true, "echo x >& /etc/a\\ b", Some("/etc/a\\ b"));
}

#[test]
fn heredoc_escaped_paren_redirect_names_whole_word() {
    expect_deny(
        true,
        "cat <<'EOF' > /etc/a\\).txt\nhi\nEOF",
        Some("/etc/a\\).txt"),
    );
}

fn expect_not_allow_naming(command: &str, needles: &[&str]) {
    let fx = fixture("redir");
    for subagent in [false, true] {
        let (d, r) = decision(&fx, &fx.plain, command, subagent);
        assert_ne!(d, "allow", "subagent={subagent}: {command:?} was allowed");
        if !subagent {
            assert!(
                needles.iter().any(|n| r.contains(n)),
                "{command:?} reason names none of {needles:?}: {r}"
            );
        }
    }
}

/// Control: the plain twin `echo x > a` onto existing untracked `a` is not
/// Allow (main: Ask confined; subagent: Deny).
#[test]
fn control_plain_existing_target_not_allowed() {
    expect_not_allow_naming("echo x > a", &["(a)", "'> a'"]);
}

/// FAIL-OPEN: existing untracked `a b`, escaped spelling.
#[test]
#[ignore = "RED: `echo x > a\\ b` onto an existing untracked file `a b` is Allow; the existence check uses the escaped spelling"]
fn escaped_space_onto_existing_file_not_allowed() {
    expect_not_allow_naming("echo x > a\\ b", &["a b", "a\\ b"]);
}

/// FAIL-OPEN: same through `>&`.
#[test]
#[ignore = "RED: `echo x >& a\\ b` onto an existing untracked file `a b` is Allow"]
fn escaped_space_amp_onto_existing_file_not_allowed() {
    expect_not_allow_naming("echo x >& a\\ b", &["a b", "a\\ b"]);
}

/// FAIL-OPEN: `\a` is `a`, which exists.
#[test]
#[ignore = "RED: `echo x > \\a` onto an existing untracked file `a` is Allow"]
fn escaped_letter_onto_existing_file_not_allowed() {
    expect_not_allow_naming("echo x > \\a", &["(a)", "\\a"]);
}

/// FAIL-OPEN: here-doc to `out\).txt`, i.e. existing `out).txt`.
#[test]
#[ignore = "RED: `cat <<'EOF' > out\\).txt` onto an existing untracked `out).txt` is Allow"]
fn heredoc_escaped_paren_onto_existing_file_not_allowed() {
    expect_not_allow_naming(
        "cat <<'EOF' > out\\).txt\nhi\nEOF",
        &["out).txt", "out\\).txt"],
    );
}

/// FAIL-OPEN (pre-existing, not ebbd1074): quoted target with a space.
#[test]
#[ignore = "RED: `echo x > \"a b\"` onto an existing untracked file `a b` is Allow (quoted target with a space; not caused by ebbd1074)"]
fn quoted_space_onto_existing_file_not_allowed() {
    expect_not_allow_naming("echo x > \"a b\"", &["a b"]);
}
