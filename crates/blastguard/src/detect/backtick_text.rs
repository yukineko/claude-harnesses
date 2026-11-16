//! A backtick inside a double-quoted argument of `backlog add|edit` or
//! `git commit` (moved here unchanged from `detect.rs` so that file stays
//! under its file-growth cap; backlog 42741ea3).

use super::{command_candidates, git_subcommand_index, normalized_command, Decision};

/// A backtick inside a DOUBLE-quoted argument of `backlog add|edit` or
/// `git commit` is command substitution: the shell runs the span and replaces
/// it with its (usually empty) output, so the stored note / commit message
/// silently loses that text. Single-quoted backticks are literal and fine.
pub(super) fn backtick_in_double_quoted_text(seg: &str) -> Option<Decision> {
    // Cheap pre-filter: without both characters there is nothing to find, and
    // `command_candidates` below is too costly to run on every re-analysed
    // segment (the D4 budget test measures it).
    if !(seg.contains('`') && seg.contains('"')) {
        return None;
    }
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let target = command_candidates(&tokens).into_iter().find_map(|idx| {
        let cmd = normalized_command(tokens[idx]);
        let rest = &tokens[idx + 1..];
        match cmd.as_str() {
            "backlog" => rest
                .iter()
                .find(|t| !t.starts_with('-'))
                .map(|t| normalized_command(t))
                .filter(|s| s == "add" || s == "edit")
                .map(|s| format!("backlog {s}")),
            "git" => git_subcommand_index(rest)
                .map(|i| normalized_command(rest[i]))
                .filter(|s| s == "commit")
                .map(|_| "git commit".to_string()),
            _ => None,
        }
    })?;
    let (mut in_s, mut in_d, mut escaped) = (false, false, false);
    for c in seg.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if !in_s => escaped = true,
            '\'' if !in_d => in_s = !in_s,
            '"' if !in_s => in_d = !in_d,
            '`' if in_d => {
                return Some(Decision::ask(format!(
                    "a backtick inside a double-quoted argument of `{target}` is command \
substitution: the shell runs that span and silently deletes it from the stored text. Use single \
quotes (or escape the backtick) to keep it literal"
                )));
            }
            _ => {}
        }
    }
    None
}
