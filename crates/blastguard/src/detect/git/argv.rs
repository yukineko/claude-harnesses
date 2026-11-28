//! One git argv parser (backlog 4e7fc8e2): the words a git subcommand was given,
//! read the way git's parse-options reads them, against an option table.
//!
//! Every per-shape reader that skipped a value option, cut the line at `--`,
//! matched only the full long spelling, or read a clustered short flag as one
//! word was a fail-open (`git init -- D`, `--initial main D`, `-ko D`,
//! `--reas r D`, `--fil get k v`). They all go through [`parse`] now:
//!
//! * `--` ends the options: every later word is an operand.
//! * A long option resolves exactly, else by UNIQUE prefix ([`resolve_long_abbrev`]);
//!   `--no-X` negates `--X`. `--name=value` and `--name value` (value options).
//!   An ambiguous prefix is undetermined; so is an unknown `--x` unless the
//!   caller asked for a lenient read.
//! * A short cluster (`-ko`, `-ob`) is taken apart by the table; a value
//!   option takes the rest of the cluster, else the next word. An unknown
//!   short letter is undetermined (lenient: skipped).
//! * Redirect words are shell punctuation and never an option, value or operand.
//!
//! Undetermined is never read as "no target": the caller gets the reasons in
//! [`Parsed::undetermined`] and judges every word that could be a path.

use super::super::words::{is_bare_redirect_op, is_redirect_token};
use super::long_opt::{resolve_long_abbrev, Abbrev};

/// Whether an option takes a value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::detect) enum Arity {
    /// No value.
    Flag,
    /// `--name=V`, `--name V`, `-X V`, `-XV`.
    Value,
    /// A value only when glued (`--name=V`); never the next word.
    Inline,
}

/// One row of an option table.
#[derive(Clone, Copy)]
pub(in crate::detect) struct Opt {
    /// Canonical name without dashes. `--NAME` is the long spelling when
    /// `long` is set.
    pub name: &'static str,
    pub long: bool,
    pub short: Option<char>,
    pub arity: Arity,
}

impl Opt {
    /// A flag with a long spelling and optionally a short one.
    pub(in crate::detect) const fn flag(name: &'static str, short: Option<char>) -> Opt {
        Opt {
            name,
            long: true,
            short,
            arity: Arity::Flag,
        }
    }
    /// A value option with a long spelling and optionally a short one.
    pub(in crate::detect) const fn val(name: &'static str, short: Option<char>) -> Opt {
        Opt {
            name,
            long: true,
            short,
            arity: Arity::Value,
        }
    }
    /// A long option whose value can only be glued with `=`.
    pub(in crate::detect) const fn inline(name: &'static str) -> Opt {
        Opt {
            name,
            long: true,
            short: None,
            arity: Arity::Inline,
        }
    }
    /// A short-only flag.
    pub(in crate::detect) const fn sflag(name: &'static str, short: char) -> Opt {
        Opt {
            name,
            long: false,
            short: Some(short),
            arity: Arity::Flag,
        }
    }
    /// A short-only option whose value can only be glued (`-M50%`).
    pub(in crate::detect) const fn sinline(name: &'static str, short: char) -> Opt {
        Opt {
            name,
            long: false,
            short: Some(short),
            arity: Arity::Inline,
        }
    }
    /// A short-only value option.
    pub(in crate::detect) const fn sval(name: &'static str, short: char) -> Opt {
        Opt {
            name,
            long: false,
            short: Some(short),
            arity: Arity::Value,
        }
    }
}

/// How to read an argv.
#[derive(Clone, Copy)]
pub(in crate::detect) struct Spec<'t> {
    /// Option tables, searched together (a shared table plus a subcommand's).
    pub tables: &'t [&'t [Opt]],
    /// An unknown option is skipped instead of undetermined. Only for readers
    /// that look for a few NAMED options and take no operand (the generic
    /// `--output` scan).
    pub lenient: bool,
    /// `-1`, `-23`: digit short options (`format-patch -2`) are flags.
    pub digits: bool,
}

/// One option as written: its canonical name and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::detect) struct Hit<'a> {
    pub name: &'static str,
    /// `None` for a flag, an inline option without `=`, and a value option
    /// that was the last word (the value is missing).
    pub value: Option<&'a str>,
    /// Written as `--no-NAME`.
    pub negated: bool,
}

/// The reading of one argv.
#[derive(Debug, Default)]
pub(in crate::detect) struct Parsed<'a> {
    pub hits: Vec<Hit<'a>>,
    pub operands: Vec<&'a str>,
    /// Why part of the argv could not be read. Never empty-means-fine: a
    /// non-empty list means [`Parsed::operands`] / [`Parsed::hits`] may be
    /// incomplete or shifted.
    pub undetermined: Vec<String>,
}

impl<'a> Parsed<'a> {
    /// The non-negated hits of option `name`.
    pub(in crate::detect) fn hits_of<'s>(
        &'s self,
        name: &'static str,
    ) -> impl Iterator<Item = &'s Hit<'a>> + 's {
        self.hits
            .iter()
            .filter(move |h| h.name == name && !h.negated)
    }
    /// Whether option `name` was given (not negated).
    pub(in crate::detect) fn has(&self, name: &'static str) -> bool {
        self.hits_of(name).next().is_some()
    }
}

fn find_short<'t>(spec: &Spec<'t>, c: char) -> Option<&'t Opt> {
    spec.tables
        .iter()
        .flat_map(|t| t.iter())
        .find(|o| o.short == Some(c))
}

enum Long<'t> {
    Found(&'t Opt),
    Ambiguous,
    Unknown,
}

fn find_long<'t>(spec: &Spec<'t>, word: &str) -> Long<'t> {
    let longs: Vec<(&'t Opt, String)> = spec
        .tables
        .iter()
        .flat_map(|t| t.iter())
        .filter(|o| o.long)
        .map(|o| (o, format!("--{}", o.name)))
        .collect();
    let names: Vec<&str> = longs.iter().map(|(_, n)| n.as_str()).collect();
    match resolve_long_abbrev(word, &names) {
        Abbrev::Resolved { name, .. } => {
            match longs.iter().find(|(_, n)| n == name).map(|(o, _)| *o) {
                Some(o) => Long::Found(o),
                None => Long::Unknown,
            }
        }
        Abbrev::Ambiguous => Long::Ambiguous,
        Abbrev::NotAnOption => Long::Unknown,
    }
}

/// Read `args` (the words after the subcommand) against `spec`.
pub(in crate::detect) fn parse<'a>(args: &[&'a str], spec: &Spec<'_>) -> Parsed<'a> {
    let mut out = Parsed::default();
    let mut i = 0;
    while i < args.len() {
        let t = args[i];
        i += 1;
        if t.is_empty() {
            continue;
        }
        if is_bare_redirect_op(t) {
            // The operator owns the next word.
            i += 1;
            continue;
        }
        if is_redirect_token(t) {
            continue;
        }
        if t == "--" {
            out.operands.extend(args[i..].iter().copied());
            break;
        }
        if let Some(body) = t.strip_prefix("--") {
            parse_long(t, body, args, &mut i, spec, &mut out);
        } else if t.len() > 1 && t.starts_with('-') {
            parse_short(t, args, &mut i, spec, &mut out);
        } else {
            out.operands.push(t);
        }
    }
    out
}

fn parse_long<'a>(
    t: &'a str,
    body: &'a str,
    args: &[&'a str],
    i: &mut usize,
    spec: &Spec<'_>,
    out: &mut Parsed<'a>,
) {
    let mut negated = false;
    let inline: Option<&'a str> = body.split_once('=').map(|(_, v)| v);
    let mut found = find_long(spec, t);
    if matches!(found, Long::Unknown) {
        if let Some(rest) = body.strip_prefix("no-") {
            // `--no-X` negates `--X`; resolve against the stripped spelling.
            if let Long::Found(o) = find_long(spec, &format!("--{rest}")) {
                found = Long::Found(o);
                negated = true;
            }
        }
    }
    match found {
        Long::Found(o) => {
            let mut value = None;
            match o.arity {
                Arity::Flag => {
                    if inline.is_some() && !negated {
                        out.undetermined
                            .push(format!("`{t}`: option `--{}` takes no value", o.name));
                    }
                }
                Arity::Inline => value = inline,
                Arity::Value => {
                    if negated {
                        // `--no-X` never takes a value.
                    } else if let Some(v) = inline {
                        value = Some(v);
                    } else if let Some(v) = args.get(*i) {
                        value = Some(*v);
                        *i += 1;
                    }
                }
            }
            out.hits.push(Hit {
                name: o.name,
                value,
                negated,
            });
        }
        Long::Ambiguous => out
            .undetermined
            .push(format!("`{t}` is an ambiguous abbreviation")),
        Long::Unknown => {
            if !spec.lenient {
                out.undetermined
                    .push(format!("`{t}` is not an option blastguard knows"));
            }
        }
    }
}

fn parse_short<'a>(
    t: &'a str,
    args: &[&'a str],
    i: &mut usize,
    spec: &Spec<'_>,
    out: &mut Parsed<'a>,
) {
    let cluster = &t[1..];
    for (pos, c) in cluster.char_indices() {
        let tail = &cluster[pos + c.len_utf8()..];
        match find_short(spec, c) {
            Some(o) if o.arity == Arity::Value => {
                let value = if !tail.is_empty() {
                    Some(tail)
                } else if let Some(v) = args.get(*i) {
                    *i += 1;
                    Some(*v)
                } else {
                    None
                };
                out.hits.push(Hit {
                    name: o.name,
                    value,
                    negated: false,
                });
                return;
            }
            Some(o) if o.arity == Arity::Inline => {
                // `-M50%`: the rest of the cluster is the value.
                out.hits.push(Hit {
                    name: o.name,
                    value: (!tail.is_empty()).then_some(tail),
                    negated: false,
                });
                return;
            }
            Some(o) => out.hits.push(Hit {
                name: o.name,
                value: None,
                negated: false,
            }),
            None if spec.digits && c.is_ascii_digit() => {}
            None => {
                if !spec.lenient {
                    out.undetermined.push(format!(
                        "`{t}`: short option `-{c}` is not one blastguard knows"
                    ));
                    // Where the rest of the cluster goes is unknown.
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &[Opt] = &[
        Opt::val("output-directory", Some('o')),
        Opt::flag("keep-subject", Some('k')),
        Opt::val("reason", None),
        Opt::flag("lock", None),
        Opt::inline("shared"),
        Opt::sflag("q", 'q'),
    ];
    const SPEC: Spec<'static> = Spec {
        tables: &[T],
        lenient: false,
        digits: true,
    };

    #[test]
    fn double_dash_operands_are_kept() {
        let p = parse(&["-q", "--", "a", "-b"], &SPEC);
        assert_eq!(p.operands, vec!["a", "-b"]);
        assert!(p.undetermined.is_empty());
    }

    #[test]
    fn abbreviated_value_option_consumes_its_value() {
        let p = parse(&["--lock", "--reas", "r", "D"], &SPEC);
        assert_eq!(p.operands, vec!["D"]);
        assert_eq!(p.hits_of("reason").next().unwrap().value, Some("r"));
    }

    #[test]
    fn clustered_short_value_takes_next_word_or_rest() {
        let p = parse(&["-ko", "D", "x"], &SPEC);
        assert_eq!(p.operands, vec!["x"]);
        assert_eq!(
            p.hits_of("output-directory").next().unwrap().value,
            Some("D")
        );
        let p = parse(&["-koD"], &SPEC);
        assert_eq!(
            p.hits_of("output-directory").next().unwrap().value,
            Some("D")
        );
        assert!(p.has("keep-subject"));
    }

    #[test]
    fn unknown_and_ambiguous_are_undetermined() {
        assert!(!parse(&["--zzz"], &SPEC).undetermined.is_empty());
        assert!(!parse(&["-z"], &SPEC).undetermined.is_empty());
        let lenient = Spec {
            lenient: true,
            ..SPEC
        };
        assert!(parse(&["--zzz", "-z"], &lenient).undetermined.is_empty());
    }

    #[test]
    fn inline_option_never_takes_the_next_word() {
        let p = parse(&["--shared", "D"], &SPEC);
        assert_eq!(p.operands, vec!["D"]);
        let p = parse(&["--shared=group", "D"], &SPEC);
        assert_eq!(p.hits_of("shared").next().unwrap().value, Some("group"));
    }

    #[test]
    fn negation_and_digits_and_redirects() {
        let p = parse(&["--no-lock", "-3", ">", "f", "D"], &SPEC);
        assert_eq!(p.operands, vec!["D"]);
        assert!(!p.has("lock"));
        assert!(p.undetermined.is_empty());
    }
}
