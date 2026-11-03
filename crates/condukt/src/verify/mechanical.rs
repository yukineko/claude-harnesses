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
        if !tail_parses("cargo test") {
            return None;
        }
        let mut cmd = vec!["cargo".to_string(), "test".to_string()];
        if let Ok(re2) = regex::Regex::new(r"-p\s+([A-Za-z0-9_-]+)") {
            if let Some(c) = re2.captures(done_criteria).and_then(|c| c.get(1)) {
                cmd.push("-p".to_string());
                cmd.push(c.as_str().to_string());
            }
        }
        return Some(cmd);
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

/// True when the criteria text from the FIRST case-insensitive occurrence of
/// `needle` to the end tokenizes with [`parse_argv`]. `false` when it does not
/// (unterminated quote) and also when `needle` cannot be located or the regex
/// is unavailable: undetermined resolves restricted (no prose command).
fn prose_tail_parses(done_criteria: &str, needle: &str) -> bool {
    let Ok(re) = regex::Regex::new(&format!("(?i){}", regex::escape(needle))) else {
        return false;
    };
    match re.find(done_criteria) {
        Some(m) => parse_argv(&done_criteria[m.start()..]).is_some(),
        None => false,
    }
}
