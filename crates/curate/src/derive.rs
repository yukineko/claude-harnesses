//! Derive an evalkit golden case from a promotion seed.
//!
//! The honest hard part: a fugu playbook records a *procedure* (title +
//! free-text `done_criteria`), not an `input→expected` test. So we auto-derive a
//! runnable case only when the acceptance criterion is *mechanical* — a
//! backticked shell command, or a recognised test-runner invocation. Everything
//! else becomes a **draft**: a valid golden that evalkit skips until a human
//! writes the assertion. (User-chosen policy: auto + draft fallback.)

use std::hash::{Hash, Hasher};

use regex::Regex;
use serde_json::{json, Value};

use crate::seed::Seed;

/// Build the evalkit golden case (as JSON) for a seed. `force_draft` emits a
/// draft even when a command could be derived (for review-before-trust).
pub fn derive_golden(seed: &Seed, force_draft: bool) -> Value {
    let id = slug_id(&seed.title);
    match (force_draft, mechanical_cmd(&seed.done_criteria)) {
        (false, Some(cmd)) => json!({
            "id": id,
            "describe": seed.title,
            "cmd": cmd,
            "assert": { "exit": 0 },
        }),
        _ => json!({
            "id": id,
            "describe": draft_describe(&seed.title, &seed.done_criteria),
            "draft": true,
        }),
    }
}

/// True if a runnable command can be auto-derived from this criterion.
pub fn is_mechanical(done_criteria: &str) -> bool {
    mechanical_cmd(done_criteria).is_some()
}

/// Extract a runnable command (argv) from a free-text acceptance criterion, or
/// `None` if it isn't mechanical. Two signals, strongest first:
///   1. an explicit backticked command — `` `cargo test --workspace` ``
///   2. a recognised test-runner mention — "cargo test", "npm test", …
///
/// An unterminated quote yields NO command (User ruling 2026-11-03, GH 1518,
/// extended to curate 2026-11-04): if ANY backticked span cannot be tokenized
/// by shlex, the criterion is undetermined and becomes a draft — it does not
/// fall through to the prose recognizer, which would emit a different command
/// than the one written (`FOO=1 cargo test -- "x` -> `cargo test`). The prose
/// path likewise returns `None` when the text from the runner mention onward
/// does not tokenize (see [`test_runner_command`]).
fn mechanical_cmd(done_criteria: &str) -> Option<Vec<String>> {
    match backticked_command(done_criteria) {
        Backticked::Command(cmd) => Some(cmd),
        Backticked::Unparseable => None,
        Backticked::Absent => test_runner_command(done_criteria),
    }
}

/// Outcome of scanning backticked spans: a runnable command, a span that
/// cannot be tokenized (undetermined — never guessed), or no command span.
enum Backticked {
    Command(Vec<String>),
    Unparseable,
    Absent,
}

/// First backticked span whose first token is a known runner/script, tokenized
/// with shell quoting rules (shlex). Any span shlex cannot parse (unterminated
/// quote), whatever its head, yields [`Backticked::Unparseable`]: there is no
/// `split_whitespace` fallback, since a best-effort argv is a different
/// command from the one written.
fn backticked_command(s: &str) -> Backticked {
    let Ok(re) = Regex::new(r"`([^`]+)`") else {
        return Backticked::Unparseable;
    };
    for caps in re.captures_iter(s) {
        let Some(inner) = caps.get(1) else {
            continue;
        };
        match shlex::split(inner.as_str().trim()) {
            Some(argv) => {
                if argv.first().is_some_and(|p| is_command_word(p)) {
                    return Backticked::Command(argv);
                }
            }
            None => return Backticked::Unparseable,
        }
    }
    Backticked::Absent
}

/// True when the criterion text from the FIRST case-insensitive occurrence of
/// `needle` to the end tokenizes with shlex. `false` on an unterminated quote,
/// and also when `needle` cannot be located or the regex is unavailable
/// (undetermined resolves to draft).
fn prose_tail_parses(s: &str, needle: &str) -> bool {
    let Ok(re) = Regex::new(&format!("(?i){}", regex::escape(needle))) else {
        return false;
    };
    match re.find(s) {
        Some(m) => shlex::split(&s[m.start()..]).is_some(),
        None => false,
    }
}

/// Canonical command for a recognised test runner mentioned in prose, capturing
/// a `-p <crate>` scope for cargo when present.
///
/// `None` when the text from the runner mention onward does not tokenize
/// (unterminated `"`/`'`, e.g. `cargo test -p demo -- "x exits 0`): the
/// canonical argv would drop the written arguments and run a broader command.
/// Consequence, stated rather than hidden: an English apostrophe after the
/// runner mention ("cargo test passes, doesn't regress") also yields `None`
/// (draft). Known gap: a quote opened BEFORE the mention is not seen.
fn test_runner_command(s: &str) -> Option<Vec<String>> {
    let lower = s.to_lowercase();
    let needle = ["cargo test", "npm test", "pytest", "go test"]
        .into_iter()
        .find(|n| lower.contains(n))?;
    if !prose_tail_parses(s, needle) {
        return None;
    }
    if lower.contains("cargo test") {
        let mut cmd = vec!["cargo".to_string(), "test".to_string()];
        if let Ok(re) = Regex::new(r"-p\s+([A-Za-z0-9_-]+)") {
            if let Some(c) = re.captures(s).and_then(|c| c.get(1)) {
                cmd.push("-p".to_string());
                cmd.push(c.as_str().to_string());
            }
        }
        return Some(cmd);
    }
    if lower.contains("npm test") {
        return Some(vec!["npm".to_string(), "test".to_string()]);
    }
    if lower.contains("pytest") {
        return Some(vec!["pytest".to_string()]);
    }
    if lower.contains("go test") {
        return Some(vec!["go".to_string(), "test".to_string()]);
    }
    None
}

fn is_command_word(tok: &str) -> bool {
    matches!(
        tok,
        "cargo"
            | "npm"
            | "pnpm"
            | "yarn"
            | "pytest"
            | "go"
            | "make"
            | "bash"
            | "sh"
            | "python"
            | "python3"
    ) || tok.starts_with("./")
}

/// Draft describe carries the criterion so the human knows what to assert.
fn draft_describe(title: &str, done_criteria: &str) -> String {
    if done_criteria.trim().is_empty() {
        format!("{title} — TODO: add a file/cmd assertion")
    } else {
        format!(
            "{title} — TODO assert done_criteria: {}",
            done_criteria.trim()
        )
    }
}

/// Stable, collision-resistant case id: an ASCII slug of the title plus a short
/// hash (so non-ASCII/Japanese titles that slug to the same stem stay distinct).
fn slug_id(title: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let stem = slug.trim_matches('-');
    let stem = if stem.is_empty() { "case" } else { stem };

    let mut h = std::collections::hash_map::DefaultHasher::new();
    title.hash(&mut h);
    format!("{stem}-{:06x}", (h.finish() as u32) & 0xff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(title: &str, dc: &str) -> Seed {
        Seed {
            ts: 0,
            title: title.into(),
            done_criteria: dc.into(),
        }
    }

    #[test]
    fn backticked_command_becomes_cmd_golden() {
        let g = derive_golden(&seed("add tests", "`cargo test --workspace` passes"), false);
        assert_eq!(g["cmd"], json!(["cargo", "test", "--workspace"]));
        assert_eq!(g["assert"]["exit"], json!(0));
        assert!(g.get("draft").is_none());
    }

    #[test]
    fn cargo_test_with_scope_is_captured() {
        let cmd = mechanical_cmd("verify that cargo test -p evalkit passes").unwrap();
        assert_eq!(cmd, vec!["cargo", "test", "-p", "evalkit"]);
    }

    #[test]
    fn pytest_and_npm_recognised() {
        assert_eq!(mechanical_cmd("pytest is green").unwrap(), vec!["pytest"]);
        assert_eq!(
            mechanical_cmd("ensure npm test exits 0").unwrap(),
            vec!["npm", "test"]
        );
    }

    #[test]
    fn non_mechanical_criterion_becomes_draft() {
        let g = derive_golden(
            &seed("refresh token flow", "auth handles token refresh"),
            false,
        );
        assert_eq!(g["draft"], json!(true));
        assert!(g.get("cmd").is_none());
        assert!(g["describe"].as_str().unwrap().contains("TODO"));
    }

    #[test]
    fn force_draft_overrides_a_mechanical_criterion() {
        let g = derive_golden(&seed("t", "`cargo test` passes"), true);
        assert_eq!(g["draft"], json!(true));
    }

    #[test]
    fn backtick_ignored_when_not_a_command() {
        // a backticked identifier, not a command → not mechanical.
        assert!(mechanical_cmd("the `Episode` struct gains a field").is_none());
    }

    #[test]
    fn slug_id_is_ascii_stable_and_unique_for_distinct_titles() {
        let a = slug_id("fugu-router label サブコマンド");
        let b = slug_id("fugu-router label something else");
        assert!(a.starts_with("fugu-router-label"), "{a}");
        assert_ne!(a, b);
        assert_eq!(a, slug_id("fugu-router label サブコマンド")); // stable
    }

    #[test]
    fn pure_non_ascii_title_still_gets_an_id() {
        let id = slug_id("日本語のみのタイトル");
        assert!(id.starts_with("case-"), "{id}");
    }
}
