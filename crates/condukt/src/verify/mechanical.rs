//! Mechanical-command extraction from a done_criteria string, split out of
//! `verify.rs` (file-growth ratchet): [`parse_argv`], [`mechanical_cmd`],
//! [`prose_tail_parses`] and the prose `cargo test` argv builder
//! (`prose_cargo_test_argv`, which keeps `--test`/`-p`/`--` arguments).

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
/// `[cargo, test, -p, condukt, --test, t, --, --include-ignored]`. A recognised
/// prose assertion (`exits 0`, `passes`, `succeeds`, ...) ends the argv and the
/// remainder is ignored; a quoted word is an explicit argument; any other bare
/// unquoted word could be a test filter, so `None` is returned (no guessed
/// wider or narrower command). Option values keep the `[A-Za-z0-9_-]`
/// restriction. Prose `cargo test` with no options yields `[cargo, test]`. See
/// `prose_cargo_test_argv` for the full grammar.
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

/// Split `tail` into raw shell words on whitespace outside quotes, returning
/// each word unquoted (via shlex) together with whether it was written with any
/// quoting/escaping. `None` on an unterminated quote or a word shlex does not
/// reduce to exactly one token (undetermined, never guessed).
fn split_words_with_quote_flag(tail: &str) -> Option<Vec<(String, bool)>> {
    let mut raws: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = tail.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == '\\' && q == '"' {
                    cur.push(chars.next()?);
                } else if c == q {
                    quote = None;
                }
            }
            None if c.is_whitespace() => {
                if in_word {
                    raws.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            None => {
                in_word = true;
                cur.push(c);
                if c == '\\' {
                    cur.push(chars.next()?);
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                }
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        raws.push(cur);
    }
    raws.into_iter()
        .map(|raw| {
            let quoted = raw.contains(['"', '\'', '\\']);
            let mut v = parse_argv(&raw)?;
            if v.len() == 1 {
                v.pop().map(|w| (w, quoted))
            } else {
                None
            }
        })
        .collect()
}

/// A recognised prose assertion starting at `words` (lowercased, trailing
/// `.,;:` stripped): `exits 0`, `exit 0`, `passes`, `pass`, `succeeds`,
/// `succeed`. Such a phrase ends the argv; whatever follows is prose.
fn starts_prose_verb(words: &[(String, bool)]) -> bool {
    let w = |i: usize| {
        words
            .get(i)
            .map(|(t, _)| t.trim_end_matches(['.', ',', ';', ':']).to_lowercase())
    };
    match w(0).as_deref() {
        Some("passes" | "pass" | "succeeds" | "succeed") => true,
        Some("exits" | "exit") => w(1).as_deref() == Some("0"),
        _ => false,
    }
}

/// True when `s` is non-empty and every char is `[A-Za-z0-9_-]` or in `extra`.
fn all_chars(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || extra.contains(&c))
}

/// Build the argv for a prose `cargo test` tail (text starting at `cargo test`).
///
/// Words are taken left to right after `cargo test`:
/// - an unquoted `-`-prefixed word (charset `[A-Za-z0-9_=,.:-]`) is kept; before
///   `--`, a flag in [`CARGO_TEST_VALUE_FLAGS`] also keeps the next word, which
///   must match `[A-Za-z0-9_-]+` (else `None`);
/// - a `--` word is kept and switches to test-binary arguments (no more value
///   flags);
/// - a quoted word is an explicit argument and is kept (unquoted) verbatim;
/// - an unquoted bare word must start a recognised prose assertion
///   ([`starts_prose_verb`]: `exits 0`, `passes`, `succeeds`, ...); that ends the
///   argv and the rest of the criteria is ignored (multi-claim criteria are
///   handled by the coverage counter). Any other bare word could be a test
///   filter or unaccounted prose, so `None` is returned: no wider or narrower
///   command is guessed.
///
/// An unterminated quote, a missing/invalid option value, or a head other than
/// `cargo test` also returns `None`.
fn prose_cargo_test_argv(tail: &str) -> Option<Vec<String>> {
    let words = split_words_with_quote_flag(tail)?;
    if words.len() < 2
        || !words[0].0.eq_ignore_ascii_case("cargo")
        || !words[1].0.eq_ignore_ascii_case("test")
    {
        return None;
    }
    let mut argv = vec!["cargo".to_string(), "test".to_string()];
    let mut after_sep = false;
    let mut i = 2;
    while let Some((t, quoted)) = words.get(i) {
        if *quoted {
            argv.push(t.clone());
            i += 1;
        } else if t.starts_with('-') {
            if !all_chars(t, &['=', ',', '.', ':']) {
                return None;
            }
            argv.push(t.clone());
            i += 1;
            if t == "--" {
                after_sep = true;
            } else if !after_sep && CARGO_TEST_VALUE_FLAGS.contains(&t.as_str()) {
                let (v, _) = words.get(i)?;
                if !all_chars(v, &[]) {
                    return None;
                }
                argv.push(v.clone());
                i += 1;
            }
        } else if starts_prose_verb(&words[i..]) {
            return Some(argv);
        } else {
            return None;
        }
    }
    Some(argv)
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

    /// Fail-closed: an unquoted bare word that is not a recognised prose verb
    /// could be a test filter, so no argv is guessed. Recognised verbs end the
    /// argv and later prose is ignored; quoted words are arguments.
    #[test]
    fn prose_unaccounted_word_yields_none_and_verbs_end_argv() {
        assert_eq!(
            mechanical_cmd("cargo test -p condukt -- foo_filter", None),
            None
        );
        assert_eq!(
            mechanical_cmd("cargo test -p condukt foo passes", None),
            None
        );
        assert_eq!(
            mechanical_cmd("cargo test -p condukt passes", None),
            argv(&["cargo", "test", "-p", "condukt"])
        );
        assert_eq!(
            mechanical_cmd("cargo test -p condukt exits 0 and docs updated", None),
            argv(&["cargo", "test", "-p", "condukt"])
        );
        assert_eq!(
            mechanical_cmd(r#"cargo test -p demo -- "x" exits 0"#, None),
            argv(&["cargo", "test", "-p", "demo", "--", "x"])
        );
        assert_eq!(mechanical_cmd("cargo test -p demo exits 1", None), None);
    }

    /// Option values keep the package/test-name charset.
    #[test]
    fn prose_option_value_charset_restricted() {
        assert_eq!(mechanical_cmd("cargo test -p x, exits 0", None), None);
        assert_eq!(mechanical_cmd("cargo test --test t. exits 0", None), None);
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
