//! The `osascript` application-delete rule of
//! [`super::analyze_code_interpreter`] (backlog 0fefcd3b). Kept apart from
//! `interpreter.rs` only for the file-size cap; it is private to that module.

use crate::model::Decision;

/// Deny when an `osascript` program tells Finder or System Events to delete
/// something or to empty the trash (backlog 0fefcd3b, user ruling 2026-11-07:
/// any command harmful to the system is Deny through any wrapper). Before
/// this, `osascript -e 'tell application "Finder" to delete POSIX file
/// "/Users"'` and `… to delete every item of trash` were Allow: only
/// `do shell script` strings were analysed.
///
/// The program is every argument shell-unquoted (the `-e` fragments, a
/// here-document or here-string body alike). It is refused when it names the
/// application `"Finder"` or `"System Events"` (case-insensitive) AND, outside
/// its double-quoted string literals, has the word `delete` or the words
/// `empty` … `trash` (`empty trash`, `empty the trash`). A `delete` that only
/// appears inside a string (`display notification "delete"`) does not count.
/// JXA (`-l JavaScript`) `Application("Finder").delete(…)` matches the same
/// way. Everything else returns `None` and is judged by the program audit as
/// before.
pub(super) fn osascript_app_delete(rest: &[&str]) -> Option<Decision> {
    let program: String = super::super::quote_aware_words(&rest.join(" "))
        .iter()
        .map(|w| super::super::first_shell_word(w).unwrap_or_else(|| w.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    let lowered = program.to_ascii_lowercase();
    let app = [
        "\"finder\"",
        "\"system events\"",
        "'finder'",
        "'system events'",
    ]
    .into_iter()
    .find(|a| lowered.contains(a))?;
    let mut outside = String::with_capacity(lowered.len());
    let mut in_string = false;
    for c in lowered.chars() {
        if c == '"' {
            in_string = !in_string;
            outside.push(' ');
        } else if !in_string {
            outside.push(c);
        }
    }
    let words: Vec<&str> = outside
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    let deletes = words.contains(&"delete");
    let empties_trash = words
        .iter()
        .enumerate()
        .any(|(i, w)| *w == "empty" && words[i + 1..].iter().take(2).any(|n| *n == "trash"));
    if !(deletes || empties_trash) {
        return None;
    }
    let what = if deletes { "delete" } else { "empty the trash" };
    Some(Decision::deny(format!(
        "`osascript` tells {} to {what} — that removes files (or the trash) without going \
through any command blastguard can judge, so it is refused (user ruling 2026-11-07: a command \
harmful to the system is denied through any wrapper)",
        app.trim_matches(|c| c == '"' || c == '\'')
    )))
}
