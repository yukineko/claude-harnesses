use super::{
    basename, chmod_mode_and_targets, compound_views, has_unresolvable_expansion, is_short_flag,
    normalized_command, positional_operands, split_segments, SegmentView,
};
use crate::exclude;
use crate::model::Decision;
use std::collections::HashMap;

pub(super) fn verb_operands_are_rewritten(verb: &str) -> bool {
    matches!(verb, "rm" | "unlink" | "rmdir" | "chmod")
}

/// The shell working-directory PREFIX this analysis has tracked so far, built
/// up left-to-right across the `;`/`&&`/`||`/`|`/`&`-separated segments of ONE
/// command line (see the loop in [`detect_bash`]) so a later segment's
/// RELATIVE operand (bare basename or `./`/`../`-qualified) can be judged in
/// the directory it actually resolves inside, not just as literal text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CwdState {
    /// No `cd`/`pushd` has been seen yet: judge relative operands exactly as
    /// before this fix (no prefix to apply).
    Root,
    /// A `cd`/`pushd` (or a `cd` through a `ln -s`-created alias) this
    /// analysis could resolve LEXICALLY to a literal directory — relative or
    /// absolute, `..` collapsed via [`exclude::normalize`].
    Known(String),
    /// A directory change occurred but its target could not be resolved
    /// statically (`cd $VAR`, bare `cd -`, `cd ~`, or a `popd` — this
    /// analysis tracks only the CURRENT directory, not a full stack, so what
    /// a `popd` returns to is not knowable here). Per CLAUDE.md §3
    /// "cannot determine" resolves to the RESTRICTIVE side, never `Allow` —
    /// see the `CwdState::Unknown` arm of [`advance_cwd_and_rewrite`].
    Unknown,
}

/// Strip a leading run of `(` and a trailing run of `)` from a single
/// already-whitespace-split token.
///
/// `split_segments` splits on `;`/`&&`/`||`/`|`/`&` but has no notion of a
/// parenthesised subshell, so `(cd .githooks && rm pre-commit)` splits into
/// the segments `"(cd .githooks "` and `" rm pre-commit)"` — the subshell
/// punctuation lands glued to the first and last WORD of the group, not as
/// its own token. This is applied only to the handful of tokens this cwd-
/// tracking pass itself inspects (a segment's head word, a `cd`/`pushd`
/// target, an `ln -s` operand, a rewritten relative operand) — never to
/// the general tokeniser `analyze_segment` uses, so it cannot change how any
/// PRE-EXISTING rule reads a token.
pub(super) fn strip_edge_parens(tok: &str) -> &str {
    let mut t = tok;
    while let Some(rest) = t.strip_prefix('(') {
        t = rest;
    }
    while let Some(rest) = t.strip_suffix(')') {
        t = rest;
    }
    t
}

/// The operand TARGETS a relative-operand rewrite (or the Undetermined-cwd
/// Ask) cares about for `head`: the same set [`rewrite_relative_operands`]
/// would substitute, computed the same way the verb's own rule arm in
/// `analyze_command_at` computes its operands, so the two definitions cannot
/// drift apart. Every other verb — anything this cwd-tracking pass has no
/// opinion about — gets an empty set, which is a no-op for both callers.
pub(super) fn verb_targets<'a>(head: &str, rest: &[&'a str]) -> Vec<&'a str> {
    match head {
        "rm" => rest
            .iter()
            .filter(|t| !t.starts_with('-'))
            .copied()
            .collect(),
        "unlink" | "rmdir" => positional_operands(rest, &[]),
        "chmod" => chmod_mode_and_targets(rest).1,
        _ => Vec::new(),
    }
}

/// True when `tok` is a RELATIVE filesystem operand — neither ABSOLUTE
/// (`/etc/x`) nor home-relative (`~/x`; left alone, this analysis has no
/// notion of `$HOME`).
///
/// BG-cwd-2 (residual, verified bypass on a571c683): the first cut of this
/// rewrite only treated a BARE basename (no `/` at all, e.g. `pre-commit`) as
/// eligible, on the reasoning that anything already carrying a `/` was
/// "already pathful" and therefore already judged correctly. That reasoning
/// is only true for a path relative to the shell's REAL starting directory —
/// it is false for one relative to the tracked cwd: `cd .githooks && rm
/// ./pre-commit` still evaluated `./pre-commit` from `Root` (no protected
/// glob matches it) even though the direct form `rm .githooks/./pre-commit`
/// was already denied. `sub/../pre-commit` is the same hole spelled with a
/// real subdirectory instead of `.`. A bare basename is just the ZERO-`/`
/// case of "relative operand", not a different question — this predicate
/// covers both, and [`rewrite_relative_operands`] treats them identically.
pub(super) fn is_relative_operand(tok: &str) -> bool {
    !tok.is_empty() && !tok.starts_with('/') && !tok.starts_with('~')
}

/// Rewrite every RELATIVE target operand of `seg` — for the verbs
/// [`verb_targets`] has an opinion about — into `dir` joined with the
/// operand and lexically normalized (`.`/`..` resolved via
/// [`exclude::normalize`]), so it is judged against the protected-path rules
/// exactly as the direct-path form (`rm .githooks/pre-commit`) already is.
/// This covers a bare basename (`pre-commit`), a `./`-prefixed one
/// (`./pre-commit`), and one that navigates back INTO the tracked directory
/// (`sub/../pre-commit`) identically — see [`is_relative_operand`]. An
/// operand that navigates back OUT of the tracked directory into somewhere
/// else (`../src/build.o`) normalizes to that somewhere-else path and is
/// judged there, which is correctly `Allow` when that somewhere-else is not
/// itself protected — this rewrite does not make an escaping relative
/// operand MORE suspicious than the same path typed directly would be.
///
/// Non-target tokens (flags, a chmod MODE) and operands carrying an
/// unresolvable shell expansion (`$VAR`, `` $(...) ``, backtick — nothing
/// about their eventual value is known statically, so nothing safe can be
/// joined against `dir`) are left untouched. Absolute and `~`-relative
/// operands are also left untouched: [`is_relative_operand`] is false for
/// them, so they fall through to the raw-token branch below and stay judged
/// as-is, exactly as before this fix.
///
/// A stray subshell `)` glued to the last token (see [`strip_edge_parens`])
/// is dropped from the rewritten path rather than carried into it — the
/// rewrite is meant to reproduce the direct-path form exactly, not a
/// direct-path-form-plus-punctuation that only some protected globs (the
/// wildcard-terminated ones) are lenient enough to still match.
pub(super) fn rewrite_relative_operands(seg: &str, dir: &str) -> String {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let Some((head_tok, rest)) = tokens.split_first() else {
        return seg.to_string();
    };
    let head = normalized_command(strip_edge_parens(head_tok));
    let targets = verb_targets(&head, rest);
    let mut out = String::from(*head_tok);
    for t in rest {
        out.push(' ');
        if targets.contains(t) {
            let clean = strip_edge_parens(t);
            if !clean.is_empty() && is_relative_operand(clean) && !has_unresolvable_expansion(clean)
            {
                out.push_str(&exclude::normalize(&format!("{dir}/{clean}")));
                continue;
            }
        }
        out.push_str(t);
    }
    out
}

/// Resolve a `cd`/`pushd` target token (or an `ln -s` source) to the
/// directory it names, relative to the cwd already tracked — or to
/// [`CwdState::Unknown`] when it cannot be resolved statically.
///
/// A LITERAL alias recorded by an earlier `ln -s SRC DEST` (see
/// [`advance_cwd_and_rewrite`]) is consulted FIRST: `cd` through a symlink
/// whose target is a protected directory must land on the same `Known(dir)`
/// a direct `cd` into that directory would, so a relative operand in a
/// LATER segment is judged the same way either route got there.
pub(super) fn resolve_dir_token(
    tok: &str,
    cwd: &CwdState,
    aliases: &HashMap<String, String>,
) -> CwdState {
    let tok = strip_edge_parens(tok);
    if let Some(target) = aliases.get(tok) {
        return CwdState::Known(target.clone());
    }
    if tok.starts_with('/') {
        return CwdState::Known(exclude::normalize(tok));
    }
    let joined = match cwd {
        CwdState::Known(dir) => format!("{dir}/{tok}"),
        CwdState::Root => tok.to_string(),
        CwdState::Unknown => return CwdState::Unknown,
    };
    CwdState::Known(exclude::normalize(&joined))
}

/// One step of the left-to-right cwd walk [`detect_bash`] runs across a
/// command line's segments: update `cwd`/`aliases` for a `cd`/`pushd`/`popd`/
/// `ln -s`, or — for any other segment — return it (rewritten, if the tracked
/// cwd is [`CwdState::Known`]) unchanged for [`analyze_segment`] to judge as
/// before, plus an optional EXTRA verdict for the [`CwdState::Unknown`]
/// restrictive case (CLAUDE.md §3: "cannot determine" is not `Allow`).
///
/// Returns `(segment to hand to analyze_segment, an extra verdict to record,
/// the cwd state for the NEXT segment)`.
pub(super) fn advance_cwd_and_rewrite(
    seg: &str,
    cwd: &CwdState,
    aliases: &mut HashMap<String, String>,
) -> (String, Option<Decision>, CwdState) {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let Some((head_tok, rest)) = tokens.split_first() else {
        return (seg.to_string(), None, cwd.clone());
    };
    let head = normalized_command(strip_edge_parens(head_tok));
    let head = head.as_str();

    // `ln -s SRC DEST`: record a static symlink alias so a LATER `cd DEST`
    // resolves through it to SRC, exactly like `cd SRC` would. Only a
    // LITERAL (non-expansion) SRC and DEST are tracked; a dynamic name on
    // either side just leaves the alias table alone — a later `cd` on that
    // name is then judged as an ordinary path (or Unknown), never as
    // Allow-by-omission.
    if head == "ln" {
        if rest
            .iter()
            .any(|t| *t == "-s" || (is_short_flag(t) && t.contains('s')))
        {
            let operands = positional_operands(rest, &[]);
            if let (Some(src), Some(dest)) = (operands.first(), operands.last()) {
                if operands.len() >= 2
                    && !has_unresolvable_expansion(src)
                    && !has_unresolvable_expansion(dest)
                {
                    if let CwdState::Known(dir) = resolve_dir_token(src, cwd, aliases) {
                        let dest_name = basename(strip_edge_parens(dest)).to_string();
                        aliases.insert(dest_name, dir);
                    }
                }
            }
        }
        return (seg.to_string(), None, cwd.clone());
    }

    if matches!(head, "cd" | "pushd") {
        let target = rest.iter().find(|t| !t.starts_with('-')).copied();
        let new_cwd = match target {
            None => CwdState::Unknown,
            Some(t) => {
                let clean = strip_edge_parens(t);
                if clean.is_empty()
                    || clean == "-"
                    || clean.starts_with('~')
                    || has_unresolvable_expansion(clean)
                {
                    CwdState::Unknown
                } else {
                    resolve_dir_token(clean, cwd, aliases)
                }
            }
        };
        return (seg.to_string(), None, new_cwd);
    }
    if head == "popd" {
        // `popd` returns to whatever the matching `pushd` displaced. This
        // pass tracks only the CURRENT top of the directory stack, not the
        // whole stack, so what it returns to is not knowable here — treated
        // as Unknown rather than silently reverting to `Root`, which could
        // wrongly clear a still-live protected prefix.
        return (seg.to_string(), None, CwdState::Unknown);
    }

    match cwd {
        CwdState::Known(dir) => (rewrite_relative_operands(seg, dir), None, cwd.clone()),
        CwdState::Unknown => {
            let targets = verb_targets(head, rest);
            if targets
                .iter()
                .any(|t| is_relative_operand(strip_edge_parens(t)))
            {
                let ask = Decision::ask(format!(
                    "`{head}` operates on a RELATIVE path after an earlier cd/pushd whose \
target directory blastguard could not statically resolve — it cannot tell whether this reaches \
a protected gate/config path, and refuses to guess"
                ));
                (seg.to_string(), Some(ask), cwd.clone())
            } else {
                (seg.to_string(), None, cwd.clone())
            }
        }
        CwdState::Root => (seg.to_string(), None, cwd.clone()),
    }
}

/// The [`CwdState`] in force BEFORE each segment of `split_segments(cmd)`,
/// replaying exactly the walk [`detect_bash`]'s per-segment loop performs
/// (same alias table; `cmd` is the text [`strip_data_here_document_bodies`]
/// already left, so a data body is absent here exactly as it is there).
///
/// The line-level redirect scans run BEFORE that loop, so without this they
/// had no per-segment base at all and judged `cd /etc && echo x > paths.d/f`
/// as the relative text `paths.d/f`, which no absolute-path axis can match
/// (f1c170ab).
pub(super) fn segment_cwd_states(cmd: &str) -> Vec<CwdState> {
    let segs = split_segments(cmd);
    let mut cwd = CwdState::Root;
    let mut aliases: HashMap<String, String> = HashMap::new();
    // The same compound-syntax view the loop uses, so `do cd .githooks` moves
    // the directory here exactly as it does there.
    let views = compound_views(cmd);
    let mut scopes = SubshellCwdScopes::new(&views);
    let mut out = Vec::with_capacity(segs.len());
    for (seg_idx, seg) in segs.iter().enumerate() {
        out.push(cwd.clone());
        let command = views
            .get(seg_idx)
            .map_or(seg.as_str(), |v| v.command.as_str());
        scopes.enter(views.get(seg_idx));
        let (_, _, next_cwd) = advance_cwd_and_rewrite(command, &cwd, &mut aliases);
        let (after, closed_dirty) = scopes.leave(views.get(seg_idx), &out[seg_idx], next_cwd);
        // `( cd /tmp; true ) > hosts`: the redirect may belong to the group
        // (outer cwd) or sit inside it (inner cwd); see `SubshellCwdScopes`.
        if closed_dirty {
            out[seg_idx] = CwdState::Unknown;
        }
        cwd = after;
    }
    out
}

/// What the cwd tracking knows after a `(`-group closes.
///
/// 9778927f: once [`compound_views`] removed a subshell's parens, a `cd`
/// inside `( cd /tmp )` was read as a `cd` in THIS shell, so a later relative
/// write that really lands in the OUTER directory
/// (`cd /etc && ( cd /tmp ) && echo x > hosts`) was placed in `/tmp` and
/// ALLOWED, while main denied it. Restoring the outer directory exactly was
/// tried and dropped (user ruling 2026-10-15): the group count comes from raw
/// `(`/`)` text, which a `#` comment, a here-document body or `${x:-(}` can
/// skew, and a skewed count restored the WRONG directory — Allow again.
///
/// So the rule is deliberately coarse and fail-closed: when a group contains
/// any cwd change, the cwd after it closes is [`CwdState::Unknown`] — neither
/// the inner directory nor a restored outer one — and every later relative
/// operand is judged as unknown-cwd (Ask, which hardens to Deny outside an
/// interactive session). A group with no cwd change leaves the cwd as it was,
/// which is exactly right because nothing in it moved.
///
/// The count is NOT trustworthy, and the rules below are what keeps a
/// miscount from producing a Known-and-wrong cwd. The first version (one
/// "dirty" flag per group, Unknown only when the popped group was dirty) was
/// measured ALLOWING 12 lines main denied (`subshell_cd_miscount_9778927f`):
/// a fake `(` opened AFTER the `cd` was clean and absorbed the real `)`; a `(`
/// inside `"$(echo "(")"` swallowed the real `)`; and a redirect glued to the
/// group (`( cd /tmp; true ) > hosts`) was placed in the inner directory.
///
/// - A `)` makes the cwd Unknown when the group it pops OR any group still
///   open had a cwd change in it, so a fake inner group cannot absorb the
///   close of a real dirty one.
/// - A line whose `(`/`)` counts do not balance (more closes than opens at any
///   point, or groups left open at the end) has a group structure this
///   bookkeeping does not know. On such a line, ANY cwd change makes the cwd
///   Unknown for the rest of the line (sticky, even past a later absolute
///   `cd`, which may itself sit in an unseen group).
/// - The segment whose `)` closes a dirty group has its OWN redirect placement
///   Unknown too ([`segment_cwd_states`]): a redirect in that segment may be
///   inside the group (inner cwd) or attached to it (outer cwd), and the text
///   does not say which this bookkeeping can trust.
///
/// Residual, not closed: a line where a real `(` is NOT recognised as a group
/// opener AND a fake `(` elsewhere restores the balance can still mis-scope.
/// Every leading `(` in command position (after `!`, `{`, `then`, …) is
/// recognised, so this needs a construct `compound_views` does not strip.
pub(super) struct SubshellCwdScopes {
    /// One entry per open group: whether a cwd change happened inside it.
    dirty: Vec<bool>,
    /// The line's paren counts do not balance; see the type docs.
    unbalanced: bool,
    /// A cwd change happened on an unbalanced line: every later cwd is Unknown.
    tainted: bool,
}

impl SubshellCwdScopes {
    /// Bookkeeping for the line whose views are `views`.
    pub(super) fn new(views: &[SegmentView]) -> Self {
        let mut depth: usize = 0;
        let mut unbalanced = false;
        for v in views {
            depth += v.opens;
            if v.closes > depth {
                unbalanced = true;
                depth = 0;
            } else {
                depth -= v.closes;
            }
        }
        Self {
            dirty: Vec::new(),
            unbalanced: unbalanced || depth != 0,
            tainted: false,
        }
    }

    /// Call BEFORE `view`'s segment runs.
    pub(super) fn enter(&mut self, view: Option<&SegmentView>) {
        for _ in 0..view.map_or(0, |v| v.opens) {
            self.dirty.push(false);
        }
    }

    /// Call AFTER `view`'s segment ran, with the cwd before and after it.
    /// Returns the cwd in force for the next segment, and whether this
    /// segment closed a group with a cwd change in it (its own redirects then
    /// have no trustworthy base).
    pub(super) fn leave(
        &mut self,
        view: Option<&SegmentView>,
        cwd_before: &CwdState,
        cwd_after: CwdState,
    ) -> (CwdState, bool) {
        if &cwd_after != cwd_before {
            if self.unbalanced {
                self.tainted = true;
            }
            // A change inside a nested group also happened inside every group
            // enclosing it.
            for d in &mut self.dirty {
                *d = true;
            }
        }
        let mut closed_dirty = false;
        for _ in 0..view.map_or(0, |v| v.closes) {
            // A close with nothing open only happens on an unbalanced line,
            // where any cwd change already set `tainted`.
            let popped = self.dirty.pop().unwrap_or(false);
            if popped || self.dirty.iter().any(|d| *d) {
                closed_dirty = true;
            }
        }
        let cwd = if closed_dirty || self.tainted {
            CwdState::Unknown
        } else {
            cwd_after
        };
        (cwd, closed_dirty)
    }
}

/// True when resolving the command word gained this segment a relative path
/// operand that [`advance_cwd_and_rewrite`] would have rewritten had it known
/// the head — i.e. the substitution really did come too late for the cwd pass.
///
/// Narrow ON PURPOSE. [`verb_targets`] has an opinion for exactly four heads
/// (`rm`, `unlink`, `rmdir`, `chmod`), so for every other resolved program the
/// cwd pass would have rewritten nothing and there is nothing to have lost.
/// That is what keeps the corpus's dominant shape
/// (`BIN=/path/to/condukt` … `cd "$SB" && "$BIN" guard main-tree --json`)
/// allowed: `condukt` is not one of the four, its operands are not path
/// targets, and `the_corpus_shape_a_literal_binary_path_invoked_behind_an_and_
/// is_allowed` pins that.
pub(super) fn cwd_rewrite_was_lost(before: &[&str], after: &[&str]) -> bool {
    let targets = |toks: &[&str]| -> Vec<String> {
        match toks.split_first() {
            Some((head_tok, rest)) => {
                let head = normalized_command(strip_edge_parens(head_tok));
                verb_targets(&head, rest)
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            }
            None => Vec::new(),
        }
    };
    let before = targets(before);
    targets(after).into_iter().any(|t| {
        if before.contains(&t) {
            return false;
        }
        let clean = strip_edge_parens(&t);
        !clean.is_empty() && is_relative_operand(clean) && !has_unresolvable_expansion(clean)
    })
}

/// True when a segment STRICTLY BEFORE `seg_idx` moves the shell's working
/// directory, so the cwd rewriting [`advance_cwd_and_rewrite`] applies is part
/// of what this line means.
///
/// `popd` counts: it also changes the directory, to one this file tracks as
/// [`CwdState::Unknown`].
pub(super) fn line_changes_cwd_before(line: &str, seg_idx: usize) -> bool {
    split_segments(line).iter().take(seg_idx).any(|s| {
        s.split_whitespace()
            .next()
            .map(|w| {
                matches!(
                    normalized_command(strip_edge_parens(w)).as_str(),
                    "cd" | "pushd" | "popd"
                )
            })
            .unwrap_or(false)
    })
}
