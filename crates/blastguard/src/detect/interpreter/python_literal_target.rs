//! A write target spelled as a Python literal whose value blastguard cannot
//! fully decode (backlog 47b20647): a `\N{…}` named escape of a NON-ASCII (or
//! unknown) name ([`Decoded::Holed`]) or a malformed escape
//! ([`Decoded::Undecodable`]). A `\N{…}` naming a printable ASCII character
//! (`\N{SOLIDUS}`, `\N{LATIN SMALL LETTER E}`) is decoded by
//! `python_escape` and never reaches this module.
//!
//! `open("\N{SOLIDUS}etc/hosts","w")` writes `/etc/hosts`; reading the body
//! undecoded (`\N{SOLIDUS}etc/hosts`, a relative path) made it Allow. An
//! undecoded literal is NOT a known value, so it never resolves to Allow on
//! the strength of its undecoded spelling:
//!
//! - each hole is one unknown code point that is not printable ASCII (the
//!   stand-ins below still include `/` and `.`, restrictively). Every way of filling the holes with
//!   a stand-in from [`HOLE_STAND_INS`] (`/`, `.`, and an ordinary file-name
//!   character) is judged by the system-directory checks the plain-literal
//!   path uses; ANY of them landing in a system directory is the program's
//!   Deny (a reading of the literal is system harm — user ruling: system harm
//!   is always Deny);
//! - a hole BEFORE a `/` is part of a directory name (`/\N{SNOWMAN}tc/hosts`)
//!   whose character the stand-ins cannot cover, so if no stand-in
//!   reading was denied the verdict is an Ask (Deny for a subagent), never
//!   Allow; so is a literal with more than [`MAX_HOLES`] holes;
//! - a hole only in the LAST path segment, after a directory spelled in full
//!   (`/tmp/\N{SNOWMAN}`), changes only the file name inside that directory,
//!   and the stand-in readings judged the directory: nothing is added;
//! - an undecodable literal has no value at all: an Ask.
//!
//! Only called for a program that writes somewhere (the caller gates on its
//! write call), as the plain-literal write-target check is.

use super::super::{Ctx, Decision};
use super::python_escape::{Decoded, HOLE};

/// Above this many holes in one literal the readings are not enumerated.
const MAX_HOLES: usize = 4;

/// What a hole is filled with, one reading each: a separator, a dot (a `..`
/// segment), and an ordinary file-name character.
const HOLE_STAND_INS: [char; 3] = ['/', '.', '_'];

/// The verdict a not-fully-decoded literal of a writing program adds, or
/// `None` when it adds nothing ([`Decoded::Value`], or holes confined to the
/// last segment of a path whose stand-in readings were not refused).
pub(super) fn unknown_literal_target_verdict(
    shape: &str,
    call: &str,
    decoded: &Decoded,
    ctx: &Ctx<'_>,
) -> Option<Decision> {
    let text = match decoded {
        Decoded::Value(_) => return None,
        Decoded::Undecodable => {
            return Some(Decision::ask(format!(
                "{shape}, and it writes (`{call}`) while one of its string literals holds an \
escape blastguard cannot decode — it cannot tell which path that literal names, so it refuses \
to guess"
            )))
        }
        Decoded::Holed(text) => text,
    };
    let shown = text.replace(HOLE, "\\N{…}");
    let holes = text.matches(HOLE).count();
    if holes > MAX_HOLES {
        return Some(Decision::ask(format!(
            "{shape}, and it writes (`{call}`) while its string literal `{shown}` holds {holes} \
`\\N{{…}}` escapes blastguard does not decode — it cannot tell which path that literal names"
        )));
    }
    let mut pending: Option<Decision> = None;
    for reading in readings(text, holes) {
        let candidate = collapse_slashes(reading.trim());
        if !candidate.contains('/') {
            continue;
        }
        let block = match super::super::system_path_block("interpreter write", &candidate, ctx) {
            Some(d) => Some(d),
            None => {
                match super::super::real_system_path_block("interpreter write", &candidate, ctx) {
                    Ok(d) => d,
                    Err(why) => Some(Decision::ask(format!(
                        "{shape}, and its string literal `{shown}` can name `{candidate}`, which \
could not be resolved through its symlinks ({why}) — blastguard cannot tell whether it lands in a \
system directory"
                    ))),
                }
            }
        };
        match block {
            Some(d) if d.is_deny() => {
                let reason = format!(
                    "{shape}, and it writes (`{call}`) a path spelled with a `\\N{{…}}` escape \
(`{shown}`) that can be `{candidate}`: {}",
                    reason_of(&d)
                );
                // The reason of a system-harm Deny already carries the mark.
                return Some(if d.is_system_harm() {
                    Decision::Deny(reason)
                } else {
                    Decision::deny_system_harm(reason)
                });
            }
            Some(ask) if pending.is_none() => pending = Some(ask),
            _ => {}
        }
    }
    if pending.is_none() && hole_in_directory_part(text) {
        pending = Some(Decision::ask(format!(
            "{shape}, and it writes (`{call}`) while its string literal `{shown}` has a \
`\\N{{…}}` escape blastguard does not decode inside a directory name — it cannot tell which \
directory that path lands in"
        )));
    }
    pending
}

/// Every filling of the `holes` holes of `text` with [`HOLE_STAND_INS`].
fn readings(text: &str, holes: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut seen = 0;
    for c in text.chars() {
        if c != HOLE {
            for r in &mut out {
                r.push(c);
            }
            continue;
        }
        seen += 1;
        out = out
            .iter()
            .flat_map(|r| {
                HOLE_STAND_INS.iter().map(move |s| {
                    let mut n = r.clone();
                    n.push(*s);
                    n
                })
            })
            .collect();
    }
    debug_assert_eq!(seen, holes);
    out
}

/// True when a hole sits before a `/`, i.e. inside a directory name.
fn hole_in_directory_part(text: &str) -> bool {
    text.find(HOLE).is_some_and(|at| text[at..].contains('/'))
}

/// `a//b` is `a/b` to the kernel; the system-directory checks compare
/// prefixes, so the reading is judged in its plain spelling.
fn collapse_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    out
}

fn reason_of(d: &Decision) -> &str {
    match d {
        Decision::Deny(r) | Decision::Ask(r) => r,
        Decision::Allow => "",
    }
}

#[cfg(test)]
mod tests {
    use super::{collapse_slashes, hole_in_directory_part, readings, HOLE};

    #[test]
    fn readings_fill_every_hole_with_every_stand_in() {
        let t = format!("{HOLE}etc/ho{HOLE}ts");
        let r = readings(&t, 2);
        assert_eq!(r.len(), 9);
        assert!(r.contains(&"/etc/ho/ts".to_string()));
        assert!(r.contains(&"_etc/ho.ts".to_string()));
    }

    #[test]
    fn hole_position_and_slashes() {
        assert!(hole_in_directory_part(&format!("/{HOLE}tc/hosts")));
        assert!(!hole_in_directory_part(&format!("/tmp/{HOLE}")));
        assert_eq!(collapse_slashes("//etc//hosts"), "/etc/hosts");
    }
}
