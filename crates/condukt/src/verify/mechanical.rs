//! Mechanical-command extraction from a done_criteria string, split out of
//! `verify.rs` (file-growth ratchet). Pure move of [`parse_argv`],
//! [`mechanical_cmd`] and [`prose_tail_parses`]; behaviour is unchanged by the
//! move.

use super::{is_criteria_runner, prose_pytest_argv};

/// Tokenize a command string into argv respecting shell quoting/escaping
/// (e.g. `"cargo test -- --path \"a b/c\""` keeps `a b/c` as one token),
/// unlike `split_whitespace` which would split it into `a`, `b/c"`.
///
/// Returns `None` when `shlex` cannot parse the input (e.g. an unterminated
/// quote). There is deliberately NO `split_whitespace` fallback: a best-effort
/// argv would be a different command from the one written, and
/// `state check-criteria` would run it and could set `skip_verifier`. An
/// unparseable command is undetermined and resolves restricted (no mechanical
/// command, so the LLM verifier is required). User ruling 2026-11-03.
pub(super) fn parse_argv(s: &str) -> Option<Vec<String>> {
    shlex::split(s)
}

/// Extract a runnable command from a done_criteria string for mechanical gate
/// checking. Returns `None` when no mechanical check can be derived (the LLM
/// verifier is then required). This is about *runnability* only: it returns
/// the FIRST runnable command and says nothing about whether that command
/// discharges the whole criteria. [`classify_criteria`](super::classify_criteria) layers the behavioral
/// veto and [`criteria_coverage_for`](super::criteria_coverage_for) (fed THIS function's returned argv)
/// layers the coverage veto on top.
///
/// `mechanical_check_hint` is the interpreter-declared, structured
/// [`crate::model::Task::mechanical_check`] fact. When `Some`, its `cmd` is
/// authoritative and tokenized via [`parse_argv`]; an unparseable `cmd` (e.g.
/// an unterminated quote) yields `None`, never a guessed argv. Otherwise: the
/// first backticked span whose head is an [`is_criteria_runner`], then
/// recognised runner prose (`cargo test`, `npm test`, pytest via
/// [`prose_pytest_argv`], `go test`).
///
/// `None` is also returned when ANY backticked span is unparseable by shlex
/// (unterminated quote), whatever its head token: the criteria names a command
/// that cannot be determined, and falling through to the prose recognizer would
/// run a different command than the one written. The LLM verifier is then
/// required (User ruling 2026-11-03).
///
/// The prose fallback obeys the same rule (User ruling 2026-11-04): before a
/// prose arm returns, the criteria text from the recognised runner mention to
/// the end must tokenize with shlex. If it does not (an unterminated `"` or
/// `'` after the runner, e.g. `cargo test -p demo -- "x exits 0`), the argv
/// the arm would build is a different command from the one written, so `None`
/// is returned instead. Consequence, stated rather than hidden: an English
/// apostrophe after the runner mention ("cargo test passes, doesn't regress")
/// also makes the tail unparseable and yields `None` (LLM verifier required).
/// Known gap: a quote opened BEFORE the runner mention (`FOO="1 cargo test`)
/// is not seen, because only the tail is tokenized.
///
/// The prose `cargo test` arm keeps the cargo options written in the tail:
/// `-p X` / `--package X`, `--test X` (and the other value options in
/// [`CARGO_TEST_VALUE_FLAGS`]), any other `-`-prefixed flag, and everything
/// after a `--` separator (`-- --include-ignored`). So prose
/// `cargo test -p condukt --test t -- --include-ignored exits 0` yields
/// `[cargo, test, -p, condukt, --test, t, --, --include-ignored]`. The words
/// after the argv must be only the trailing assertion `exits 0` (or `exit 0`);
/// any other bare word could be a test filter, so the argv cannot be cleanly
/// separated from the prose and `None` is returned (no guessed wider or
/// narrower command). Prose `cargo test` with no options and no trailing words
/// yields `[cargo, test]`, as before.
pub fn mechanical_cmd(
    done_criteria: &str,
    mechanical_check_hint: Option<&crate::model::MechanicalCheck>,
) -> Option<Vec<String>> {
    if let Some(check) = mechanical_check_hint {
        // `?`-style: an unparseable hint cmd yields None (never a guessed argv).
        return parse_argv(&check.cmd);
    }
    // Prefer an explicit backtick command: `cargo test -p condukt`
    if let Ok(re) = regex::Regex::new(r"`([^`]+)`") {
        for caps in re.captures_iter(done_criteria) {
            if let Some(inner) = caps.get(1) {
                match parse_argv(inner.as_str()) {
                    Some(argv) => {
                        if argv.first().is_some_and(|p| is_criteria_runner(p)) {
                            return Some(argv);
                        }
                    }
                    None => {
                        // Unparseable span (unterminated quote), whatever its
                        // head. The criteria wrote a command we cannot
                        // determine; falling through to prose recognition
                        // would run a DIFFERENT command than the one written
                        // (e.g. `FOO=1 cargo test -- "x` -> `cargo test`).
                        // Fail closed: no mechanical command.
                        return None;
                    }
                }
            }
        }
    }
    // Fall back to recognised test-runner prose. Each arm first requires the
    // text from the runner mention onward to tokenize (no unterminated quote);
    // otherwise the command is undetermined and no argv is guessed.
    let lower = done_criteria.to_lowercase();
    let tail_parses = |needle: &str| prose_tail_parses(done_criteria, needle);
    if lower.contains("cargo test") {
        // The cargo arm builds its argv from the tail itself (see
        // `prose_cargo_test_argv`); `None` when the tail cannot be cleanly split
        // into cargo argv and trailing prose.
        let tail = prose_tail(done_criteria, "cargo test")?;
        return prose_cargo_test_argv(tail);
    }
    if lower.contains("npm test") {
        if !tail_parses("npm test") {
            return None;
        }
        return Some(vec!["npm".to_string(), "test".to_string()]);
    }
    if lower.contains("pytest") {
        if !tail_parses("pytest") {
            return None;
        }
        return Some(prose_pytest_argv(done_criteria));
    }
    if lower.contains("go test") {
        if !tail_parses("go test") {
            return None;
        }
        return Some(vec!["go".to_string(), "test".to_string()]);
    }
    None
}

/// The criteria text from the FIRST case-insensitive occurrence of `needle` to
/// the end. `None` when `needle` cannot be located or the regex is unavailable.
fn prose_tail<'a>(done_criteria: &'a str, needle: &str) -> Option<&'a str> {
    let re = regex::Regex::new(&format!("(?i){}", regex::escape(needle))).ok()?;
    re.find(done_criteria).map(|m| &done_criteria[m.start()..])
}

/// True when the criteria text from the FIRST case-insensitive occurrence of
/// `needle` to the end tokenizes with [`parse_argv`]. `false` when it does not
/// (unterminated quote) and also when `needle` cannot be located or the regex
/// is unavailable: undetermined resolves restricted (no prose command).
fn prose_tail_parses(done_criteria: &str, needle: &str) -> bool {
    prose_tail(done_criteria, needle).is_some_and(|tail| parse_argv(tail).is_some())
}

/// Options of `cargo test` that take a value as the next token (before `--`).
const CARGO_TEST_VALUE_FLAGS: &[&str] = &[
    "-p",
    "--package",
    "--test",
    "--bin",
    "--example",
    "--exclude",
    "--features",
];

/// Trailing assertion prose that may follow a `cargo test` command without
/// being a test filter (compared lowercase, trailing `.` stripped).
const CARGO_TEST_TRAILING_PROSE: &[&str] = &["exits 0", "exit 0"];

/// Build the argv for a prose `cargo test` tail (text starting at `cargo test`).
///
/// The tail is tokenized with [`parse_argv`]. Leading `cargo test` is kept,
/// then cargo options are consumed: a token starting with `-` is kept as-is,
/// and a token in [`CARGO_TEST_VALUE_FLAGS`] (before `--`) also keeps the value
/// token that follows it. A `--` separator switches to test-binary arguments
/// (every later `-`-prefixed token is kept verbatim). The first bare word ends
/// the argv: the remaining words are prose, and they must be exactly one of
/// [`CARGO_TEST_TRAILING_PROSE`] (`exits 0`). Any other remainder — a bare word
/// that could be a test filter, or a word like `passes` the argv does not
/// account for — returns `None`, because the argv would then be a wider or
/// narrower command than the one written. A missing option value, or a head
/// other than `cargo test`, also returns `None`.
fn prose_cargo_test_argv(tail: &str) -> Option<Vec<String>> {
    let tokens = parse_argv(tail)?;
    if tokens.len() < 2
        || !tokens[0].eq_ignore_ascii_case("cargo")
        || !tokens[1].eq_ignore_ascii_case("test")
    {
        return None;
    }
    let mut argv = vec!["cargo".to_string(), "test".to_string()];
    let mut after_sep = false;
    let mut i = 2;
    while let Some(t) = tokens.get(i).map(String::as_str) {
        if !t.starts_with('-') {
            break;
        }
        argv.push(t.to_string());
        i += 1;
        if t == "--" {
            after_sep = true;
        } else if !after_sep && CARGO_TEST_VALUE_FLAGS.contains(&t) {
            argv.push(tokens.get(i)?.clone());
            i += 1;
        }
    }
    let rest = tokens[i..].join(" ");
    let rest = rest.trim_end_matches('.').to_lowercase();
    if rest.is_empty() || CARGO_TEST_TRAILING_PROSE.contains(&rest.as_str()) {
        Some(argv)
    } else {
        None
    }
}

/// `found.max(semicolon_clause_count(done_criteria))`, kept here so the call
/// site in `verify.rs` stays one line (file-growth ratchet).
pub(super) fn semi_max(done_criteria: &str, found: usize) -> usize {
    found.max(semicolon_clause_count(done_criteria))
}

/// Number of non-empty `;` / `；` separated clauses of `done_criteria`,
/// ignoring `;` inside backticked spans (a command like `a; b` is one claim).
/// Each clause that carries an alphanumeric character is a separate claim, so
/// "X exits 0; no regressions; version bumped." is 3 claims. Fed into
/// `enumerated_claim_count` via max, so a single clause yields 1 (never
/// vetoes) and multiple clauses make the coverage `Partial` (verifier runs).
/// An unbalanced backtick leaves the tail span unstripped, which can only
/// over-count (the restrictive side).
pub(super) fn semicolon_clause_count(done_criteria: &str) -> usize {
    let mut outside = String::with_capacity(done_criteria.len());
    for (i, part) in done_criteria.split('`').enumerate() {
        if i % 2 == 0 {
            outside.push_str(part);
        } else {
            outside.push('X');
        }
    }
    outside
        .split([';', '；'])
        .filter(|c| c.chars().any(char::is_alphanumeric))
        .count()
}

// NOTE: the prose `cargo test` cases below were written by the implementer of
// the backlog fix (5a633271), NOT by an independent verifier, so they are
// non-independent evidence. The independent RED repro is the integration test
// `backlog_20261116_check_criteria_drops_test_target`.
#[cfg(test)]
mod tests {
    use super::mechanical_cmd;

    fn argv(v: &[&str]) -> Option<Vec<String>> {
        Some(v.iter().map(|s| s.to_string()).collect())
    }

    /// Prose `cargo test` keeps `--test <name>` and the `--`-separated
    /// test-binary flags; the trailing assertion `exits 0` is not an argv word.
    #[test]
    fn prose_keeps_test_target_and_trailing_flags() {
        let dc = "cargo test -p condukt --test worktree_remove_orphan_leftover -- --include-ignored exits 0";
        assert_eq!(
            mechanical_cmd(dc, None),
            argv(&[
                "cargo",
                "test",
                "-p",
                "condukt",
                "--test",
                "worktree_remove_orphan_leftover",
                "--",
                "--include-ignored",
            ])
        );
    }

    /// A trailing assertion without any filter option still yields the bare
    /// runner argv (prose `exits 0` dropped, not passed as a filter).
    #[test]
    fn prose_trailing_exits_is_not_a_filter() {
        assert_eq!(
            mechanical_cmd("cargo test -p condukt exits 0.", None),
            argv(&["cargo", "test", "-p", "condukt"])
        );
        assert_eq!(
            mechanical_cmd("cargo test exit 0", None),
            argv(&["cargo", "test"])
        );
    }

    /// Fail-closed: a bare word after the argv could be a test filter (or prose
    /// the argv does not account for), so no argv is guessed.
    #[test]
    fn prose_unaccounted_word_yields_none() {
        assert_eq!(
            mechanical_cmd("cargo test -p condukt -- foo_filter", None),
            None
        );
        assert_eq!(mechanical_cmd("cargo test -p condukt passes", None), None);
        assert_eq!(
            mechanical_cmd("cargo test -p condukt exits 0; no regressions", None),
            None
        );
    }

    /// Fail-closed: a missing option value, an unterminated quote in the tail,
    /// or a head that is not `cargo test` yields no argv.
    #[test]
    fn prose_malformed_tail_yields_none() {
        assert_eq!(mechanical_cmd("cargo test -p", None), None);
        assert_eq!(
            mechanical_cmd(r#"cargo test -p x -- "never exits 0"#, None),
            None
        );
        assert_eq!(mechanical_cmd("cargo testing -p x exits 0", None), None);
    }
}
