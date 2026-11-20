//! A brace group that FEEDS a pipe reader: `{ echo 'echo hi'; } | bash`.
//!
//! The pipe splitter cuts a brace group apart: the closing `}` is the first
//! stage of the last statement and the reader is the stage after it, so the
//! stage "piping into" the reader is the bare word `}` and no text was read
//! (the line was an Ask, a Deny in a subagent, while `echo 'echo hi' | bash`
//! was Allow; backlog 7eb86bb3).
//!
//! The group's stdout is the concatenation of its members' stdout, so when
//! EVERY member is a plain `echo` or an evaluable `printf` and the members are
//! joined only by `;`, a newline or `&&`, that concatenation is the program —
//! the same model [`super::concatenated_outputs`] gives a process substitution
//! (`bash <(echo a; echo b)`). Any other member (`cat f`, an unquoted
//! expansion, a redirect, a nested pipe or group, `||`, `&`) leaves the group
//! unrecognised here, and the caller keeps its Ask.
//!
//! The operators between the pieces are read from `cmd` itself (the splitters
//! drop them, and `&&` / `||` look alike to [`super::super::SegmentSep`]): the
//! pieces are located in `cmd` in order and the text between two of them is
//! their separator. Pieces that cannot be located (a here-document body, say)
//! recognise no group.

use super::{
    backticks_as_dollar_paren, concatenated_outputs, evaluate_printf_stage, quoted_string_literals,
    PrintfStage,
};

/// One recognised group, as indices into the `pieces` given to
/// [`feeder_groups`].
pub(super) struct FeederGroup {
    /// The piece holding the opening `{` (and the first member).
    pub open: usize,
    /// The piece that is the closing `}`; the piece after it is the reader.
    pub close: usize,
    /// Every reading of the members' concatenated output
    /// ([`concatenated_outputs`]: one per printf dialect combination).
    pub concatenated: Vec<String>,
    /// What the reader receives as its program: `concatenated`, plus each
    /// member's own quoted literals and printf outputs, so a harmful member
    /// is judged on its own as well as in the whole. Non-blank entries only.
    pub texts: Vec<String>,
}

/// The brace groups among `pieces` (the stages of `cmd` as a splitter cut
/// them, in order) that feed the piece after their closing `}` and whose
/// members all have static output; see the module docs.
pub(super) fn feeder_groups(cmd: &str, pieces: &[&str]) -> Vec<FeederGroup> {
    // Blank pieces (the empty statement between `;` and a newline) carry no
    // text; their operators join the gap before the next real piece.
    let real: Vec<usize> = (0..pieces.len())
        .filter(|&k| !pieces[k].trim().is_empty())
        .collect();
    let Some(gaps) = gaps_before(cmd, real.iter().map(|&k| pieces[k].trim())) else {
        return Vec::new();
    };
    let text = |r: usize| pieces[real[r]].trim();
    let mut groups = Vec::new();
    for c in 1..real.len().saturating_sub(1) {
        if text(c) != "}" || gaps[c + 1] != "|" {
            continue;
        }
        let Some((open, members)) = members_back(&real, pieces, &gaps, c) else {
            continue;
        };
        let Ok(concatenated) = concatenated_outputs(&members) else {
            continue;
        };
        groups.push(FeederGroup {
            open: real[open],
            close: real[c],
            texts: program_texts(&members, &concatenated),
            concatenated,
        });
    }
    groups
}

/// For each statement of `statements` (split from `cmd`): the texts the
/// reader at its stage 1 receives as its program when its stage 0 is the `}`
/// of a recognised group ([`feeder_groups`]); `None` otherwise.
pub(super) fn group_feeder_texts(
    cmd: &str,
    statements: &[Vec<String>],
) -> Vec<Option<Vec<String>>> {
    let mut origin: Vec<(usize, usize)> = Vec::new();
    let mut pieces: Vec<&str> = Vec::new();
    for (s, statement) in statements.iter().enumerate() {
        for (i, stage) in statement.iter().enumerate() {
            origin.push((s, i));
            pieces.push(stage.as_str());
        }
    }
    let mut out = vec![None; statements.len()];
    for group in feeder_groups(cmd, &pieces) {
        let (s, i) = origin[group.close];
        if i == 0 {
            out[s] = Some(group.texts);
        }
    }
    out
}

/// `gaps[r]`: the operator characters (`;` `&` `|` newline) between real piece
/// `r - 1` and piece `r` in `cmd` (empty for `r == 0`). `None` when the pieces
/// are not found in `cmd` in order, separated by nothing but operators and
/// blanks.
fn gaps_before<'a>(cmd: &str, pieces: impl Iterator<Item = &'a str>) -> Option<Vec<String>> {
    let mut pos = 0;
    let mut gaps = Vec::new();
    for piece in pieces {
        let rest = cmd.get(pos..)?;
        let skipped = rest
            .find(|c: char| !(c.is_whitespace() || matches!(c, ';' | '&' | '|')))
            .unwrap_or(rest.len());
        if !rest[skipped..].starts_with(piece) {
            return None;
        }
        gaps.push(
            rest[..skipped]
                .chars()
                .filter(|c| matches!(c, ';' | '&' | '|' | '\n'))
                .collect::<String>(),
        );
        pos += skipped + piece.len();
    }
    Some(gaps)
}

/// Whether `gap` joins two members of a group: `;` and newlines, or (between
/// members, not before the `}`) `&&` followed by any newlines.
fn joins_members(gap: &str, before_close: bool) -> bool {
    let sequential = !gap.is_empty() && gap.chars().all(|c| matches!(c, ';' | '\n'));
    let and = gap
        .strip_prefix("&&")
        .is_some_and(|t| t.chars().all(|c| c == '\n'));
    sequential || (and && !before_close)
}

/// The members of the group closed by real piece `c`, in order, with the real
/// index of the piece that opens it: each piece back to the one whose first
/// word is `{` is one member, joined to the next by [`joins_members`].
/// `None` for a nested group, a pipe, `||`, `&` or no opener.
fn members_back(
    real: &[usize],
    pieces: &[&str],
    gaps: &[String],
    c: usize,
) -> Option<(usize, Vec<String>)> {
    let mut members = Vec::new();
    let mut r = c;
    loop {
        if !joins_members(&gaps[r], r == c) {
            return None;
        }
        r = r.checked_sub(1)?;
        let text = pieces[real[r]].trim();
        match text.split_whitespace().next() {
            Some("}") => return None,
            Some("{") => {
                let member = text["{".len()..].trim();
                if !member.is_empty() {
                    members.push(member.to_string());
                }
                break;
            }
            _ => members.push(text.to_string()),
        }
    }
    members.reverse();
    (!members.is_empty()).then_some((r, members))
}

/// The program texts a group with these `members` hands its reader.
fn program_texts(members: &[String], concatenated: &[String]) -> Vec<String> {
    let mut texts = concatenated.to_vec();
    for member in members {
        texts.extend(quoted_string_literals(&backticks_as_dollar_paren(member)));
        if let PrintfStage::Outputs(outputs) = evaluate_printf_stage(member) {
            texts.extend(outputs);
        }
    }
    let mut unique: Vec<String> = Vec::new();
    for text in texts {
        if !text.trim().is_empty() && !unique.contains(&text) {
            unique.push(text);
        }
    }
    unique
}
