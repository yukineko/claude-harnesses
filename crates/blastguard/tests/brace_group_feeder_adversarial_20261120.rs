#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent adversarial controls for the brace-group feeder model
//! (backlog 7eb86bb3, blastguard 0.2.765): a brace group whose every member is
//! a plain `echo` or an evaluable `printf`, joined by `;` / newline / `&&`,
//! piped to a shell, is judged as the concatenation of its members' output.
//!
//! Written by an independent test author (not the implementer) to try to
//! break that model, in BOTH main and subagent modes:
//!
//! * harmful programs emitted by such a group — whole in one member, or split
//!   across members and joined by `;`, `&&` or a newline — must be Deny;
//! * groups outside the model (a member that reads a file, `||`, `&`, an
//!   expansion, a redirect, a nested pipe or group, a backtick, …) must NOT be
//!   Allow (Ask, or Deny in a subagent, or Deny).
//!
//! DOES NOT PROVE: which exact reason text a Deny carries (only the decision is
//! asserted), nor that a harmless group is Allow (that is the 20261114 RED's
//! job, `backlog_20261114_brace_group_feeder_piped_to_bash_fp.rs`).

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
        "bg-groupfeed-adv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "brace-group-feeder-adversarial-20261120",
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

/// Every (mode, command) whose decision fails `ok`, with decision and reason.
fn failing(cmds: &[&str], tag: &str, ok: fn(&str) -> bool) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if !ok(&d) {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

fn is_deny(d: &str) -> bool {
    d == "deny"
}

fn is_not_allow(d: &str) -> bool {
    d == "ask" || d == "deny"
}

// ---------------------------------------------------------------- Deny

#[test]
fn harmful_program_in_one_member_is_deny() {
    let bad = failing(
        &[
            "{ echo 'chmod 777 /etc/hosts'; } | bash",
            "{ echo 'rm -rf /etc'; } | sh",
            "{ echo \"rm -rf /etc\"; } | bash",
            "{ echo rm -rf /etc; } | bash",
            "{ echo 'echo hi'; echo 'rm -rf /etc'; } | bash",
            "{ echo hi && echo 'rm -rf /etc'; } | bash",
            "{ printf '%s\\n' 'rm -rf /etc'; } | bash",
        ],
        "deny-one",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_program_split_across_members_with_semicolon_is_deny() {
    let bad = failing(
        &[
            "{ printf 'chmod 777 '; echo /etc/hosts; } | bash",
            "{ printf 'rm -rf /e'; printf 'tc'; } | bash",
            "{ printf rm; printf ' -rf '; printf /etc; } | bash",
            "{ echo -n 'rm -rf /e'; echo tc; } | bash",
        ],
        "deny-split-semi",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_program_split_across_members_with_and_is_deny() {
    let bad = failing(
        &[
            "{ printf 'rm -rf /e' && printf 'tc'; } | bash",
            "{ printf 'chmod 777 ' && echo /etc/hosts; } | sh",
        ],
        "deny-split-and",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_program_split_across_members_with_newline_is_deny() {
    let bad = failing(
        &[
            "{ printf 'rm -rf /e'\nprintf 'tc'; } | bash",
            "{\nprintf 'chmod 777 '\necho /etc/hosts\n} | bash",
        ],
        "deny-split-nl",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_split_with_printf_escapes_is_deny() {
    let bad = failing(
        &[
            "{ printf 'rm -rf /\\x65'; printf 'tc'; } | bash",
            "{ printf 'rm -rf /\\145'; printf 'tc'; } | bash",
            "{ printf '%b' 'rm -rf /e'; printf 'tc'; } | bash",
        ],
        "deny-escapes",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_group_to_other_shell_readers_is_deny() {
    let bad = failing(
        &[
            "{ printf 'rm -rf '; printf /etc; } | bash -s",
            "{ printf 'rm -rf '; printf /etc; } | /bin/sh",
            "{ printf 'rm -rf '; printf /etc; } | zsh",
        ],
        "deny-readers",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_statement_beside_a_harmless_group_is_deny() {
    let bad = failing(
        &[
            "{ echo a; echo b; } | bash; rm -rf /etc",
            "{ echo a; } | bash && rm -rf /etc",
            "{ echo hi; } | bash; { printf 'rm -rf /e'; printf 'tc'; } | bash",
            "{ echo 'echo hi'; } | bash -c 'rm -rf /etc'",
        ],
        "deny-beside",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn harmful_group_in_other_spellings_is_deny() {
    let bad = failing(
        &[
            "{ echo $'rm -rf /etc'; } | bash",
            "{ echo -e 'rm -rf /\\x65tc'; } | bash",
            "{ echo 'rm -rf /etc'; } |& bash",
            "{ { echo 'rm -rf /etc'; }; } | bash",
            "( { echo 'rm -rf /etc'; } | bash )",
            "{ printf 'rm -rf /e'; printf 'tc'; } | (bash)",
            "{ printf 'rm -rf /e'; printf 'tc'; } | { bash; }",
            "{ printf 'rm -rf /e';printf 'tc';}|bash",
            "echo x; { printf 'rm -rf /e'; printf 'tc'; } | bash",
            "{ printf 'rm -rf /e'; printf 'tc'; } | bash # }",
            "{ echo; printf 'rm -rf /e'; printf 'tc'; } | bash",
            "{ printf 'rm -rf /e'; printf ''; printf 'tc'; } | bash",
            "{ echo -n 'rm '; echo -n '-rf '; echo '/etc'; } |bash",
            "{ echo 'rm -rf ~'; } | bash",
            "bash -c \"{ printf 'rm -rf /e'; printf 'tc'; } | bash\"",
        ],
        "deny-spellings",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

// ------------------------------------------------------------ not Allow

#[test]
fn group_with_a_non_emitter_member_is_not_allow() {
    let bad = failing(
        &[
            "{ cat f; } | sh",
            "{ echo a; cat f; } | bash",
            "{ cat f; echo a; } | bash",
            "{ echo a; ls; } | bash",
            "{ echo a; read x; } | bash",
        ],
        "na-member",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

#[test]
fn group_with_or_or_background_is_not_allow() {
    let bad = failing(
        &[
            "{ echo a || echo b; } | bash",
            "{ echo a & } | bash",
            "{ echo a & echo b; } | bash",
            "{ echo a; echo b & } | bash",
        ],
        "na-ops",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

#[test]
fn group_with_an_expansion_is_not_allow() {
    let bad = failing(
        &[
            "{ echo $(ls); } | bash",
            "{ echo \"$X\"; } | bash",
            "{ echo $X; } | bash",
            "{ echo a; echo `cat f`; } | bash",
            "{ printf \"$X\"; } | bash",
            "{ printf '%s' \"$(cat f)\"; } | bash",
            "{ echo ${X:-a}; } | bash",
        ],
        "na-expansion",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

#[test]
fn group_with_a_redirect_is_not_allow() {
    let bad = failing(
        &[
            "{ echo a > f; } | bash",
            "{ echo a; echo b > f; } | bash",
            "{ echo a; cat < f; } | bash",
            "{ echo a; } < f | bash",
        ],
        "na-redirect",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

#[test]
fn group_with_a_nested_pipe_or_group_is_not_allow() {
    let bad = failing(
        &[
            "{ echo a | cat; } | bash",
            "{ echo a; { cat f; }; } | bash",
            "{ { cat f; }; echo a; } | bash",
            "{ echo a; ( cat f ); } | bash",
            "{ cat f | head; } | bash",
        ],
        "na-nested",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

#[test]
fn group_feeding_a_file_reader_chain_is_not_allow() {
    let bad = failing(
        &[
            "{ echo a; } | cat f | bash",
            "{ echo a; cat; } | bash",
            "{ echo 'bash f'; } | bash",
            "{ printf 'bash '; printf f; } | sh",
        ],
        "na-chain",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

#[test]
fn group_outside_the_model_in_other_shapes_is_not_allow() {
    let bad = failing(
        &[
            "{ echo a && echo b || echo c; } | bash",
            "{ echo `echo rm -rf /etc`; } | bash",
            "{ printf 'rm -rf /e'; true; printf 'tc'; } | bash",
            "{ printf 'rm -rf /e'; : ; printf 'tc'; } | bash",
            "{ echo 'rm -rf /e' | tr -d '\\n'; echo tc; } | bash",
            "{ printf '%c%c%c' r m ' '; printf -- '-rf /etc'; } | bash",
            "{ echo 'rm -rf /e\\'; echo 'tc'; } | bash",
            "( { cat f; } | bash )",
            "( { echo a; cat f; } | bash )",
            "{ echo 'rm -rf /etc'; } | xargs -I{} bash -c {}",
        ],
        "na-shapes",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

// ------------------------------------------------- findings (ignored RED)
//
// Each was observed at 0b906f524 (blastguard 0.2.765) by this independent
// author and compared with 5cfe8e73e (the fix reverted).

/// FINDING 1 — INTRODUCED BY THE FIX (fail-open, system harm Allowed):
/// `( { printf 'rm -rf /e'; printf 'tc'; } | bash )` is Allow in both modes at
/// 0b906f524; at 5cfe8e73e it was Ask ("`bash` reads its program from another
/// command's output — blastguard cannot judge what it would do (Table B.5:
/// Undetermined)"). The unsplit `( { echo 'rm -rf /etc'; } | bash )` and the
/// unwrapped split form are Deny. The other two rows are Ask (not Deny) both
/// before and after the fix.
#[test]
fn finding1_introduced_split_group_feeder_in_subshell_is_deny() {
    let bad = failing(
        &[
            "( { printf 'rm -rf /e'; printf 'tc'; } | bash )",
            "( { printf 'chmod 777 '; echo /etc/hosts; } | bash )",
            "( { echo 'rm -rf '; echo /etc; } | sh )",
        ],
        "f1",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

/// FINDING 2 — PRE-EXISTING (system harm not Deny): a split harmful group
/// feeder inside `if` / `for` is Ask (main) both before and after the fix,
/// while `if true; then printf 'rm -rf /etc' | bash; fi` is Deny.
#[test]
#[ignore = "backlog 8ec6de86: pre-existing: split brace-group feeder inside if/for is Ask, not Deny"]
fn finding2_preexisting_split_group_feeder_in_compound_is_deny() {
    let bad = failing(
        &[
            "if true; then { printf 'rm -rf /e'; printf 'tc'; } | bash; fi",
            "for i in 1; do { printf 'rm -rf /e'; printf 'tc'; } | bash; done",
        ],
        "f2",
        is_deny,
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

/// FINDING 3 — PRE-EXISTING (fail-open, Ask-class command Allowed):
/// `git push --force` is Ask on its own, but `echo 'git push --force' | bash`
/// is Allow in both modes before and after the fix. The fix extends the same
/// reading to brace groups: the two group rows were not Allow at 5cfe8e73e
/// and are Allow at 0b906f524 (they now match the plain feeder).
#[test]
#[ignore = "backlog e0288820: pre-existing: Ask-class program fed to bash by an echo feeder is Allow"]
fn finding3_preexisting_ask_class_program_through_feeder_is_not_allow() {
    let bad = failing(
        &[
            "{ echo 'git push --force'; } | bash",
            "{ printf 'git push '; echo --force; } | bash",
            "echo 'git push --force' | bash",
        ],
        "f3",
        is_not_allow,
    );
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}
