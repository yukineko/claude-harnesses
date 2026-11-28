//! The structural gate in front of every learned / rule Allow (spec
//! `docs/specs/blastguard-ask-learning.md`, "Lines an Allow can apply to").
//!
//! [`is_single_static_command`] is the ONE place that decides, on the brush
//! AST (never on the command text), whether a Bash line is
//!
//! 1. exactly one simple command (no pipeline, `;`, `&&`, `||`, `&`, `!`,
//!    `time`, compound command, function definition, subshell, command or
//!    process substitution, heredoc or here-string), and
//! 2. made only of static literal words: every word (the command word, the
//!    arguments and each redirect target) is plain text, `'..'` text or
//!    `".."` text with nothing in it that the shell would expand — no `$`
//!    expansion, `$'..'`, backquote, `$(..)`, `$((..))`, `~`, brace
//!    expansion, glob, or backslash escape — and no assignment stands before
//!    or among the words.
//!
//! Anything this function cannot show to be so is `false`: the Ask stands.
//! The class of the path arguments (no `..`, no symlink in the spelling) is
//! the other half of the gate and lives in `shape::class_of_path`.

use super::parse::{parse, parse_word};
use brush_parser::ast::{
    Command, CommandPrefixOrSuffixItem, IoFileRedirectKind, IoFileRedirectTarget, IoRedirect,
    SeparatorOperator, SimpleCommand, Word,
};
use brush_parser::word::WordPiece;

/// Characters that make UNQUOTED text more than a literal: pathname expansion,
/// brace expansion, extglob openers. (`~`, `$`, `` ` `` and `\` are separate
/// pieces or are refused below.)
const UNQUOTED_META: &[char] = &['*', '?', '[', ']', '{', '}', '(', ')', '\\', '`', '$'];

/// Whether `command` is one simple command whose every word is a static
/// literal. See the module docs.
pub(crate) fn is_single_static_command(command: &str) -> bool {
    let Ok(program) = parse(command) else {
        return false;
    };
    let [list] = program.complete_commands.as_slice() else {
        return false;
    };
    let [item] = list.0.as_slice() else {
        return false;
    };
    if !matches!(item.1, SeparatorOperator::Sequence) || !item.0.additional.is_empty() {
        return false;
    }
    let pipeline = &item.0.first;
    if pipeline.timed.is_some() || pipeline.bang {
        return false;
    }
    let [Command::Simple(simple)] = pipeline.seq.as_slice() else {
        return false;
    };
    simple_is_static(simple)
}

fn simple_is_static(c: &SimpleCommand) -> bool {
    let Some(name) = &c.word_or_name else {
        return false;
    };
    if !is_static_word(&name.value) {
        return false;
    }
    let prefix = c.prefix.iter().flat_map(|p| p.0.iter());
    let suffix = c.suffix.iter().flat_map(|s| s.0.iter());
    for item in prefix.chain(suffix) {
        let ok = match item {
            CommandPrefixOrSuffixItem::Word(w) => is_static_word(&w.value),
            CommandPrefixOrSuffixItem::IoRedirect(r) => redirect_is_static(r),
            // An assignment prefix / declaration word and a process
            // substitution are never static.
            CommandPrefixOrSuffixItem::AssignmentWord(..)
            | CommandPrefixOrSuffixItem::ProcessSubstitution(..) => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

fn redirect_is_static(r: &IoRedirect) -> bool {
    match r {
        IoRedirect::File(_, kind, target) => match (kind, target) {
            (
                IoFileRedirectKind::Read
                | IoFileRedirectKind::Write
                | IoFileRedirectKind::Append
                | IoFileRedirectKind::ReadAndWrite
                | IoFileRedirectKind::Clobber,
                IoFileRedirectTarget::Filename(w),
            ) => is_static_word(&w.value),
            // `2>&1`, `>&-`: a descriptor, never a file.
            (
                IoFileRedirectKind::DuplicateInput | IoFileRedirectKind::DuplicateOutput,
                IoFileRedirectTarget::Fd(_),
            ) => true,
            (
                IoFileRedirectKind::DuplicateInput | IoFileRedirectKind::DuplicateOutput,
                IoFileRedirectTarget::Duplicate(w),
            ) => is_static_word(&w.value) && is_fd_word(w),
            _ => false,
        },
        IoRedirect::OutputAndError(w, _) => is_static_word(&w.value),
        IoRedirect::HereDocument(..) | IoRedirect::HereString(..) => false,
    }
}

fn is_fd_word(w: &Word) -> bool {
    let v = w.value.trim();
    v == "-" || (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
}

/// Whether the shell word `raw` is a static literal: nothing in it is
/// expanded, split, globbed or unescaped. Decided on the brush word pieces.
pub(crate) fn is_static_word(raw: &str) -> bool {
    if raw.is_empty() {
        return false;
    }
    let Ok(pieces) = parse_word(raw) else {
        return false;
    };
    !pieces.is_empty()
        && pieces
            .iter()
            .all(|p| piece_is_static(&p.piece, /*quoted*/ false))
}

fn piece_is_static(p: &WordPiece, quoted: bool) -> bool {
    match p {
        WordPiece::Text(s) => {
            if quoted {
                // Inside `".."` a `$` / backquote / backslash that is NOT an
                // expansion piece is already literal, but keep the gate
                // simple: refuse them (an escape changes the value).
                !s.contains(['\\', '`', '$'])
            } else {
                !s.contains(UNQUOTED_META)
            }
        }
        WordPiece::SingleQuotedText(_) => true,
        WordPiece::DoubleQuotedSequence(inner) => {
            inner.iter().all(|i| piece_is_static(&i.piece, true))
        }
        // Every other piece is an expansion, an escape or `$'..'`.
        WordPiece::AnsiCQuotedText(_)
        | WordPiece::GettextDoubleQuotedSequence(_)
        | WordPiece::TildeExpansion(_)
        | WordPiece::ParameterExpansion(_)
        | WordPiece::CommandSubstitution(_)
        | WordPiece::BackquotedCommandSubstitution(_)
        | WordPiece::EscapeSequence(_)
        | WordPiece::ArithmeticExpression(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_simple_commands_are_static() {
        for c in [
            "git add -A",
            "git commit -m 'a b'",
            "git commit -m \"a b\"",
            "rm -rf /tmp/x/y",
            "git log > /tmp/o 2>&1",
            "git add -A # note",
            "git status;",
        ] {
            assert!(is_single_static_command(c), "{c}");
        }
    }

    #[test]
    fn everything_else_is_not() {
        for c in [
            "",
            "git add -A && git status",
            "git add -A; git status",
            "git add -A | cat",
            "git add -A &",
            "! git add -A",
            "for P in a; do git add -A; done",
            "(git add -A)",
            "{ git add -A; }",
            "f() { git add -A; }",
            "git add $(echo -A)",
            "git add `echo -A`",
            "git add <(true)",
            "PATH=/x git add -A",
            "git add $'\\x2dA'",
            "git add $X",
            "git add \"$X\"",
            "git add ~/x",
            "git add {-A,x}",
            "git add *.rs",
            "git add f?",
            "git add [a]",
            "git add -\\A",
            "git add $((1+1))",
            "cat <<EOF\nx\nEOF",
            "cat <<< x",
            "git add -A > $F",
            "git add -A > ~/f",
            "git add -A >&f",
            "export X=1",
            "if true; then git add -A; fi",
        ] {
            assert!(!is_single_static_command(c), "{c:?}");
        }
    }
}
