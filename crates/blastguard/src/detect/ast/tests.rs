//! Contract tests for `detect::ast` (run-20261115-212659-5759, task
//! `ast-tests`), written by an independent test author against the frozen API
//! (`ast-api.frozen.md`, mirrored in `mod.rs`) — not by the implementer.
//!
//! Two groups, so later tasks can un-ignore them separately:
//!
//! * [`parse_caps`] (un-ignored; only `positive_control_plain_echo_is_known`
//!   stays `#[ignore = "RED: ast parse_caps"]` until eval lands): the pre-scan and
//!   parser limits — unbalanced quotes, unterminated heredoc, the
//!   `MAX_PARSE_BYTES` / `MAX_PARSE_DEPTH` caps, zsh-only syntax — plus ONE
//!   positive control (`echo hi` is `Known`), so the group cannot pass by
//!   returning `Undetermined` for everything.
//! * [`eval`] (`#[ignore = "RED: ast eval"]`): the static evaluator —
//!   assignment resolution and its scoping, `for` instantiation, cwd tracking,
//!   stdin sources, `program_source`, heredoc flags, fd dup targets, byte
//!   spans.
//!
//! Every test in both groups FAILS against the skeleton (`analyze` is always
//! `Undetermined(NotImplemented)`, `program_source` is always
//! `Some(Undetermined(..))`): an Undetermined-expecting test asserts a cause
//! other than `NotImplemented`, and every other test first requires `Known`.
//!
//! Where the frozen contract leaves a choice open, the test accepts every
//! contract-conforming answer and says so at the test (e.g. which
//! `Cause` an unbalanced quote maps to; whether `..` in a redirect target
//! after a `cd` is normalised or made Undetermined).
//!
//! Relative redirect targets are NOT such a choice (user ruling 2026-11-16,
//! task `ast-tests-tighten` of run-20261116-020031-33040), and the tests pin
//! exactly one answer:
//!
//! * no `cd` anywhere before the command: the target stays as written,
//!   `echo x > f` -> `Path(Literal("f"))`; the consumer resolves it against
//!   the hook's cwd.
//! * after a literal `cd`: the target is joined to the tracked cwd and is
//!   absolute, `cd /tmp/w && echo x > f` -> `Path(Literal("/tmp/w/f"))`.
//! * after a `cd` whose target is undetermined (`cd -`, `cd $(..)`, an
//!   unbound variable): `Path(Value::Undetermined(_))`.
//!
//! A relative `Literal` target never remains after a `cd`. An absolute target
//! after a `cd` stays as written.
//!
//! DOES NOT PROVE (parse_caps): that inputs *below* the caps are accepted
//! (only `echo hi` is checked as Known); that the chosen cap values are good
//! ones; the 1 s bound under load other than this test run's; behaviour for
//! nesting kinds not listed (arrays, arithmetic, `case`, `while`, process
//! substitution, backticks); any zsh-only syntax other than `=(cmd)` and
//! `${(f)x}`.
//!
//! DOES NOT PROVE (eval): any detector verdict (that is the binary-level
//! `tests/ast_eval_fp_targets_20261115.rs`); that every Undetermined source of
//! the contract is handled (only the listed ones are exercised); that
//! `Context` is right for anything except the pipeline / `$( )` / subshell
//! cases asserted; that `~` / `$HOME` resolve from the process HOME (not
//! exercised — it would make the tests env-dependent); `cd` alone,
//! `pushd`/`popd`, `local`/`readonly`, function bodies; spans other than the
//! one non-ASCII word checked; WHICH `ProgramSource` a wrapper (`env` /
//! `sudo` / `xargs` + interpreter) yields (only that it is not `None`); the
//! cwd of a command with no `cd` before it.

// Test-only module: a failed expectation panics (the crate root allows
// unwrap/expect under cfg(test) but not panic).
#![allow(clippy::panic)]

use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Run `analyze` on a thread with the DEFAULT stack size (no
/// `Builder::stack_size`), and require it to answer within `limit`. A stack
/// overflow aborts the whole test binary, which fails the run loudly.
fn analyze_bounded(command: String, limit: Duration) -> Analysis {
    let (tx, rx) = mpsc::channel();
    let start = Instant::now();
    let handle = std::thread::Builder::new()
        .name("ast-analyze-bounded".into())
        .spawn(move || {
            let a = analyze(&command);
            let _ = tx.send(a);
        })
        .expect("spawn analyze thread");
    let got = rx.recv_timeout(limit);
    let elapsed = start.elapsed();
    match got {
        Ok(a) => {
            handle.join().expect("analyze thread panicked");
            assert!(
                elapsed <= limit,
                "analyze took {elapsed:?}, over the {limit:?} bound"
            );
            a
        }
        Err(e) => panic!("analyze did not answer within {limit:?} ({e:?})"),
    }
}

fn known(command: &str) -> Script {
    match analyze(command) {
        Analysis::Known(s) => s,
        Analysis::Undetermined(c) => {
            panic!("expected Known for {command:?}, got Undetermined({c:?})")
        }
    }
}

fn lit(s: &str) -> Value {
    Value::Literal(s.to_string())
}

fn is_undet(v: &Value) -> bool {
    matches!(v, Value::Undetermined(_))
}

/// The command word, when it is a literal.
fn name(c: &SimpleCommand) -> Option<&str> {
    match c.argv.first().map(|w| &w.value) {
        Some(Value::Literal(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Every command whose literal command word is `n`, in source order.
fn all_named<'a>(s: &'a Script, n: &str) -> Vec<&'a SimpleCommand> {
    s.commands.iter().filter(|c| name(c) == Some(n)).collect()
}

/// The single command whose literal command word is `n`.
fn one_named<'a>(s: &'a Script, n: &str) -> &'a SimpleCommand {
    let v = all_named(s, n);
    assert_eq!(v.len(), 1, "expected exactly one `{n}` command in {s:#?}");
    v[0]
}

fn arg(c: &SimpleCommand, i: usize) -> &Value {
    &c.argv
        .get(i)
        .unwrap_or_else(|| panic!("argv[{i}] missing in {c:#?}"))
        .value
}

fn one_redirect(c: &SimpleCommand) -> &Redirect {
    assert_eq!(c.redirects.len(), 1, "expected one redirect in {c:#?}");
    &c.redirects[0]
}

mod parse_caps {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(1);

    /// Undetermined for a reason the parse stage owns: anything except the
    /// skeleton's `NotImplemented`. The contract does not fix which of
    /// `Parse` / `Unsupported` a malformed input maps to, so both are
    /// accepted.
    fn assert_parse_undetermined(command: &str) {
        match analyze_bounded(command.to_string(), LIMIT) {
            Analysis::Undetermined(Cause::Parse(_) | Cause::Unsupported(_)) => {}
            other => {
                panic!("expected Undetermined(Parse|Unsupported) for {command:?}, got {other:?}")
            }
        }
    }

    fn assert_too_deep(kind: &str, command: String) {
        match analyze_bounded(command, LIMIT) {
            Analysis::Undetermined(Cause::TooDeep) => {}
            other => panic!("{kind}: expected Undetermined(TooDeep), got {other:?}"),
        }
    }

    /// Depths checked for every nesting kind: just over the cap, and far over
    /// it, both still under `MAX_PARSE_BYTES`. The second depth is bounded by
    /// the byte cap: the widest fixture (`if true; then ` + `fi; `, 18 bytes
    /// per level) must fit in 8 KiB, so 400 (7200 B + tail) is used; 4000 did
    /// not fit (user ruling 2026-11-16, esc-38006305).
    fn depths() -> [usize; 2] {
        [MAX_PARSE_DEPTH + 1, 400]
    }

    fn assert_under_byte_cap(command: &str) {
        assert!(
            command.len() <= MAX_PARSE_BYTES,
            "fixture must stay under MAX_PARSE_BYTES to test the DEPTH cap ({} > {})",
            command.len(),
            MAX_PARSE_BYTES
        );
    }

    #[test]
    fn positive_control_plain_echo_is_known() {
        match analyze_bounded("echo hi".to_string(), LIMIT) {
            Analysis::Known(s) => {
                assert_eq!(s.commands.len(), 1, "{s:#?}");
                assert_eq!(name(&s.commands[0]), Some("echo"));
                assert_eq!(arg(&s.commands[0], 1), &lit("hi"));
            }
            other => panic!("expected Known for `echo hi`, got {other:?}"),
        }
    }

    #[test]
    fn unbalanced_single_quote_is_undetermined() {
        assert_parse_undetermined("echo 'abc");
    }

    #[test]
    fn unbalanced_double_quote_is_undetermined() {
        assert_parse_undetermined("echo \"abc");
    }

    #[test]
    fn unterminated_heredoc_is_undetermined() {
        assert_parse_undetermined("cat <<'EOF'\nrm -rf /tmp/z\n");
    }

    #[test]
    fn over_max_parse_bytes_is_too_long() {
        let command = format!("echo {}", "a".repeat(MAX_PARSE_BYTES));
        assert!(command.len() > MAX_PARSE_BYTES);
        match analyze_bounded(command, LIMIT) {
            Analysis::Undetermined(Cause::TooLong) => {}
            other => panic!("expected Undetermined(TooLong), got {other:?}"),
        }
    }

    #[test]
    fn nested_brace_groups_over_cap_are_too_deep() {
        for n in depths() {
            let command = format!("{}echo hi;{}", "{ ".repeat(n), " }".repeat(n));
            assert_under_byte_cap(&command);
            assert_too_deep(&format!("brace groups depth {n}"), command);
        }
    }

    #[test]
    fn nested_subshells_over_cap_are_too_deep() {
        for n in depths() {
            let command = format!("{}echo hi{}", "(".repeat(n), ")".repeat(n));
            assert_under_byte_cap(&command);
            assert_too_deep(&format!("subshells depth {n}"), command);
        }
    }

    #[test]
    fn nested_if_over_cap_is_too_deep() {
        for n in depths() {
            let command = format!(
                "{}echo hi; {}",
                "if true; then ".repeat(n),
                "fi; ".repeat(n)
            );
            assert_under_byte_cap(&command);
            assert_too_deep(&format!("if depth {n}"), command);
        }
    }

    #[test]
    fn nested_command_substitution_over_cap_is_too_deep() {
        for n in depths() {
            let command = format!("echo {}echo hi{}", "$(".repeat(n), ")".repeat(n));
            assert_under_byte_cap(&command);
            assert_too_deep(&format!("$( ) depth {n}"), command);
        }
    }

    /// Boundary: the caps are `>`, not `>=`. A fixture of exactly the cap must
    /// get past the pre-parse caps (`parse` is `Ok`, and `analyze` is neither
    /// `TooDeep` nor `TooLong`); one more must be the cap's own `Cause`.
    /// `parse` is checked directly because the evaluator may still be a
    /// skeleton (`Undetermined(NotImplemented)`), which is not a cap answer.
    fn assert_passes_caps(kind: &str, command: &str) {
        if let Err(c) = super::parse::parse(command) {
            panic!("{kind}: expected the caps to let {command:?} through, parse failed with {c:?}");
        }
        if let Analysis::Undetermined(c @ (Cause::TooDeep | Cause::TooLong)) =
            analyze_bounded(command.to_string(), LIMIT)
        {
            panic!("{kind}: {c:?} at exactly the cap for {command:?}");
        }
    }

    /// `n` nested brace groups: plain-bracket depth is exactly `n` (one `{`
    /// per level, closers all after the openers).
    fn brace_groups(n: usize) -> String {
        let command = format!("{}echo hi;{}", "{ ".repeat(n), " }".repeat(n));
        assert_eq!(command.matches('{').count(), n);
        assert_eq!(command.matches('}').count(), n);
        command
    }

    /// `n` nested subshells: plain-bracket depth is exactly `n`.
    fn subshells(n: usize) -> String {
        let command = format!("{}echo hi{}", "(".repeat(n), ")".repeat(n));
        assert_eq!(command.matches('(').count(), n);
        assert_eq!(command.matches(')').count(), n);
        command
    }

    #[test]
    fn brace_groups_at_exactly_max_depth_pass_the_cap() {
        assert_passes_caps("brace groups depth 16", &brace_groups(16));
    }

    #[test]
    fn brace_groups_one_over_max_depth_are_too_deep() {
        assert_too_deep("brace groups depth 17", brace_groups(17));
    }

    #[test]
    fn subshells_at_exactly_max_depth_pass_the_cap() {
        assert_passes_caps("subshells depth 16", &subshells(16));
    }

    #[test]
    fn subshells_one_over_max_depth_are_too_deep() {
        assert_too_deep("subshells depth 17", subshells(17));
    }

    fn padded_echo(total: usize) -> String {
        let command = format!("echo {}", "a".repeat(total - "echo ".len()));
        assert_eq!(command.len(), total);
        command
    }

    #[test]
    fn exactly_max_parse_bytes_passes_the_cap() {
        assert_passes_caps("8192 bytes", &padded_echo(8192));
    }

    #[test]
    fn one_byte_over_max_parse_bytes_is_too_long() {
        match analyze_bounded(padded_echo(8193), LIMIT) {
            Analysis::Undetermined(Cause::TooLong) => {}
            other => panic!("expected Undetermined(TooLong) at 8193 bytes, got {other:?}"),
        }
    }

    #[test]
    fn zsh_process_substitution_equals_is_zsh_only() {
        match analyze_bounded("diff =(ls) /tmp/a".to_string(), LIMIT) {
            Analysis::Undetermined(Cause::ZshOnly(_)) => {}
            other => panic!("expected Undetermined(ZshOnly) for `=(cmd)`, got {other:?}"),
        }
    }

    #[test]
    fn zsh_parameter_flags_are_zsh_only() {
        match analyze_bounded("echo ${(f)x}".to_string(), LIMIT) {
            Analysis::Undetermined(Cause::ZshOnly(_)) => {}
            other => panic!("expected Undetermined(ZshOnly) for `${{(f)x}}`, got {other:?}"),
        }
    }
}

mod eval {
    use super::*;

    // ---------------- literal assignment resolution ----------------

    #[test]
    fn assignment_then_semicolon_resolves_rm_operand() {
        let s = known("S=/tmp/a; rm -rf $S");
        let rm = one_named(&s, "rm");
        assert_eq!(arg(rm, 2), &lit("/tmp/a"));
        assert_eq!(rm.argv[2].raw, "$S");
        assert_eq!(rm.argv[2].quoting, Quoting::Unquoted);
    }

    #[test]
    fn assignment_then_and_resolves_redirect_target() {
        let s = known("S=/tmp/a && cat > $S/x");
        let cat = one_named(&s, "cat");
        let r = one_redirect(cat);
        assert_eq!(r.op, RedirOp::Out);
        assert_eq!(r.target, RedirTarget::Path(lit("/tmp/a/x")));
    }

    #[test]
    fn assignment_then_or_and_newline_bind() {
        let s = known("S=/tmp/a || true\nrm -rf \"${S}/y\"");
        assert_eq!(arg(one_named(&s, "rm"), 2), &lit("/tmp/a/y"));
    }

    #[test]
    fn export_assignment_binds() {
        let s = known("export S=/tmp/a; rm -rf $S");
        assert_eq!(arg(one_named(&s, "rm"), 2), &lit("/tmp/a"));
    }

    #[test]
    fn prefix_assignment_is_not_used_for_word_expansion_unbound() {
        // `S=x cmd $S`: the prefix binds S in cmd's ENVIRONMENT only; the
        // word `$S` is expanded before that, with S unbound.
        let s = known("S=x printf %s $S");
        let c = one_named(&s, "printf");
        assert_eq!(c.assignments.len(), 1, "{c:#?}");
        assert_eq!(c.assignments[0].0, "S");
        assert_eq!(c.assignments[0].1.value, lit("x"));
        assert!(
            is_undet(arg(c, 2)),
            "$S must not take the prefix value: {c:#?}"
        );
    }

    #[test]
    fn prefix_assignment_word_uses_pre_existing_binding() {
        let s = known("S=/a; S=/b rm -rf $S");
        let rm = one_named(&s, "rm");
        assert_eq!(arg(rm, 2), &lit("/a"));
        assert_eq!(rm.assignments[0].1.value, lit("/b"));
    }

    #[test]
    fn reassignment_uses_latest() {
        let s = known("S=/a; S=/b; rm -rf $S");
        assert_eq!(arg(one_named(&s, "rm"), 2), &lit("/b"));
    }

    #[test]
    fn assignment_inside_subshell_does_not_leak() {
        let s = known("(S=/a); rm -rf $S");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    #[test]
    fn assignment_in_pipeline_stage_does_not_leak() {
        let s = known("S=/a | cat; rm -rf $S");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    #[test]
    fn assignment_inside_command_substitution_does_not_leak() {
        let s = known("echo $(S=/a); rm -rf $S");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    #[test]
    fn value_from_command_substitution_is_undetermined() {
        for c in ["S=$(pwd); rm -rf $S", "S=`pwd`; rm -rf $S"] {
            let s = known(c);
            assert!(is_undet(arg(one_named(&s, "rm"), 2)), "{c:?}");
        }
    }

    #[test]
    fn value_from_read_is_undetermined() {
        let s = known("S=/a; read S; rm -rf $S");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    #[test]
    fn unset_makes_undetermined() {
        let s = known("S=/a; unset S; rm -rf $S");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    #[test]
    fn arithmetic_value_is_undetermined() {
        let s = known("S=$((1+2)); rm -rf /tmp/$S");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    #[test]
    fn unbound_variable_is_undetermined() {
        let s = known("rm -rf $BLASTGUARD_AST_TEST_SURELY_UNBOUND/x");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    #[test]
    fn default_from_unbound_is_undetermined() {
        let s = known("rm -rf ${BLASTGUARD_AST_TEST_SURELY_UNBOUND:-/tmp/a}");
        assert!(is_undet(arg(one_named(&s, "rm"), 2)));
    }

    // ---------------- for loops ----------------

    #[test]
    fn for_over_literal_words_yields_one_instance_per_value() {
        let s = known("for v in a b; do rm -rf /tmp/$v; done");
        let rms = all_named(&s, "rm");
        assert_eq!(rms.len(), 2, "{s:#?}");
        assert_eq!(arg(rms[0], 2), &lit("/tmp/a"));
        assert_eq!(arg(rms[1], 2), &lit("/tmp/b"));
    }

    /// A `for` over a non-literal word list cannot be instantiated per value.
    /// The body must still appear (a 0-iteration fold would drop the `rm` and
    /// read as "nothing runs" = clean), and its loop variable is Undetermined.
    #[test]
    fn for_over_expansion_keeps_body_with_undetermined_var() {
        for c in [
            "for f in $(ls); do rm $f; done",
            "for f in $BLASTGUARD_AST_TEST_SURELY_UNBOUND; do rm $f; done",
            "for f in \"$@\"; do rm $f; done",
        ] {
            let s = known(c);
            let rms = all_named(&s, "rm");
            assert!(
                !rms.is_empty(),
                "{c:?}: the loop body must still appear: {s:#?}"
            );
            for rm in rms {
                assert!(is_undet(arg(rm, 1)), "{c:?}: {rm:#?}");
            }
        }
    }

    #[test]
    fn for_over_glob_is_undetermined() {
        let s = known("for f in *.txt; do rm $f; done");
        let rms = all_named(&s, "rm");
        assert!(!rms.is_empty(), "the loop body must still appear: {s:#?}");
        for rm in rms {
            assert!(is_undet(arg(rm, 1)), "{rm:#?}");
        }
    }

    // ---------------- cwd ----------------

    /// The redirect target of the single `echo`, which must be a `>` redirect.
    fn echo_out_target(command: &str) -> RedirTarget {
        let s = known(command);
        let r = one_redirect(one_named(&s, "echo"));
        assert_eq!(r.op, RedirOp::Out, "{command:?}: {r:#?}");
        r.target.clone()
    }

    /// Exactly `Path(Literal(expected))` — no alternative is accepted (user
    /// ruling 2026-11-16, see the module doc).
    fn assert_path_target(command: &str, expected: &str) {
        assert_eq!(
            echo_out_target(command),
            RedirTarget::Path(lit(expected)),
            "{command:?}"
        );
    }

    #[test]
    fn relative_redirect_without_cd_stays_relative() {
        assert_path_target("echo x > f", "f");
    }

    #[test]
    fn literal_cd_sets_cwd_for_relative_redirect() {
        let s = known("cd /tmp/w && echo x > f");
        let e = one_named(&s, "echo");
        assert_eq!(e.cwd, lit("/tmp/w"));
        assert_eq!(
            one_redirect(e).target,
            RedirTarget::Path(lit("/tmp/w/f")),
            "{e:#?}"
        );
    }

    #[test]
    fn cd_through_literal_variable_sets_cwd() {
        let s = known("S=/tmp/w; cd $S && echo x > f");
        let e = one_named(&s, "echo");
        assert_eq!(e.cwd, lit("/tmp/w"));
        assert_eq!(
            one_redirect(e).target,
            RedirTarget::Path(lit("/tmp/w/f")),
            "{e:#?}"
        );
    }

    #[test]
    fn cd_to_etc_joins_relative_redirect() {
        assert_path_target("cd /etc && echo x > f", "/etc/f");
    }

    #[test]
    fn absolute_redirect_after_cd_stays_as_written() {
        assert_path_target("cd /tmp/w && echo x > /etc/passwd", "/etc/passwd");
    }

    /// `..` after a `cd` is either normalised lexically (`/etc/passwd`) or
    /// Undetermined (a symlinked cwd makes lexical normalisation unsound —
    /// both are contract-conforming). A Literal that still contains `..`, or
    /// any other literal, is not.
    #[test]
    fn dotdot_redirect_after_cd_is_normalised_or_undetermined() {
        let command = "cd /tmp/w && echo x > ../../etc/passwd";
        match echo_out_target(command) {
            RedirTarget::Path(Value::Undetermined(_)) => {}
            RedirTarget::Path(Value::Literal(p)) if p == "/etc/passwd" => {}
            other => panic!(
                "{command:?}: expected Path(Literal(\"/etc/passwd\")) or \
                 Path(Undetermined), got {other:?}"
            ),
        }
    }

    #[test]
    fn relative_cd_is_relative_to_previous_cwd() {
        let s = known("cd /tmp/w && cd sub && echo x");
        assert_eq!(one_named(&s, "echo").cwd, lit("/tmp/w/sub"));
    }

    #[test]
    fn cd_dash_and_cd_substitution_make_cwd_undetermined() {
        for c in [
            "cd /tmp/w && cd - && echo x > f",
            "cd /tmp/w && cd $(pwd) && echo x > f",
            "cd /tmp/w && cd $UNKNOWN_VAR && echo x > f",
            "cd - && echo x > f",
            "cd $(pwd) && echo x > f",
            "cd $UNKNOWN_VAR && echo x > f",
        ] {
            let s = known(c);
            let e = one_named(&s, "echo");
            assert!(is_undet(&e.cwd), "{c:?}: {s:#?}");
            // The relative target must not stay a relative literal after a
            // `cd`, nor be joined to the stale `/tmp/w`.
            assert!(
                matches!(
                    one_redirect(e).target,
                    RedirTarget::Path(Value::Undetermined(_))
                ),
                "{c:?}: {e:#?}"
            );
        }
    }

    // ---------------- stdin ----------------

    #[test]
    fn echo_piped_into_bash_is_literal_stdin() {
        let s = known("echo hi | bash");
        let b = one_named(&s, "bash");
        assert_eq!(b.stdin, StdinSource::Literal("hi\n".into()));
        assert_eq!(b.context, Context::Pipeline { pos: 1, len: 2 });
    }

    #[test]
    fn printf_piped_into_python_is_literal_stdin() {
        let s = known("printf 'print(1)\\n' | python3");
        assert_eq!(
            one_named(&s, "python3").stdin,
            StdinSource::Literal("print(1)\n".into())
        );
    }

    #[test]
    fn here_string_is_literal_stdin_with_newline() {
        let s = known("bash <<< 'x'");
        assert_eq!(
            one_named(&s, "bash").stdin,
            StdinSource::Literal("x\n".into())
        );
    }

    #[test]
    fn quoted_heredoc_is_literal_stdin() {
        let s = known("bash <<'EOF'\nrm -rf /tmp/z\nEOF");
        assert_eq!(
            one_named(&s, "bash").stdin,
            StdinSource::Literal("rm -rf /tmp/z\n".into())
        );
    }

    #[test]
    fn unquoted_heredoc_with_substitution_is_undetermined_stdin() {
        let s = known("bash <<EOF\n$(id)\nEOF");
        let b = one_named(&s, "bash");
        assert!(
            matches!(b.stdin, StdinSource::Undetermined(_)),
            "{:#?}",
            b.stdin
        );
    }

    /// A `$( )` inside an unquoted heredoc body runs (in the CALLING shell,
    /// before `bash` starts): it must appear as a command, and the heredoc
    /// must not be reported as literal stdin.
    #[test]
    fn unquoted_heredoc_substitution_command_is_visible() {
        let s = known("bash <<EOF\n$(rm -rf /etc)\nEOF");
        let rm = one_named(&s, "rm");
        assert_eq!(rm.context, Context::CommandSubstitution, "{rm:#?}");
        assert_eq!(arg(rm, 2), &lit("/etc"));
        let b = one_named(&s, "bash");
        assert!(
            !matches!(b.stdin, StdinSource::Literal(_)),
            "{:#?}",
            b.stdin
        );
    }

    #[test]
    fn non_literal_producer_is_from_command() {
        let s = known("curl x | bash");
        assert_eq!(one_named(&s, "bash").stdin, StdinSource::FromCommand);
    }

    // ---------------- program_source ----------------

    fn ps(command: &str, prog: &str) -> Option<ProgramSource> {
        let s = known(command);
        program_source(one_named(&s, prog))
    }

    #[test]
    fn script_operand_is_the_program_and_dev_stdin_is_an_arg() {
        assert_eq!(
            ps("bash x.sh /dev/stdin", "bash"),
            Some(ProgramSource::File(lit("x.sh")))
        );
    }

    #[test]
    fn dash_c_program_is_inline_and_dev_stdin_is_dollar_zero() {
        assert_eq!(
            ps("bash -c echo /dev/stdin", "bash"),
            Some(ProgramSource::InlineLiteral("echo".into()))
        );
    }

    #[test]
    fn dev_stdin_operand_reads_program_from_stdin() {
        assert!(matches!(
            ps("bash /dev/stdin", "bash"),
            Some(ProgramSource::Stdin(_))
        ));
        assert_eq!(
            ps("echo hi | bash /dev/stdin", "bash"),
            Some(ProgramSource::Stdin(StdinSource::Literal("hi\n".into())))
        );
    }

    #[test]
    fn dash_s_and_lone_dash_read_program_from_stdin() {
        assert!(matches!(
            ps("bash -s", "bash"),
            Some(ProgramSource::Stdin(_))
        ));
        assert!(matches!(ps("sh -", "sh"), Some(ProgramSource::Stdin(_))));
    }

    #[test]
    fn python_dash_c_is_inline_literal() {
        assert_eq!(
            ps("python3 -c 'print(1)'", "python3"),
            Some(ProgramSource::InlineLiteral("print(1)".into()))
        );
    }

    /// A wrapper that runs an interpreter (`env bash -s`, `sudo bash`,
    /// `xargs bash`) must not read as "not an interpreter" (`None` = no
    /// program to inspect = clean). Which `Some(_)` is not fixed here.
    #[test]
    fn interpreter_wrappers_have_a_program_source() {
        for (command, wrapper) in [
            ("env bash -s", "env"),
            ("sudo bash", "sudo"),
            ("xargs bash", "xargs"),
        ] {
            let got = ps(command, wrapper);
            assert!(got.is_some(), "{command:?}: program_source was None");
        }
    }

    #[test]
    fn non_interpreter_has_no_program_source() {
        assert_eq!(ps("ls -la", "ls"), None);
    }

    // ---------------- heredoc ----------------

    fn heredoc_of(command: &str, prog: &str) -> HereDoc {
        let s = known(command);
        let r = one_redirect(one_named(&s, prog));
        assert_eq!(r.op, RedirOp::HereDoc);
        match &r.target {
            RedirTarget::HereDoc(h) => h.clone(),
            other => panic!("expected HereDoc target, got {other:?}"),
        }
    }

    #[test]
    fn quoted_heredoc_flag_and_literal_body() {
        let command = "cat <<'EOF'\nx $y\nEOF";
        let h = heredoc_of(command, "cat");
        assert!(h.quoted);
        assert!(!h.strip_tabs);
        assert_eq!(h.body, lit("x $y\n"));
        assert_eq!(&command[h.body_span.clone()], "x $y\n");
    }

    #[test]
    fn dash_heredoc_strips_tabs() {
        let h = heredoc_of("cat <<-EOF\n\tx\n\tEOF", "cat");
        assert!(h.strip_tabs);
        assert!(!h.quoted);
        assert_eq!(h.body, lit("x\n"));
    }

    #[test]
    fn unquoted_heredoc_with_expansion_has_undetermined_body() {
        let h = heredoc_of("cat <<EOF\n$y\nEOF", "cat");
        assert!(!h.quoted);
        assert!(is_undet(&h.body), "{h:#?}");
    }

    // ---------------- fd dup targets ----------------

    fn fd_target(command: &str) -> FdTarget {
        let s = known(command);
        let r = one_redirect(one_named(&s, "bash"));
        assert_eq!(r.op, RedirOp::DupIn, "{r:#?}");
        match &r.target {
            RedirTarget::Fd(t) => t.clone(),
            other => panic!("expected Fd target for {command:?}, got {other:?}"),
        }
    }

    #[test]
    fn dup_in_number() {
        assert_eq!(fd_target("bash <&3"), FdTarget::Number(3));
        let s = known("bash <&3");
        assert_eq!(
            one_named(&s, "bash").stdin,
            StdinSource::Fd(FdTarget::Number(3))
        );
    }

    #[test]
    fn dup_in_move() {
        assert_eq!(fd_target("bash <&3-"), FdTarget::Move(3));
    }

    #[test]
    fn dup_in_close() {
        assert_eq!(fd_target("bash <&-"), FdTarget::Close);
    }

    #[test]
    fn dup_in_quoted_number_is_dequoted() {
        // bash dequotes the word before reading it as an fd number.
        assert_eq!(fd_target("bash <&'3'"), FdTarget::Number(3));
    }

    #[test]
    fn dup_in_unbound_variable_is_undetermined() {
        assert!(matches!(fd_target("bash <&$fd"), FdTarget::Undetermined(_)));
    }

    // ---------------- spans ----------------

    #[test]
    fn spans_are_byte_offsets() {
        // `é` is 2 bytes in UTF-8: a char-offset span would be off by one.
        let command = "echo é && rm -rf x";
        let s = known(command);
        let rm = one_named(&s, "rm");
        let at = command.find("rm").unwrap();
        assert_eq!(rm.argv[0].span, at..at + 2);
        assert_eq!(&command[rm.argv[0].span.clone()], "rm");
        assert_eq!(&command[rm.span.clone()], "rm -rf x");
        let e = one_named(&s, "echo");
        assert_eq!(&command[e.argv[1].span.clone()], "é");
    }
}

/// RED tests for two fail-opens found by an independent verifier and observed
/// in bash on 2026-11-16 (`HOME=/h; echo a=~/x b=~ c=x:~/y of=~/f` prints
/// `a=/h/x b=/h c=x:/h/y of=/h/f`; `x=/a; [[ 0 -eq x=5 ]]` leaves `x=5`).
/// Contract (criterion 2): a value not provably determined from the text is
/// Undetermined, never a guessed literal.
mod tilde_and_arith_fail_open {
    use super::*;

    fn word(command: &str, cmd: &str, i: usize) -> Value {
        let s = known(command);
        arg(one_named(&s, cmd), i).clone()
    }

    fn cat_target(command: &str) -> RedirTarget {
        let s = known(command);
        one_redirect(one_named(&s, "cat")).target.clone()
    }

    #[test]
    fn dd_of_tilde_word_is_undetermined() {
        let v = word("dd if=/dev/zero of=~/f", "dd", 2);
        assert!(is_undet(&v), "of=~/f is tilde-expanded by bash: {v:?}");
    }

    #[test]
    fn name_equals_tilde_word_is_undetermined() {
        let v = word("echo a=~/x", "echo", 1);
        assert!(is_undet(&v), "a=~/x is tilde-expanded by bash: {v:?}");
    }

    #[test]
    fn colon_tilde_word_is_undetermined() {
        let v = word("echo c=x:~/y", "echo", 1);
        assert!(is_undet(&v), "c=x:~/y is tilde-expanded by bash: {v:?}");
    }

    #[test]
    fn control_equals_word_without_tilde_stays_literal() {
        assert_eq!(word("echo a=x", "echo", 1), lit("a=x"));
    }

    #[test]
    fn arith_compare_assignment_makes_variable_undetermined() {
        let t = cat_target("x=/a; [[ 0 -eq x=1 ]]; cat > $x");
        match t {
            RedirTarget::Path(v) if is_undet(&v) => {}
            other => panic!("[[ 0 -eq x=1 ]] assigns x; got {other:?}"),
        }
    }

    #[test]
    fn control_arith_compare_without_assignment_keeps_literal() {
        assert_eq!(
            cat_target("x=/a; [[ 0 -eq 1 ]]; cat > $x"),
            RedirTarget::Path(lit("/a"))
        );
    }
}

mod arith_subscript_fail_open {
    use super::*;

    fn cat_target(command: &str) -> RedirTarget {
        let s = known(command);
        one_redirect(one_named(&s, "cat")).target.clone()
    }

    #[test]
    fn printf_v_arith_subscript_makes_variable_undetermined() {
        let t = cat_target("x=/a; printf -v 'a[x=6]' hi; cat > $x");
        match t {
            RedirTarget::Path(v) if is_undet(&v) => {}
            other => panic!("printf -v 'a[x=6]' assigns x in zsh; got {other:?}"),
        }
    }

    #[test]
    fn read_arith_subscript_makes_variable_undetermined() {
        let t = cat_target("x=/a; read 'a[x=8]' < /dev/null; cat > $x");
        match t {
            RedirTarget::Path(v) if is_undet(&v) => {}
            other => panic!("read 'a[x=8]' assigns x; got {other:?}"),
        }
    }

    #[test]
    fn declare_i_arith_assignment_makes_variable_undetermined() {
        let t = cat_target("x=/a; declare -i n; n=x=9; cat > $x");
        match t {
            RedirTarget::Path(v) if is_undet(&v) => {}
            other => panic!("n=x=9 under declare -i assigns x; got {other:?}"),
        }
    }

    #[test]
    fn control_read_plain_name_keeps_literal() {
        assert_eq!(
            cat_target("x=/a; read y < /dev/null; cat > $x"),
            RedirTarget::Path(lit("/a"))
        );
    }
}

mod subscript_command_substitution_fail_open {
    use super::*;

    /// A quoted name operand whose subscript holds `$( )` / backquotes runs
    /// that command in the shell; a Known script that omits it hides it.
    fn assert_undetermined(command: &str) {
        match analyze(command) {
            Analysis::Undetermined(_) => {}
            Analysis::Known(s) => panic!(
                "{command:?} runs a command inside the subscript; a Known script hides it: {s:#?}"
            ),
        }
    }

    #[test]
    fn read_quoted_subscript_substitution_is_undetermined() {
        assert_undetermined("read 'a[$(rm -rf /tmp/x)]' </dev/null");
    }

    #[test]
    fn unset_quoted_subscript_substitution_is_undetermined() {
        assert_undetermined("unset 'a[$(rm -rf /tmp/x)]'");
    }

    #[test]
    fn declare_quoted_subscript_substitution_is_undetermined() {
        assert_undetermined("declare 'a[$(rm -rf /tmp/x)]=1'");
    }

    #[test]
    fn printf_v_quoted_subscript_substitution_is_undetermined() {
        assert_undetermined("printf -v 'a[$(rm -rf /tmp/x)]' hi");
    }

    #[test]
    fn read_quoted_subscript_backquote_is_undetermined() {
        assert_undetermined("read 'a[`rm -rf /tmp/x`]' </dev/null");
    }

    #[test]
    fn control_read_plain_subscript_stays_known() {
        let _ = known("read 'a[1]' </dev/null");
    }
}

/// User ruling 2026-11-16: a subscript or arithmetic expression whose value
/// comes from the environment (not fixed by the command text) is
/// `Undetermined(Unsupported)`, because bash re-evaluates it recursively.
mod env_arith_unsupported {
    use super::*;

    fn assert_unsupported(command: &str) {
        match analyze(command) {
            Analysis::Undetermined(Cause::Unsupported(_)) => {}
            other => panic!(
                "{command:?} depends on an environment value in a subscript/arithmetic \
                 context; expected Undetermined(Unsupported), got {other:#?}"
            ),
        }
    }

    fn assert_known(command: &str) {
        match analyze(command) {
            Analysis::Known(_) => {}
            other => panic!("{command:?} is fully determined by its text; got {other:#?}"),
        }
    }

    #[test]
    fn read_subscript_unassigned_var_is_unsupported() {
        assert_unsupported("read 'a[$x]' </dev/null");
    }

    #[test]
    fn read_subscript_bare_name_is_unsupported() {
        assert_unsupported("read 'a[i]' </dev/null");
    }

    #[test]
    fn unset_subscript_bare_name_is_unsupported() {
        assert_unsupported("unset 'a[i]'");
    }

    #[test]
    fn printf_v_subscript_bare_name_is_unsupported() {
        assert_unsupported("printf -v 'a[i]' hi");
    }

    #[test]
    fn arith_expansion_unassigned_var_is_unsupported() {
        assert_unsupported("echo $(( x + 1 ))");
    }

    #[test]
    fn arith_command_unassigned_var_is_unsupported() {
        assert_unsupported("(( x ))");
    }

    #[test]
    fn double_bracket_arith_unassigned_var_is_unsupported() {
        assert_unsupported("[[ x -eq 1 ]]");
    }

    #[test]
    fn let_unassigned_var_is_unsupported() {
        assert_unsupported("let 'y = x'");
    }

    #[test]
    fn assignment_with_unknown_subscript_is_unsupported() {
        assert_unsupported("a[i]=v");
    }

    #[test]
    fn control_read_literal_subscript_stays_known() {
        assert_known("read 'a[1]' </dev/null");
    }

    #[test]
    fn control_read_assigned_var_subscript_stays_known() {
        assert_known("x=1; read 'a[$x]' </dev/null");
    }

    #[test]
    fn control_arith_assigned_var_stays_known() {
        assert_known("i=2; echo $(( i + 1 ))");
    }

    #[test]
    fn control_double_bracket_literal_arith_stays_known() {
        assert_known("[[ 3 -eq 1 ]]");
    }

    #[test]
    fn control_literal_subscript_assignment_stays_known() {
        assert_known("a[0]=v");
    }
}
