#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial tests (independent author, 2026-11-20) for fix 807706054
//! (blastguard 0.2.777, backlog 3ab6109f): a QUOTED `git commit -F - <<'EOF'`
//! body is data when the only undetermined words of the opener are the VALUE
//! of `--trailer` / `--author` and that value runs no command.
//!
//! What these tests assert (decisions only, never message text):
//! * every other undetermined word keeps the body judged, so a body naming a
//!   protected path (or holding `rm -rf /etc`) is not Allow: a bare `$X`
//!   operand, `-F "$X"`, `-m "$X"`, `--date`, `-c`, `-C`, `--template`,
//!   `--file "$X"`, an undetermined subcommand, `$( )` / backquote / `<( )`
//!   values, wrappers (`command git`, `env git`, `/usr/bin/git`) and a `git`
//!   function redefinition;
//! * a later command that runs the stored message (`sh .git/COMMIT_EDITMSG`)
//!   or a pipe into `sh` keeps the line not Allow (Deny for `rm -rf /etc`);
//! * positive: `--trailer "T: $X"`, `--author "$X"`, `${X}` (value as the
//!   next word) and the `-F-` / `--file=-` / `--file -` spellings with a
//!   protected-path prose body are Allow in main and subagent mode.
//!
//! DOES NOT PROVE: the fix's glued `--trailer=…` / `--author=…` branch (not
//! measurable here, see the NOT MEASURED comment); the string steps in
//! heredoc.rs; that the
//! unquoted `--trailer $X` (word splitting / globbing into several words) is
//! harmless — it is not asserted here; anything about later separate tool
//! calls that run `.git/COMMIT_EDITMSG`.

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
        "bg-gcdadv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "git-commit-dollar-value-adversarial-20261120",
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

/// Prose that names a protected path (the backlog 3ab6109f shape).
const PROTECTED_BODY: &str =
    "subject line\n\nThe ruling is read narrowly (no git worktree /\nsecond checkout). Prose in .githooks/pre-push, the docstrings\nis updated to match.\nEOF\n";

/// A system-harm body.
const HARM_BODY: &str = "subject\n\nrm -rf /etc\nEOF\n";

/// Harmless prose: no path, no command word blastguard has a rule for.
const PROSE_BODY: &str = "subject line\n\nplain words only here\nEOF\n";

fn opener(option: &str) -> String {
    format!("git commit -q -F - {option} <<'EOF'\n")
}

fn assert_not_allow(cases: &[String]) {
    let home = unique_home("na");
    for command in cases {
        for subagent in [false, true] {
            let (d, r) = run_as(command, &home, subagent);
            assert_ne!(
                d, "allow",
                "command={command:?} subagent={subagent}: {d} | {r}"
            );
        }
    }
}

fn assert_allow(cases: &[String]) {
    let home = unique_home("ok");
    for command in cases {
        for subagent in [false, true] {
            let (d, r) = run_as(command, &home, subagent);
            assert_eq!(
                d, "allow",
                "command={command:?} subagent={subagent}: {d} | {r}"
            );
        }
    }
}

fn with_bodies(head: &str, tail: &str) -> Vec<String> {
    [PROTECTED_BODY, HARM_BODY]
        .iter()
        .map(|b| format!("{head}{b}{tail}"))
        .collect()
}

// --- required (done criteria) ---

#[test]
fn bare_dollar_operand_keeps_body_judged() {
    assert_not_allow(&with_bodies(&opener("$X"), ""));
}

#[test]
fn dollar_file_value_keeps_body_judged() {
    assert_not_allow(&with_bodies("git commit -q -F \"$X\" <<'EOF'\n", ""));
}

#[test]
fn dollar_message_value_keeps_body_judged() {
    assert_not_allow(&with_bodies(&opener("-m \"$X\""), ""));
}

#[test]
fn dollar_trailer_harm_body_run_from_commit_editmsg_is_deny() {
    let home = unique_home("deny");
    let command = opener("--trailer \"T: $X\"") + HARM_BODY + "sh .git/COMMIT_EDITMSG\n";
    for subagent in [false, true] {
        let (d, r) = run_as(&command, &home, subagent);
        assert_eq!(d, "deny", "subagent={subagent}: {d} | {r}");
    }
}

#[test]
fn git_function_redefinition_with_dollar_trailer_is_not_allow() {
    let mut cases = Vec::new();
    for def in [
        "git() { sh; }\n",
        "git() { sh; }; ",
        "function git { sh; }\n",
    ] {
        cases.extend(with_bodies(
            &format!("{def}{}", opener("--trailer \"T: $X\"")),
            "",
        ));
    }
    assert_not_allow(&cases);
}

// --- adversarial: command-running values ---

#[test]
fn command_running_trailer_values_keep_body_judged() {
    let mut cases = Vec::new();
    for option in [
        "--trailer \"T: $(id)\"",
        "--trailer \"T: `id`\"",
        "--trailer <(id)",
        "--trailer \"$(id)\"",
        "--trailer=$(id)",
        "--trailer=`id`",
        "--author \"$(id)\"",
        "--author=$(id)",
        "--trailer >(id)",
    ] {
        cases.extend(with_bodies(&opener(option), ""));
    }
    assert_not_allow(&cases);
}

// --- adversarial: other options with a $ value ---

#[test]
fn other_options_with_dollar_value_keep_body_judged() {
    let mut cases = Vec::new();
    for option in [
        "--date \"$X\"",
        "-c \"$X\"",
        "-C \"$X\"",
        "--template \"$X\"",
        "--file \"$X\"",
        "--date=$X",
        "--cleanup=$X",
    ] {
        cases.extend(with_bodies(&opener(option), ""));
    }
    cases.extend(with_bodies("git commit -q --file \"$X\" <<'EOF'\n", ""));
    cases.extend(with_bodies(
        "git commit -q -F - --trailer \"T: $X\" --date \"$Y\" <<'EOF'\n",
        "",
    ));
    assert_not_allow(&cases);
}

#[test]
fn undetermined_subcommand_or_command_word_keeps_body_judged() {
    let mut cases = Vec::new();
    for head in [
        "git $SUB -q -F - --trailer \"T: $X\" <<'EOF'\n",
        "git \"$SUB\" -q -F - <<'EOF'\n",
        "$GIT commit -q -F - --trailer \"T: $X\" <<'EOF'\n",
        "\"$GIT\" commit -q -F - <<'EOF'\n",
    ] {
        cases.extend(with_bodies(head, ""));
    }
    assert_not_allow(&cases);
}

#[test]
fn wrapped_git_with_dollar_trailer_is_not_allow() {
    let mut cases = Vec::new();
    for wrapper in ["command git", "env git", "/usr/bin/git"] {
        cases.extend(with_bodies(
            &format!("{wrapper} commit -q -F - --trailer \"T: $X\" <<'EOF'\n"),
            "",
        ));
    }
    assert_not_allow(&cases);
}

// --- adversarial: the body is then run ---

#[test]
fn dollar_trailer_then_pipe_or_run_of_commit_editmsg_is_not_allow() {
    let mut cases = Vec::new();
    cases.extend(with_bodies(
        "git commit -q -F - --trailer \"$X\" <<'EOF' | sh\n",
        "",
    ));
    cases.extend(with_bodies(
        &opener("--trailer \"$X\""),
        "sh .git/COMMIT_EDITMSG\n",
    ));
    cases.extend(with_bodies(
        "git commit -q -F - --trailer \"$X\" <<'EOF'; sh .git/COMMIT_EDITMSG\n",
        "",
    ));
    cases.extend(with_bodies(
        &opener("--author=$X"),
        "bash .git/COMMIT_EDITMSG\n",
    ));
    assert_not_allow(&cases);
}

#[test]
fn dollar_trailer_harm_body_then_run_is_deny_in_every_spelling() {
    let home = unique_home("deny2");
    for command in [
        opener("--trailer \"$X\"") + HARM_BODY + "sh .git/COMMIT_EDITMSG\n",
        format!("git commit -q -F - --trailer \"$X\" <<'EOF'; sh .git/COMMIT_EDITMSG\n{HARM_BODY}"),
        opener("--author=$X") + HARM_BODY + "sh .git/COMMIT_EDITMSG\n",
    ] {
        for subagent in [false, true] {
            let (d, r) = run_as(&command, &home, subagent);
            assert_eq!(
                d, "deny",
                "command={command:?} subagent={subagent}: {d} | {r}"
            );
        }
    }
}

// --- positive: the fix's accepted shapes are Allow ---

#[test]
fn dollar_trailer_or_author_value_with_protected_prose_is_allow() {
    let mut cases = Vec::new();
    for option in [
        "--trailer \"Condukt-Task: $TASK_ID\"",
        "--trailer \"T: ${X}\"",
        "--author \"$AUTHOR\"",
        "--trailer \"T: $X\" --author \"$Y\"",
    ] {
        cases.push(opener(option) + PROTECTED_BODY);
    }
    for head in [
        "git commit -q -F- --trailer \"T: $X\" <<'EOF'\n",
        "git commit -q --file=- --trailer \"T: $X\" <<'EOF'\n",
        "git commit -q --file - --trailer \"T: $X\" <<'EOF'\n",
        "git commit -q --trailer \"T: $X\" -F - <<'EOF'\n",
    ] {
        cases.push(format!("{head}{PROTECTED_BODY}"));
    }
    assert_allow(&cases);
}

// NOT MEASURED HERE: the glued spellings (`--trailer=$X`, `--author=$X`,
// `--trailer="T: $X"`, `--author="$X"`). They are Ask from git_stage's
// whole-tree staging rule (observed 2026-11-20 on 807706054: "an unquoted
// expansion, glob or brace list the shell may split" / "a runtime expansion
// inside an option word"; Deny in subagent mode by the generic Ask-to-Deny
// conversion), and that Ask hides whether the body is judged: with the fix
// stubbed out (git_commit_reads_message_from_stdin always false) a
// not-Deny / no-body-text-in-the-reason test of them still passed. So this
// harness cannot observe the fix's glued branch; such tests were removed
// (run-driver ruling 2026-11-20).

#[test]
fn dollar_trailer_with_harmless_prose_is_neither_ask_nor_deny() {
    let home = unique_home("prose");
    for option in [
        "--trailer \"T: $X\"",
        "--author \"$X\"",
        "--trailer \"T: ${X}\"",
    ] {
        let command = opener(option) + PROSE_BODY;
        for subagent in [false, true] {
            let (d, r) = run_as(&command, &home, subagent);
            assert!(
                d != "ask" && d != "deny",
                "option={option} subagent={subagent}: {d} | {r}"
            );
        }
    }
}
