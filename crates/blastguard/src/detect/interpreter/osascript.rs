//! The `osascript` application-delete rule of
//! [`super::analyze_code_interpreter`] (backlog 0fefcd3b, 3c750df2). Kept
//! apart from `interpreter.rs` only for the file-size cap.

use crate::model::Decision;

/// The applications whose `delete` / move-to-trash removes files: Finder and
/// System Events, by name and by bundle id (`tell application id "…"`).
/// Compared against a string literal's contents, case-insensitively.
const FILE_REMOVING_APPS: &[&str] = &[
    "finder",
    "system events",
    "com.apple.finder",
    "com.apple.systemevents",
];

/// Deny when an `osascript -e` / here-string program tells a file-removing
/// application to delete something; see [`osascript_program_delete`]. The
/// program is every argument shell-unquoted (the `-e` fragments, a
/// here-string body alike).
pub(super) fn osascript_app_delete(rest: &[&str]) -> Option<Decision> {
    let program: String = super::super::quote_aware_words(&rest.join(" "))
        .iter()
        .map(|w| super::super::first_shell_word(w).unwrap_or_else(|| w.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    osascript_program_delete(&[program])
}

/// Deny when an `osascript` program (AppleScript or JXA) removes files through
/// an application (backlog 0fefcd3b, 3c750df2; user ruling 2026-11-07: any
/// command harmful to the system is Deny through any wrapper, and a repeat
/// stays Deny, hence [`Decision::deny_system_harm`]).
///
/// `programs` are the program texts however they arrive: `-e` fragments, a
/// here-document body (quoted, unquoted or `-` form), or the literals an
/// `echo`/`printf` stage pipes in. The caller collects them; this judges text.
///
/// Refused when, OUTSIDE string literals, the program has a removing verb —
/// the word `delete`, the words `empty` … `trash`, or the word `move` followed
/// later by the word `trash` (move to trash) — AND either
///   * it names a [`FILE_REMOVING_APPS`] entry as a string literal (also
///     `tell application id "com.apple.finder"`, JXA `Application("Finder")`),
///     or
///   * some `application` / `app` reference has a target that is NOT one plain
///     string literal: a concatenation (`"Fin" & "der"`, `"Fin" + "der"`), a
///     variable, a call. blastguard cannot tell which application that is, so
///     it does not assume it is a harmless one (CLAUDE.md §3).
///
/// A verb that only appears inside a string (`display notification
/// "delete"`) does not count, and an application named by a plain literal
/// that is not file-removing (`tell application "Safari" to delete tab 1`) is
/// left to the program audit. Everything else returns `None`.
pub(in crate::detect) fn osascript_program_delete(programs: &[String]) -> Option<Decision> {
    // Lexed twice: with AppleScript's only string quote (`"`), and with JXA's
    // three. The program's language is not known here, and an apostrophe in
    // an AppleScript comment (`-- don't`) read as a JXA quote would hide every
    // later word — so a finding under EITHER lexing counts (the restrictive
    // side).
    let lexings = programs.iter().flat_map(|p| {
        let lowered = p.to_ascii_lowercase();
        [
            tokenize(&lowered, &['"']),
            tokenize(&lowered, &['"', '\'', '`']),
        ]
    });
    for toks in lexings {
        let Some(what) = removing_verb(&toks) else {
            continue;
        };
        if let Some(app) = toks.iter().find_map(|t| match t {
            Tok::Str(s) if FILE_REMOVING_APPS.contains(&s.as_str()) => Some(s.clone()),
            _ => None,
        }) {
            return Some(Decision::deny_system_harm(format!(
                "`osascript` tells {app} to {what} — that removes files (or the trash) without \
going through any command blastguard can judge, so it is refused (user ruling 2026-11-07: a \
command harmful to the system is denied through any wrapper)"
            )));
        }
        if let Some(target) = unresolved_application_target(&toks) {
            return Some(Decision::deny_system_harm(format!(
                "`osascript` program has `{what}` and addresses an application whose name is not \
one plain string literal (`{target}`) — blastguard cannot tell that it is not Finder or System \
Events removing files, so it is refused (user ruling 2026-11-07: a command harmful to the \
system is denied through any wrapper)"
            )));
        }
    }
    None
}

/// One lexical item of a lower-cased AppleScript / JXA program.
#[derive(Debug, PartialEq)]
enum Tok {
    /// A string literal's contents (`"…"`, and JXA's `'…'` / `` `…` ``),
    /// backslash escapes resolved.
    Str(String),
    /// A run of alphanumerics / `_`.
    Word(String),
    /// Any other non-space character.
    Punct(char),
}

/// Split `program` into [`Tok`]s, `quotes` opening string literals. An unterminated string runs to the end of
/// the text (it is still a string, so its words never count as verbs — the
/// app check on [`Tok::Str`] still sees it).
fn tokenize(program: &str, quotes: &[char]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut chars = program.chars().peekable();
    while let Some(c) = chars.next() {
        if quotes.contains(&c) {
            let mut s = String::new();
            while let Some(d) = chars.next() {
                if d == '\\' {
                    if let Some(e) = chars.next() {
                        s.push(e);
                    }
                } else if d == c {
                    break;
                } else {
                    s.push(d);
                }
            }
            out.push(Tok::Str(s));
        } else if c.is_alphanumeric() || c == '_' {
            let mut w = String::from(c);
            while let Some(&d) = chars.peek() {
                if d.is_alphanumeric() || d == '_' {
                    w.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok::Word(w));
        } else if !c.is_whitespace() {
            out.push(Tok::Punct(c));
        }
    }
    out
}

/// The removing verb the program spells outside its strings, if any.
fn removing_verb(toks: &[Tok]) -> Option<&'static str> {
    let words: Vec<&str> = toks
        .iter()
        .filter_map(|t| match t {
            Tok::Word(w) => Some(w.as_str()),
            _ => None,
        })
        .collect();
    if words.contains(&"delete") {
        return Some("delete");
    }
    if words
        .iter()
        .enumerate()
        .any(|(i, w)| *w == "empty" && words[i + 1..].iter().take(2).any(|n| *n == "trash"))
    {
        return Some("empty the trash");
    }
    if words
        .iter()
        .enumerate()
        .any(|(i, w)| *w == "move" && words[i + 1..].contains(&"trash"))
    {
        return Some("move to the trash");
    }
    None
}

/// The first `application` / `app` reference whose target is not one plain
/// string literal, as a short sample of the program from that point.
///
/// Not a reference (skipped): `application.` / `app.` (a property access such
/// as JXA `Application.currentApplication()` or a variable named `app`),
/// `app =` (an assignment to such a variable), and a word at the very end.
/// After the word, an `id` and a `(` are stepped over; then the target must be
/// a string literal that is not followed by `&` or `+` (concatenation).
fn unresolved_application_target(toks: &[Tok]) -> Option<String> {
    for (i, t) in toks.iter().enumerate() {
        let Tok::Word(w) = t else { continue };
        if w != "application" && w != "app" {
            continue;
        }
        let mut j = i + 1;
        match toks.get(j) {
            None | Some(Tok::Punct('.' | '=' | ',' | ';' | ')')) => continue,
            _ => {}
        }
        if toks.get(j) == Some(&Tok::Word("id".to_string())) {
            j += 1;
        }
        if toks.get(j) == Some(&Tok::Punct('(')) {
            j += 1;
        }
        let resolved = matches!(toks.get(j), Some(Tok::Str(_)))
            && !matches!(toks.get(j + 1), Some(Tok::Punct('&' | '+')));
        if !resolved {
            return Some(sample(&toks[i..]));
        }
    }
    None
}

/// A short readable rendering of the first few tokens.
fn sample(toks: &[Tok]) -> String {
    toks.iter()
        .take(6)
        .map(|t| match t {
            Tok::Str(s) => format!("\"{s}\""),
            Tok::Word(w) => w.clone(),
            Tok::Punct(c) => c.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::osascript_program_delete;

    fn denies(p: &str) -> bool {
        osascript_program_delete(&[p.to_string()]).is_some_and(|d| d.is_system_harm())
    }

    #[test]
    fn removing_verbs_on_file_removing_apps_deny() {
        assert!(denies(
            r#"tell application "Finder" to delete POSIX file "/etc/hosts""#
        ));
        assert!(denies(
            r#"tell application id "com.apple.finder" to delete POSIX file "/etc/hosts""#
        ));
        assert!(denies(
            r#"tell application "Finder" to move POSIX file "/etc/hosts" to trash"#
        ));
        assert!(denies(
            r#"Application("Finder").delete(Path("/etc/hosts"))"#
        ));
        assert!(denies(r#"tell app "Finder" to empty the trash"#));
    }

    #[test]
    fn unresolved_application_target_with_delete_denies() {
        assert!(denies(
            r#"tell application "Fin" & "der" to delete POSIX file "/etc/hosts""#
        ));
        assert!(denies("set a to x\ntell application a to delete item 1"));
        assert!(denies(r#"Application("Fin" + "der").delete(Path("/x"))"#));
    }

    #[test]
    fn benign_programs_are_not_judged_here() {
        for p in [
            r#"display notification "x""#,
            r#"display notification "delete""#,
            r#"tell application "Finder" to open POSIX file "/tmp""#,
            r#"tell application "Finder" to reveal POSIX file "/tmp""#,
            r#"tell application "Finder" to get name of every item of desktop"#,
            r#"tell application "Safari" to delete tab 1 of window 1"#,
            "var app = Application.currentApplication(); app.displayDialog('hi')",
        ] {
            assert!(osascript_program_delete(&[p.to_string()]).is_none(), "{p}");
        }
    }
}
