//! git long-option abbreviation resolution (backlog f63b97b3).
//!
//! git's option parser accepts any UNIQUE prefix of a long option
//! (`--dir=x` is `--directory=x`). A verdict that matches only the full
//! spelling is blind to the abbreviations. A prefix that matches several
//! options is a git error, but whether the command line really is one depends
//! on the command's full option table, so it is reported as [`Abbrev::Ambiguous`]
//! and callers resolve it restrictively (CLAUDE.md section 3).

/// The result of resolving one command-line word against a table of long
/// options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Abbrev<'a> {
    /// Not a long option, or it names none of the table's options.
    NotAnOption,
    /// Exactly one option of the table; `value` is the `=value` part, if any.
    Resolved {
        name: &'a str,
        value: Option<&'a str>,
    },
    /// The prefix fits more than one option of the table.
    Ambiguous,
}

/// Resolve `arg` (`--dir=x`, `--directory`, `--d`) against `known` (full
/// names with their leading `--`). An exact name always wins. A bare `--` or
/// a word that is not `--…` is [`Abbrev::NotAnOption`].
pub(super) fn resolve_long_abbrev<'a>(arg: &'a str, known: &[&'a str]) -> Abbrev<'a> {
    let Some(body) = arg.strip_prefix("--") else {
        return Abbrev::NotAnOption;
    };
    let (word, value) = match body.split_once('=') {
        Some((w, v)) => (w, Some(v)),
        None => (body, None),
    };
    if word.is_empty() {
        return Abbrev::NotAnOption;
    }
    let mut hits = known
        .iter()
        .copied()
        .filter(|k| k.strip_prefix("--").is_some_and(|n| n.starts_with(word)));
    let first = hits.next();
    let second = hits.next();
    if let Some(k) = known
        .iter()
        .copied()
        .find(|k| k.strip_prefix("--") == Some(word))
    {
        return Abbrev::Resolved { name: k, value };
    }
    match (first, second) {
        (None, _) => Abbrev::NotAnOption,
        (Some(name), None) => Abbrev::Resolved { name, value },
        (Some(_), Some(_)) => Abbrev::Ambiguous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &[&str] = &["--directory", "--apply", "--reject", "--reverse", "--stat"];

    #[test]
    fn unique_prefixes_resolve() {
        for a in ["--d", "--dir", "--direct=x", "--directory"] {
            assert!(matches!(
                resolve_long_abbrev(a, T),
                Abbrev::Resolved {
                    name: "--directory",
                    ..
                }
            ));
        }
        assert_eq!(
            resolve_long_abbrev("--dir=x", T),
            Abbrev::Resolved {
                name: "--directory",
                value: Some("x")
            }
        );
    }

    #[test]
    fn ambiguous_and_unknown() {
        assert_eq!(resolve_long_abbrev("--re", T), Abbrev::Ambiguous);
        assert_eq!(resolve_long_abbrev("--zzz", T), Abbrev::NotAnOption);
        assert_eq!(resolve_long_abbrev("--", T), Abbrev::NotAnOption);
        assert_eq!(resolve_long_abbrev("-d", T), Abbrev::NotAnOption);
    }
}
