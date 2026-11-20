#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent adversarial controls for backlog e79485cb (B2, blastguard
//! 0.2.781): a quoted heredoc body (`<<'EOF'`) on a line that also DEFINES a
//! shell function is treated as data (`heredoc_data::functions_harmless`,
//! `names_landing_path` shadowing, `ast::analyze_with_function_definitions`).
//!
//! User ruling (binding): the body is data ONLY under the narrow reading — the
//! reader is the real `cat` / `tee` / `git` (not a function of that name) and
//! nothing on the line later executes the landing file. System harm stays Deny
//! through any wrapper.
//!
//! Written by an independent test author (not the implementer) to try to break
//! that rule, in BOTH main and subagent modes. Every shape here is one where
//! the body may run (or a defined function may reach the landing file), so it
//! must NOT be Allow (Ask in main / Deny in subagent, or Deny); the system-harm
//! shapes must be Deny.
//!
//! The non-harm bodies hold `$(f)` on purpose: judged as code it is Ask in
//! main / Deny in subagent (the RED's observation), and it is Allow only when
//! the body is removed as data. A harmless body such as `id` would be Allow
//! either way and so prove nothing (observed by this author: an `id` body made
//! every shape here Allow on 0.2.781, which does not tell kept from removed;
//! a `* * * * * $(f)` cron line was Allow on BOTH 0.2.780 and 0.2.781 for the
//! `crontab` shapes, so those bodies are plain `$(f)` too).
//!
//! DOES NOT PROVE: the exact reason text (only the decision is asserted); that
//! the harmless RED shape is Allow (that is
//! `backlog_20261114_quoted_heredoc_substitution_text_fp.rs`); anything about
//! a landing file run by a LATER, separate tool call (an accepted residual);
//! that the listed shapes are exhaustive — they are probes chosen from reading
//! the diff, not an enumeration of the function grammar.

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
        "bg-b2fn-adv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "b2-function-heredoc-adversarial-20261120",
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

fn assert_not_allow(cmds: &[&str], tag: &str) {
    let bad = failing(cmds, tag, is_not_allow);
    assert!(bad.is_empty(), "expected NOT Allow:\n{}", bad.join("\n"));
}

fn assert_deny(cmds: &[&str], tag: &str) {
    let bad = failing(cmds, tag, is_deny);
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

// ------------------------------------------- 1. function named like the reader

#[test]
fn function_named_cat_is_not_the_reader() {
    assert_not_allow(
        &[
            "cat() { bash; }; cat <<'EOF'\n$(f)\nEOF",
            "cat() { git apply; }; cat <<'EOF'\n$(f)\nEOF",
            "cat() { crontab -; }; cat <<'EOF'\n$(f)\nEOF",
            "cat() { command bash; }; cat <<'EOF'\n$(f)\nEOF",
            "cat() { tee /dev/null; }; cat > x.json <<'EOF'\n$(f)\nEOF\ncat < x.json",
        ],
        "reader-cat",
    );
}

#[test]
fn function_named_tee_or_git_is_not_the_reader() {
    assert_not_allow(
        &[
            "tee() { bash; }; tee x.json <<'EOF'\n$(f)\nEOF",
            "tee() { crontab -; }; tee x.json <<'EOF'\n$(f)\nEOF",
            "git() { bash; }; git commit -F - <<'EOF'\n$(f)\nEOF",
            "git() { crontab -; }; git commit -F - <<'EOF'\n$(f)\nEOF",
        ],
        "reader-tee-git",
    );
}

#[test]
fn reader_named_function_in_other_spellings_is_not_the_reader() {
    assert_not_allow(
        &[
            // `function` keyword forms
            "function cat { crontab -; }; cat <<'EOF'\n$(f)\nEOF",
            "function cat() { crontab -; }; cat <<'EOF'\n$(f)\nEOF",
            // a backslash does not bypass a function (only an alias)
            "cat() { crontab -; }; \\cat <<'EOF'\n$(f)\nEOF",
            "cat() { crontab -; }; 'cat' <<'EOF'\n$(f)\nEOF",
            // nested: `cat` defined when `f` runs
            "f() { cat() { crontab -; }; }; f; cat <<'EOF'\n$(f)\nEOF",
            // a subshell body form
            "cat() ( crontab - ); cat <<'EOF'\n$(f)\nEOF",
        ],
        "reader-spellings",
    );
}

// ------------------------------- 2. eval / sh -c / source inside a function

#[test]
fn program_source_inside_function_body_is_not_allow() {
    assert_not_allow(
        &[
            "f() { eval \"$1\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf \"$(cat x.json)\"",
            "f() { source x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { . ./x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { sh -c \"$(cat \"$1\")\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf x.json",
            "f() { bash -c \"$(cat x.json)\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { bash \"$1\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf x.json",
            "f() { sh x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { exec bash x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { \"$@\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf sh x.json",
        ],
        "body-program-source",
    );
}

// ----------------------------------- 3. a function call fed through stdin

#[test]
fn function_call_fed_the_landing_file_or_the_body_is_not_allow() {
    assert_not_allow(
        &[
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf < x.json",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf 0< x.json",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\n{ f; } < x.json",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\n(f) < x.json",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nwhile :; do f; done < x.json",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\ncat x.json | f",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nexec < x.json; f",
            "f() { crontab - < x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { crontab - < \"$1\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf x.json",
            "f() { exec 0<x.json; crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { bash; }; cat <<'EOF' | f\n$(f)\nEOF",
            "f() { crontab -; }; cat <<'EOF' | f\n$(f)\nEOF",
            "f() { cat; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf < x.json | bash",
        ],
        "fed-stdin",
    );
}

// ------------------------------------------ 4. the landing file is executed

#[test]
fn landing_file_executed_after_a_function_definition_is_not_allow() {
    assert_not_allow(
        &[
            "f() { :; }; cat > x.sh <<'EOF'\n$(f)\nEOF\nbash x.sh",
            "f() { bash \"$1\"; }; cat > x.sh <<'EOF'\n$(f)\nEOF\nf x.sh",
            "f() { :; }; cat > x.py <<'EOF'\nimport os\nos.system('$(f)')\nEOF\npython3 x.py",
            "f() { :; }; cat > x.json <<'EOF'\n$(f)\nEOF\nsh x.json",
            "f() { :; }; cat > x.json <<'EOF'\n$(f)\nEOF\nsh < x.json",
            "f() { :; }; tee x.json <<'EOF'\n$(f)\nEOF\nbash x.json",
            "f() { :; }; cat > x.json <<'EOF'\n$(f)\nEOF\nchmod +x x.json; ./x.json",
        ],
        "landing-exec",
    );
}

#[test]
fn function_body_or_call_naming_the_landing_path_is_not_allow() {
    assert_not_allow(
        &[
            "f() { crontab \"$1\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf x.json",
            "f() { crontab x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { crontab \"$1\"; }; cat > X.JSON <<'EOF'\n$(f)\nEOF\nf x.json",
            "f() { crontab \"$1\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf ./x.json",
            "f() { crontab \"$1\"/x.json; }; cat > sub/x.json <<'EOF'\n$(f)\nEOF\nf sub",
            "f() { crontab $1; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf x.js*",
            "f() { crontab \"$1\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nX=x.json; f \"$X\"",
            "f() { crontab $(ls); }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { crontab *; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { crontab ./*.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { chmod +x x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
        ],
        "names-landing",
    );
}

#[test]
fn inert_command_names_shadowed_by_functions_are_not_inert() {
    assert_not_allow(
        &[
            "ls() { crontab \"$@\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nls x.json",
            "cd() { crontab \"$@\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\ncd x.json",
            "cd() { crontab x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\ncd .",
            "cargo() { crontab x.json; }; cat > x.json <<'EOF'\n$(f)\nEOF\ncargo test",
            "cargo() { crontab \"$2\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\ncargo test x.json",
            "head() { crontab \"$1\"; }; cat > x.json <<'EOF'\n$(f)\nEOF\nhead x.json",
        ],
        "shadowed-inert",
    );
}

// ------------------ the function re-emits a quoted body to something that runs it

#[test]
fn function_emitting_a_quoted_body_to_a_runner_is_not_allow() {
    assert_not_allow(
        &[
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f | bash",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f | crontab -",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f | awk '{system($0)}'",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f | sed e",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f | at now",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; bash <(f)",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; bash -c \"$(f)\"",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; $(f)",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; eval \"$(f)\"",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f > x.sh; bash x.sh",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f > x.json; sh x.json",
            "f() { tee x.json <<'EOF'\n$(f)\nEOF\n}; f; sh x.json",
        ],
        "reemit",
    );
}

/// A quoted body inside a function body has its own (terminal) stdout, so the
/// landing file comes only from the CALL's redirect (`f > p.awk`), which the
/// reader's landing set does not see; the file is then run on the same line by
/// a program that is not an interpreter of `ast::program_source`.
///
/// Observed 2026-11-20 by this author: every command below is Ask (main) /
/// Deny (subagent) on 7606a6b61 (0.2.780) and Allow in BOTH modes on 1d365a961
/// (0.2.781) — a fail-open introduced by the fix.
#[test]
fn function_output_redirected_then_run_by_a_non_interpreter_is_not_allow() {
    assert_not_allow(
        &[
            "f() { cat <<'EOF'\n{system(\"$(f)\")}\nEOF\n}; f > p.awk; awk -f p.awk /dev/null",
            "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f > p.awk; awk -f p.awk",
            "f() { cat <<'EOF'\nall:\n\t$(f)\nEOF\n}; f > Makefile; make",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f > x.json; awk -f x.json /dev/null",
            "f() { cat <<'EOF'\n$(f)\nEOF\n}; f >> p.awk; awk -f p.awk /dev/null",
        ],
        "call-redirect-run",
    );
}

/// Observed 2026-11-20 by this author: Allow in BOTH modes on 7606a6b61
/// (0.2.780) AND on 1d365a961 (0.2.781) — pre-existing, not introduced by the
/// B2 fix (`sed -f` with an `e` command runs its argument as a shell command).
#[test]
#[ignore = "backlog-candidate pre-existing (0.2.780 too): f(){ cat <<'EOF' 1e ...; }; f > p.sed; sed -f p.sed is Allow"]
fn function_output_redirected_then_run_by_sed_e_is_not_allow() {
    assert_not_allow(
        &["f() { cat <<'EOF'\n1e $(f)\nEOF\n}; f > p.sed; sed -f p.sed /dev/null"],
        "call-redirect-sed",
    );
}

/// Control without any function definition: the same file run by `awk -f`
/// after a plain `cat > p.awk` heredoc keeps its body judged.
#[test]
fn control_without_function_awk_file_body_is_not_allow() {
    assert_not_allow(
        &[
            "cat > p.awk <<'EOF'\n{system(\"$(f)\")}\nEOF\nawk -f p.awk /dev/null",
            "cat <<'EOF' > p.awk\nBEGIN{system(\"$(f)\")}\nEOF\nawk -f p.awk",
        ],
        "control-nofn-awk",
    );
}

// --------------------------- 6. lookup changers / redefinition / interplay

#[test]
fn lookup_changers_inside_a_function_body_are_not_allow() {
    assert_not_allow(
        &[
            "f() { alias cat=bash; }; f; cat <<'EOF'\n$(f)\nEOF",
            "f() { hash -p /bin/bash cat; }; f; cat <<'EOF'\n$(f)\nEOF",
            "f() { PATH=/tmp/evil; }; f; cat <<'EOF'\n$(f)\nEOF",
            "f() { export PATH=/tmp/evil; }; f; cat <<'EOF'\n$(f)\nEOF",
            "f() { local PATH=/tmp/evil; cat <<'EOF'\n$(f)\nEOF\n}; f",
            "f() { enable -n cat; }; f; cat <<'EOF'\n$(f)\nEOF",
            "f() { shopt -s expand_aliases; }; f; cat <<'EOF'\n$(f)\nEOF",
            "f() { declare PATH=/tmp/evil; }; f; cat <<'EOF'\n$(f)\nEOF",
            "f() { typeset -x PATH; }; f; cat <<'EOF'\n$(f)\nEOF",
        ],
        "lookup-changers",
    );
}

#[test]
fn redefinition_nesting_and_traps_are_not_allow() {
    assert_not_allow(
        &[
            "f() { :; }; f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf < x.json",
            "f() { g() { crontab -; }; }; f; cat > x.json <<'EOF'\n$(f)\nEOF\ng < x.json",
            "f() { trap 'bash x.json' EXIT; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf",
            "f() { crontab x.json; }; trap f EXIT; cat > x.json <<'EOF'\n$(f)\nEOF",
            "f() { :; }; trap 'crontab x.json' EXIT; cat > x.json <<'EOF'\n$(f)\nEOF",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nf <<< \"$(cat x.json)\"",
            "f() { crontab -; }; cat > x.json <<'EOF'\n$(f)\nEOF\nexec 3< x.json; f <&3",
            "cat() { bash; }; builtin cat <<'EOF'\n$(f)\nEOF\ncat x.json",
        ],
        "redef-nest-trap",
    );
}

// ---------------------------------------------------------- 5. system harm

#[test]
fn system_harm_in_function_heredoc_shapes_is_deny() {
    assert_deny(
        &[
            "cat() { bash; }; cat <<'EOF'\nrm -rf ~\nEOF",
            "cat() { bash; }; cat <<'EOF'\nrm -rf /\nEOF",
            "tee() { bash; }; tee x.json <<'EOF'\nrm -rf ~\nEOF",
            "git() { bash; }; git commit -F - <<'EOF'\nrm -rf ~\nEOF",
            "f() { :; }; cat <<'EOF' | bash\nrm -rf ~\nEOF",
            "f() { bash; }; cat <<'EOF' | f\nrm -rf ~\nEOF",
            "f() { cat <<'EOF'\nrm -rf ~\nEOF\n}; f | bash",
            "f() { cat <<'EOF'\nrm -rf ~\nEOF\n}; f | sh",
            "f() { :; }; cat > x.sh <<'EOF'\nrm -rf ~\nEOF\nbash x.sh",
            "f() { :; }; cat > x.json <<'EOF'\nrm -rf ~\nEOF\nbash x.json",
            "f() { bash \"$1\"; }; cat > x.json <<'EOF'\nrm -rf ~\nEOF\nf x.json",
        ],
        "harm-body",
    );
}

#[test]
fn system_harm_outside_the_body_on_a_function_line_is_deny() {
    assert_deny(
        &[
            "f() { rm -rf ~; }; cat <<'EOF'\nhi\nEOF\nf",
            "f() { rm -rf /; }; f; cat <<'EOF'\nhi\nEOF",
            "f() { :; }; cat <<'EOF'\nhi\nEOF\nrm -rf ~",
            "f() { :; }; rm -rf ~; cat <<'EOF'\nhi\nEOF",
            "f() { echo hi; }; cat <<'EOF'\n$(f)\nEOF\nrm -rf ~",
        ],
        "harm-outside",
    );
}

// ======================================================= round 2 (0.2.785)
//
// Added by the same independent author after c5d617a02 (0.2.784: a defined
// function's call stdout redirect is a landing path). One command per test fn,
// so a fail-open found here can be un-ignored on its own.
//
// DOES NOT PROVE (round 2): that the probes cover every way a function's
// output reaches a file; only the shapes listed were run.
//
// Observed 2026-11-20 by this author, once per tree: every round-2 test passes
// on 7606a6b61 (0.2.780). On c5d617a02 (0.2.784) the ten `#[ignore]`d below
// were Allow in BOTH main and subagent mode (fail-open); the rest passed.

/// One `#[test]` per command, asserting not Allow in main and subagent mode.
macro_rules! not_allow_probe {
    ($(#[$m:meta])* $name:ident, $cmd:expr) => {
        $(#[$m])*
        #[test]
        fn $name() {
            assert_not_allow(&[$cmd], stringify!($name));
        }
    };
}

// ------------------------------------- done_criteria (5): exact commands

#[test]
fn criterion5_cat_function_running_sh_with_harm_body_is_deny() {
    assert_deny(
        &["cat() { sh; }; cat <<'EOF'\nrm -rf /etc\nEOF"],
        "c5-cat-sh",
    );
}

not_allow_probe!(
    criterion5_function_running_landing_file_is_not_allow,
    "f() { sh a.txt; }; cat <<'EOF' > a.txt\nrm -rf /etc\nEOF"
);

not_allow_probe!(
    criterion5_function_body_eval_is_not_allow,
    "f() { eval x; }; cat <<'EOF'\nrm -rf /etc\nEOF"
);

not_allow_probe!(
    criterion5_unquoted_heredoc_expands_function_call_is_not_allow,
    "f() { echo hi; }; cat <<EOF\n$(f)\nEOF"
);

/// Round 4 (0.2.788): a reader-name variant that heredoc_data alone decides.
/// The bare `cat() { sh; }; cat <<'EOF'` shape above is also refused by
/// another layer (it stayed GREEN with heredoc_data's refusals stubbed out),
/// so it does not prove heredoc_data's reader-name rule. `\cat` / `'cat'`
/// still call the function `cat` (quoting bypasses only an alias), and a
/// `crontab -` body is not refused elsewhere: with heredoc_data's refusals
/// stubbed out these were observed Allow in main and subagent mode.
#[test]
fn criterion5_reader_named_function_quoted_spelling_is_not_allow() {
    assert_not_allow(
        &[
            "cat() { crontab -; }; \\cat <<'EOF'\n$(f)\nEOF",
            "cat() { crontab -; }; 'cat' <<'EOF'\n$(f)\nEOF",
        ],
        "c5-reader-name-quoted",
    );
}

// ------------- a function's output reaches a file through a compound / exec

not_allow_probe!(
    brace_group_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; { f; } > p.awk; awk -f p.awk"
);

not_allow_probe!(
    brace_group_append_with_other_member_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; { echo; f; } >> p.awk; awk -f p.awk"
);

not_allow_probe!(
    subshell_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; (f) > p.awk; awk -f p.awk"
);

not_allow_probe!(
    command_substitution_echoed_to_file_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; echo \"$(f)\" > p.awk; awk -f p.awk"
);

not_allow_probe!(
    command_substitution_assigned_then_written_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; x=$(f); printf '%s\\n' \"$x\" > p.awk; awk -f p.awk"
);

not_allow_probe!(
    exec_stdout_redirect_then_call_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; exec > p.awk; f; awk -f p.awk"
);

not_allow_probe!(
    exec_fd_redirect_then_call_to_fd_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; exec 3> p.awk; f >&3; awk -f p.awk"
);

not_allow_probe!(
    if_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; if true; then f; fi > p.awk; awk -f p.awk"
);

not_allow_probe!(
    for_loop_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; for i in 1; do f; done > p.awk; awk -f p.awk"
);

not_allow_probe!(
    while_loop_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; while true; do f; break; done > p.awk; awk -f p.awk"
);

not_allow_probe!(
    case_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; case x in x) f;; esac > p.awk; awk -f p.awk"
);

not_allow_probe!(
    pipe_to_tee_file_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f | tee p.awk; awk -f p.awk"
);

not_allow_probe!(
    pipe_to_cat_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f | cat > p.awk; awk -f p.awk"
);

not_allow_probe!(
    pipe_to_dd_of_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f | dd of=p.awk; awk -f p.awk"
);

not_allow_probe!(
    nested_call_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; g() { f; }; g > p.awk; awk -f p.awk"
);

not_allow_probe!(
    redefinition_after_a_call_then_append_then_awk_f,
    "f() { :; }; f > p.awk; f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f >> p.awk; awk -f p.awk"
);

not_allow_probe!(
    call_redirect_explicit_fd1_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f 1> p.awk; awk -f p.awk"
);

not_allow_probe!(
    call_redirect_ampersand_gt_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f &> p.awk; awk -f p.awk"
);

not_allow_probe!(
    call_redirect_clobber_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f >| p.awk; awk -f p.awk"
);

not_allow_probe!(
    call_background_redirect_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f > p.awk & wait; awk -f p.awk"
);

not_allow_probe!(
    call_into_process_substitution_writer_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f > >(cat > p.awk); awk -f p.awk"
);

not_allow_probe!(
    awk_program_from_process_substitution,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; awk -f <(f)"
);

not_allow_probe!(
    awk_program_from_dev_stdin_pipe,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f | awk -f /dev/stdin"
);

not_allow_probe!(
    awk_program_from_command_substitution_word,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; awk \"$(f)\""
);

not_allow_probe!(
    inner_reader_redirect_to_awk_file_then_awk_f,
    "f() { cat <<'EOF' > p.awk\nBEGIN{system(\"$(f)\")}\nEOF\n}; f; awk -f p.awk"
);

not_allow_probe!(
    redirect_on_function_definition_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n} > p.awk; f; awk -f p.awk"
);

// ======================================================= round 3 (0.2.787)
//
// Added by the same independent author after f32f48624 (0.2.786, ruling (a):
// a quoted heredoc INSIDE a function body is never data). Probes whether the
// "inside a function body" test (`ast::Script::function_body_ranges`) covers
// every way bash puts a heredoc inside a function, and whether the parser and
// bash agree on where a body ends. Every body is `$(f)` / `$(x)` or an awk
// program calling `system("$(f)")`, so a removed body shows as Allow.
//
// DOES NOT PROVE (round 3): that brush-parser and bash agree on every
// function-body boundary — only the spellings below were run. Observed
// 2026-11-20 by this author: all pass on 7606a6b61 (0.2.780) and f32f48624
// (0.2.786). In a scratch tree at f32f48624 with heredoc_data's refusals
// stubbed out (ruling (a) skip, functions_harmless, runs_no_landing_file,
// names_landing_path, quoted/exact check), only the awk-f / `$( )` probes
// went RED (they see heredoc_data); the crontab / sh pipe, `command` /
// `builtin cat`, `tee`, subshell-body and dead-branch probes stayed GREEN —
// another layer decides them, so they do not prove ruling (a) by themselves.

// ------------------------------------------- parser / bash disagreement

// In bash `echo }` does not close the body (`}` is an argument): the heredoc
// below is INSIDE `f`. A parser that closes the body at that `}` would see a
// top-level reader.
not_allow_probe!(
    echo_brace_does_not_close_body_then_crontab,
    "f() { echo }; cat <<'EOF'\n$(x)\nEOF\n}; f | crontab -"
);

not_allow_probe!(
    echo_brace_does_not_close_body_then_sh,
    "f() { echo }; cat <<'EOF'\n$(x)\nEOF\n}; f | sh"
);

not_allow_probe!(
    echo_brace_does_not_close_body_then_awk_f,
    "f() { echo }; cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f > p.awk; awk -f p.awk"
);

// `EOF }` is not the delimiter line (the delimiter must be alone on its line).
not_allow_probe!(
    delimiter_with_trailing_brace_is_not_the_close_then_awk_f,
    "f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF }\nEOF\n}; f > p.awk; awk -f p.awk"
);

// ------------------------------------ other ways a heredoc is in a function

not_allow_probe!(
    heredoc_in_nested_function_then_awk_f,
    "f() { g() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; }; f; g > p.awk; awk -f p.awk"
);

not_allow_probe!(
    heredoc_in_nested_function_piped_to_crontab,
    "f() { g() { cat <<'EOF'\n$(x)\nEOF\n}; }; f; g | crontab -"
);

not_allow_probe!(
    heredoc_in_function_keyword_body_then_awk_f,
    "function f { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f > p.awk; awk -f p.awk"
);

not_allow_probe!(
    heredoc_in_function_keyword_parens_body_piped_to_crontab,
    "function f() { cat <<'EOF'\n$(x)\nEOF\n}; f | crontab -"
);

not_allow_probe!(
    heredoc_in_subshell_function_body_then_awk_f,
    "f() ( cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n); f > p.awk; awk -f p.awk"
);

not_allow_probe!(
    heredoc_in_subshell_function_body_piped_to_crontab,
    "f() ( cat <<'EOF'\n$(x)\nEOF\n); f | crontab -"
);

not_allow_probe!(
    heredoc_in_function_body_inside_brace_group_then_awk_f,
    "f() { { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; }; f > p.awk; awk -f p.awk"
);

not_allow_probe!(
    heredoc_in_function_body_inside_if_then_awk_f,
    "f() { if true; then cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\nfi; }; f > p.awk; awk -f p.awk"
);

// -------------------------------- definitions in loops / $( ) / branches

not_allow_probe!(
    definition_inside_for_loop_then_awk_f,
    "for i in 1; do f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; done; f > p.awk; awk -f p.awk"
);

not_allow_probe!(
    definition_inside_while_loop_piped_to_crontab,
    "while true; do f() { cat <<'EOF'\n$(x)\nEOF\n}; break; done; f | crontab -"
);

not_allow_probe!(
    definition_inside_command_substitution_then_awk_f,
    "x=$(f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f); echo \"$x\" > p.awk; awk -f p.awk"
);

not_allow_probe!(
    definition_inside_command_substitution_as_awk_program,
    "awk \"$(f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f)\""
);

not_allow_probe!(
    definition_in_live_else_branch_then_awk_f,
    "if false; then g() { :; }; else f() { cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; fi; f > p.awk; awk -f p.awk"
);

not_allow_probe!(
    definition_after_and_and_piped_to_crontab,
    "true && f() { cat <<'EOF'\n$(x)\nEOF\n}; f | crontab -"
);

not_allow_probe!(
    definition_in_dead_branch_shadowing_reader_name,
    "if false; then cat() { :; }; fi; cat() { crontab -; }; cat <<'EOF'\n$(x)\nEOF"
);

// ------------------------------------ builtin / command cat inside a body

not_allow_probe!(
    command_cat_inside_body_then_awk_f,
    "f() { command cat <<'EOF'\nBEGIN{system(\"$(f)\")}\nEOF\n}; f > p.awk; awk -f p.awk"
);

not_allow_probe!(
    builtin_cat_inside_body_piped_to_sh,
    "f() { builtin cat <<'EOF'\n$(x)\nEOF\n}; f | sh"
);

not_allow_probe!(
    command_cat_inside_body_piped_to_crontab,
    "f() { command cat <<'EOF'\n$(x)\nEOF\n}; f | crontab -"
);

not_allow_probe!(
    tee_inside_body_piped_to_crontab,
    "f() { tee /dev/null <<'EOF'\n$(x)\nEOF\n}; f | crontab -"
);
