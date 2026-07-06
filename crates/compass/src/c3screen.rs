//! c3screen — a deterministic *advisory* pre-filter for the C3 "observable DoD"
//! LLM gate (gates.rs evaluates only the C1/C2 floor; C3 is judged by the skill).
//!
//! ARCHITECTURE: this is a lexical SCREEN, not a hard gate. It flags DoD items
//! whose wording is vague/non-observable (no measurable pass/fail) so the skill's
//! C3 judgment has a deterministic starting point — it never replaces that
//! judgment and never blocks. A DoD item is flagged iff it contains a **vague
//! token** AND lacks any **observability signal** (a digit, a measurable keyword,
//! or a path-like token). Items that name a test, a command, a number, a file, or
//! an explicit pass/fail condition are therefore NOT flagged.

use serde::Serialize;

/// Vague verbs/adjectives with no inherent measurable pass/fail. Small, explicit,
/// advisory list — Japanese + English. Matched case-insensitively (substring).
const VAGUE_TOKENS: &[&str] = &[
    // Japanese
    "速く",
    "速くする",
    "きれい",
    "きれいにする",
    "改善",
    "最適化",
    "使いやすく",
    // English
    "fast",
    "clean",
    "better",
    "improve",
    "optimize",
    "nicer",
];

/// Keywords that signal a measurable/observable pass-fail condition. Their
/// presence suppresses a flag even when a vague token is also present. Matched
/// case-insensitively (substring). Digits and path-like tokens are handled
/// separately (see [`has_observability_signal`]).
const MEASURABLE_KEYWORDS: &[&str] = &["test", "テスト", "exit", "benchmark", "回"];

/// One flagged DoD item: its index in `definition_of_done`, the item text, and a
/// human-readable reason (which vague token tripped it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct C3Flag {
    /// 0-based index of the item in the charter `definition_of_done`.
    pub index: usize,
    /// The DoD item text, verbatim.
    pub item: String,
    /// Why it was flagged (the vague term, and that no observability signal was
    /// present).
    pub reason: String,
}

/// Whether a whitespace token looks like a file/path (contains `/`, or a `.` with
/// alphanumerics on both sides — a `name.ext` shape). Liberal by design: an
/// advisory screen should under-flag rather than over-flag, so anything remotely
/// path-like counts as an observability signal.
fn is_path_like(token: &str) -> bool {
    if token.contains('/') {
        return true;
    }
    // `name.ext`: a dot with an alnum immediately before AND after it.
    let bytes: Vec<char> = token.chars().collect();
    for (i, c) in bytes.iter().enumerate() {
        if *c == '.' && i > 0 && i + 1 < bytes.len() {
            let before = bytes[i - 1];
            let after = bytes[i + 1];
            if before.is_alphanumeric() && after.is_alphanumeric() {
                return true;
            }
        }
    }
    false
}

/// Whether an item carries any deterministic observability signal: a digit, a
/// measurable keyword, or a path-like token. `lower` is the lowercased item (so
/// keyword matching is case-insensitive); `item` is the original (for tokens).
fn has_observability_signal(item: &str, lower: &str) -> bool {
    if item.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    if MEASURABLE_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return true;
    }
    item.split_whitespace().any(is_path_like)
}

/// Screen a charter's `definition_of_done` for non-observable wording. Pure and
/// deterministic. Returns one [`C3Flag`] per item that contains a vague token yet
/// carries no observability signal. Empty input => empty output.
pub fn c3_lexical_screen(dod_items: &[String]) -> Vec<C3Flag> {
    let mut flagged = Vec::new();
    for (index, item) in dod_items.iter().enumerate() {
        let lower = item.to_lowercase();
        let Some(tok) = VAGUE_TOKENS.iter().find(|t| lower.contains(**t)) else {
            continue; // no vague wording => nothing to flag.
        };
        if has_observability_signal(item, &lower) {
            continue; // vague, but a measurable signal is present => observable.
        }
        flagged.push(C3Flag {
            index,
            item: item.clone(),
            reason: format!("vague term '{tok}' with no measurable pass/fail signal"),
        });
    }
    flagged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_input_yields_no_flags() {
        assert!(c3_lexical_screen(&[]).is_empty());
    }

    #[test]
    fn vague_japanese_item_is_flagged() {
        let flags = c3_lexical_screen(&items(&["速くする"]));
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].index, 0);
        assert_eq!(flags[0].item, "速くする");
        assert!(flags[0].reason.contains("速く"));
    }

    #[test]
    fn vague_english_item_is_flagged_case_insensitively() {
        let flags = c3_lexical_screen(&items(&["Make it Clean"]));
        assert_eq!(flags.len(), 1);
        assert!(flags[0].reason.contains("clean"));
    }

    #[test]
    fn observable_command_is_not_flagged() {
        // names a test/command => observable, even without a digit.
        assert!(c3_lexical_screen(&items(&["cargo test -p x green"])).is_empty());
    }

    #[test]
    fn observable_number_is_not_flagged() {
        // a concrete metric => observable.
        assert!(c3_lexical_screen(&items(&["p95 < 200ms"])).is_empty());
    }

    #[test]
    fn observable_exit_condition_is_not_flagged() {
        // explicit pass/fail (exit + digit) => observable.
        assert!(c3_lexical_screen(&items(&["exit 0 を返す"])).is_empty());
    }

    #[test]
    fn vague_with_keyword_is_suppressed() {
        // vague adjective but a measurable keyword present => not flagged.
        assert!(c3_lexical_screen(&items(&["make the test suite clean"])).is_empty());
    }

    #[test]
    fn path_like_token_is_observable() {
        // names a file => observable even with vague wording.
        assert!(c3_lexical_screen(&items(&["improve src/main.rs"])).is_empty());
    }

    #[test]
    fn mixed_list_flags_only_the_vague_items() {
        let flags = c3_lexical_screen(&items(&[
            "速くする",              // flagged (index 0)
            "cargo test -p x green", // observable
            "きれいにする",          // flagged (index 2)
        ]));
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].index, 0);
        assert_eq!(flags[1].index, 2);
    }
}
