// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier tests for condukt task e70b6a71 (blastguard 0.2.589:
//! a QUOTED-delimiter `git commit -F -` heredoc body is stripped from the
//! judged text even when commands follow the closing delimiter line).
//!
//! The fix rests on the claim that such a reader "stores the body nowhere".
//! Observed 2026-11-14 in a scratch repo: `git commit -F - <<'EOF'` writes the
//! body verbatim to `.git/COMMIT_EDITMSG` (and into the commit object), so a
//! later line in the same command can execute it (`sh .git/COMMIT_EDITMSG`,
//! `git log -1 --format=%B | sh`, ...). Each case below must not be Allow, in
//! both main-session and subagent modes.
//!
//! DOES NOT PROVE: anything about a LATER, separate tool call executing the
//! stored message (a documented residual), nor the exact Ask/Deny split.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn verdict(command: &str, subagent: bool) -> (String, String) {
    // VERIFY_E70B6A71_BIN: run the same cases against another build (the
    // verifier used it to record base eeaa73843 vs fix 1420ba8ee).
    let bin = std::env::var("VERIFY_E70B6A71_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_blastguard").to_string());
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    if subagent {
        payload["agent_id"] = serde_json::json!("agent-verify-e70b6a71");
        payload["agent_type"] = serde_json::json!("general-purpose");
    }
    let payload = payload.to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let d = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let r = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (d, r)
}

/// Runs every case in both modes, prints a table, and fails listing every
/// (case, mode) that came back Allow.
fn assert_none_allowed(cases: &[(&str, &str)]) {
    let mut allowed = Vec::new();
    for (name, cmd) in cases {
        for subagent in [false, true] {
            let (d, r) = verdict(cmd, subagent);
            let mode = if subagent { "sub" } else { "main" };
            eprintln!("CASE {name} [{mode}] => {d}: {r}");
            if d == "allow" {
                allowed.push(format!("{name} [{mode}]"));
            }
        }
    }
    assert!(allowed.is_empty(), "allowed: {allowed:?}");
}

// (a) Unquoted delimiter: the shell expands `...` / $(...) before git runs.
#[test]
fn a_unquoted_commit_heredoc_with_dangerous_substitution_and_later_command_is_not_allowed() {
    assert_none_allowed(&[
        (
            "a1_unquoted_backtick_later",
            "git commit -F - <<EOF\nfix: `rm -rf ~`\nEOF\ngit log -1",
        ),
        (
            "a2_unquoted_dollar_paren_later",
            "git commit -F - <<EOF\nfix: $(rm -rf ~)\nEOF\ngit log -1",
        ),
        (
            "a3_unquoted_backtick_no_later",
            "git commit -F - <<EOF\nfix: `rm -rf ~`\nEOF",
        ),
    ]);
}

// (b) A later dangerous command after the terminator stays judged.
#[test]
fn b_quoted_commit_heredoc_followed_by_dangerous_command_is_not_allowed() {
    assert_none_allowed(&[
        (
            "b1_later_rm_home",
            "git commit -F - <<'EOF'\nmsg `x`\nEOF\nrm -rf ~",
        ),
        (
            "b2_later_rm_after_benign",
            "git commit -F - <<'EOF'\nmsg `x`\nEOF\ngit log -1\nrm -rf ~",
        ),
        (
            "b3_double_quoted_delim_later_rm",
            "git commit -F - <<\"EOF\"\nmsg `x`\nEOF\nrm -rf ~",
        ),
        (
            "b4_backslash_delim_later_rm",
            "git commit -F - <<\\EOF\nmsg `x`\nEOF\nrm -rf ~",
        ),
    ]);
}

// (c) Unterminated quoted heredoc: the body is never closed, not exempted.
#[test]
fn c_unterminated_quoted_commit_heredoc_with_dangerous_body_is_not_allowed() {
    assert_none_allowed(&[
        (
            "c1_unterminated_backtick_rm",
            "git commit -F - <<'EOF'\nmsg `rm -rf ~`\n",
        ),
        (
            "c2_unterminated_then_rm_line",
            "git commit -F - <<'EOF'\nmsg\nrm -rf ~\nEOFX\necho done",
        ),
    ]);
}

// (d) The message IS stored (.git/COMMIT_EDITMSG, the commit object); a later
// line in the same command can execute it.
#[test]
fn d_later_line_executing_the_stored_commit_message_is_not_allowed() {
    const HEAD: &str = "git commit -F - <<'EOF'\nrm -rf ~\nEOF\n";
    let cases: Vec<(String, String)> = [
        ("d1_sh_commit_editmsg", "sh .git/COMMIT_EDITMSG"),
        ("d2_log_format_body_pipe_sh", "git log -1 --format=%B | sh"),
        ("d3_bash_stdin_commit_editmsg", "bash < .git/COMMIT_EDITMSG"),
        ("d4_source_commit_editmsg", "source .git/COMMIT_EDITMSG"),
        ("d5_dot_commit_editmsg", ". .git/COMMIT_EDITMSG"),
        (
            "d6_show_to_file_then_sh",
            "git show -s --format=%B HEAD > r.sh; sh r.sh",
        ),
        ("d7_bash_commit_editmsg", "bash .git/COMMIT_EDITMSG"),
        ("d8_cat_editmsg_pipe_sh", "cat .git/COMMIT_EDITMSG | sh"),
    ]
    .iter()
    .map(|(n, tail)| (n.to_string(), format!("{HEAD}{tail}")))
    .collect();
    let refs: Vec<(&str, &str)> = cases
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_str()))
        .collect();
    assert_none_allowed(&refs);
}

// (e) git readers other than `commit -F -` that write the body somewhere a
// later line can run it, and git global-option spellings.
#[test]
fn e_git_readers_that_store_the_body_followed_by_a_later_command_are_not_allowed() {
    assert_none_allowed(&[
        (
            "e3_hash_object_then_cat_file_sh",
            "git hash-object -w --stdin <<'EOF'\nrm -rf ~\nEOF\ngit cat-file -p HEAD | sh",
        ),
        (
            "e4_git_C_commit_then_sh_editmsg",
            "git -C x commit -F - <<'EOF'\nrm -rf ~\nEOF\nsh x/.git/COMMIT_EDITMSG",
        ),
        (
            "e5_git_c_opt_commit_then_sh_editmsg",
            "git -c core.hooksPath=/dev/null commit -F - <<'EOF'\nrm -rf ~\nEOF\nsh .git/COMMIT_EDITMSG",
        ),
        (
            "e6_commit_tail_then_later_sh",
            "git commit -F - <<'EOF' | cat\nrm -rf ~\nEOF\nsh .git/COMMIT_EDITMSG",
        ),
    ]);
}

// (e') `git apply` / `git am` write working-tree files from the body; a later
// `sh run.sh` runs them. Observed Allow at 1420ba8ee. NOT caused by that
// commit: [`here_document_consumer_is_data`] accepts only `git … commit -F -`,
// so these shapes never reach the changed `later_commands_ok`; the body
// line `+rm …` is simply not read as a command. Base eeaa73843 not measured
// (the test-rerun hook refused the run).
#[test]
#[ignore = "pre-existing fail-open (git apply/am heredoc then sh run.sh is Allow), unrelated to e70b6a71 — file as backlog"]
fn e_prime_git_apply_or_am_patch_then_running_the_written_file_is_not_allowed() {
    let patch = "diff --git a/run.sh b/run.sh\nnew file mode 100644\n--- /dev/null\n+++ b/run.sh\n@@ -0,0 +1 @@\n+rm -rf ~\n";
    let apply = format!("git apply <<'EOF'\n{patch}EOF\nsh run.sh");
    let am = format!("git am <<'EOF'\n{patch}EOF\nsh run.sh");
    assert_none_allowed(&[
        ("e1_git_apply_then_sh", apply.as_str()),
        ("e2_git_am_then_sh", am.as_str()),
    ]);
}

// Sanity: the target FP shape (benign later commands) is Allow at the fix, so
// the cases above are not passing merely because every later line is refused.
#[test]
fn sanity_quoted_commit_heredoc_with_backticks_and_benign_later_command_is_allowed() {
    for subagent in [false, true] {
        let (d, r) = verdict(
            "git commit -F - <<'EOF'\nfix: `blastguard log` output\nEOF\ngit log --oneline -1",
            subagent,
        );
        eprintln!("SANITY [{subagent}] => {d}: {r}");
        assert_eq!(d, "allow", "subagent={subagent}: {r}");
    }
}
