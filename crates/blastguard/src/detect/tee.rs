//! The legacy `tee` arm of `detect.rs`'s command-word match (moved here
//! from `detect.rs`, file-growth cap).
//!
//! CA-blastguard-009: `tee FILE` (no -a/--append) truncates/overwrites FILE
//! identically to a `>` redirect. Append-mode tee is Allow unless a target is
//! command-running configuration (user ruling 2026-11-20, below).

use super::ast::targets::tied_b1_targets;
use super::ast::{
    classify, names_command_running_config, spelling_names_command_running_config, Effect,
    PathClass, PathCtx, Value,
};
use super::rm::has_unquoted_glob;
use super::words::first_shell_word;
use super::{positional_operands, protected_path_block, system_path_block, Ctx, Decision};

/// See the module doc and the comments inline.
pub(super) fn analyze_tee(rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    // Round 3, MIRROR GAP. Rule 2b in `detect_bash` already settled
    // that for a PROTECTED target the append/truncate distinction is
    // irrelevant — one appended line adds a hook entry or an
    // `exit 0` — and denies `>>` onto one. `tee -a` is the same
    // operation spelled differently and never got the same treatment,
    // so the append arm below handed it an unconditional Allow.
    // Classifying the target FIRST is what makes the two spellings
    // agree.
    let targets = positional_operands(rest, &[]);
    // Axis order is rule 2's, and for rule 2's reasons.
    //
    // PROTECTED first: a gate/config file is surfaced however
    // recoverable it is, because a silently disabled gate voids every
    // later verdict.
    if let Some(deny) = targets
        .iter()
        .find_map(|t| protected_path_block(ctx, "tee target", t))
    {
        return deny;
    }
    // SYSTEM DIRECTORY next, and BEFORE recoverability — the probe
    // answers `NothingToDestroy` for a path that does not exist yet,
    // which is the correct answer to the question it was asked and the
    // wrong question for `/etc/sudoers.d/evil`, a file that destroys
    // nothing by being created and grants root.
    if let Some(deny) = targets
        .iter()
        .find_map(|t| system_path_block("tee target", t, ctx))
    {
        return deny;
    }
    if rest.iter().any(|t| *t == "-a" || *t == "--append") {
        // User ruling 2026-11-20 (S6-6 open question 1): an append onto
        // command-running configuration in the OWN worktree that
        // `protected_path_block` does not cover (`hooks/hooks.json`,
        // `crates/*/hooks/…`, `scripts/check-*.py`) is Ask (main) / Deny
        // (subagent), like the `.claude/settings.json` / `.githooks/`
        // appends above: one appended line is a new hook entry or gate
        // command. The predicate is the one S6-4 keeps Deny with
        // (`names_command_running_config`, unlifted); a target it cannot
        // place counts as naming it (fail-closed).
        if let Some(shown) = append_command_running_config(rest, &targets, ctx) {
            return command_running_config_append_ask("tee -a", &shown);
        }
        return Decision::Allow;
    }
    // MIRROR GAP, round 4. Rule 2 in `detect_bash` stopped asserting
    // that a truncating write destroys bytes without first asking
    // whether any bytes are there (operator ruling 2026-09-09). `tee`
    // is the same operation spelled as a command and kept the flat
    // Deny, whose reason — "truncates and overwrites its target
    // file(s)" — was a claim about files NOTHING in the crate had
    // looked at. That is the §4 divergence between the reason a human
    // reads and the finding actually made, and it was this crate's
    // most-recurring false positive (overwatch signature
    // `blastguard:tee-truncate`, 94 occurrences across 21 sessions).
    //
    // Only the strongest, least contestable half of rule 2's recovery
    // ruling is taken here: `NothingToDestroy` — an ABSENT target, for
    // which there are no prior bytes for the write to destroy, and
    // which is true in any tree. `RecoverableFromGit` is deliberately
    // NOT honoured: rule 2 pairs it with a confinement test that needs
    // the whole LINE (`line_changes_cwd_before`) to rule out
    // `cd /usr && …`, and this arm is handed tokens only. Undetermined
    // and Unrecoverable keep denying, per CLAUDE.md §3 — so this can
    // only move an absent target from Deny to Allow, never the reverse.
    //
    // A GLOB WORD IS NOT AN ABSENT TARGET (fail-open, S6-6 REDs
    // 2026-11-20). `{P}/*.txt` probed as one path names no file, so the
    // probe answered `NothingToDestroy` — but the shell hands `tee` the
    // existing files the glob matches, and `tee` truncates them. A word
    // with an unquoted glob metacharacter is placed instead by the
    // literal-dir glob ruling (2026-11-19): a final-component glob under
    // a literal directory takes that directory's class ([`glob_placement`]).
    // Scratch / OwnWorktree (not command-running config) keeps the Allow
    // the absent probe gave it; any other class is the legacy Deny of an
    // existing target; a glob the new layer cannot place is Ask (Deny
    // for a subagent) — never the Allow it had.
    if targets.is_empty() {
        return truncate_deny();
    }
    let mut unresolved: Option<String> = None;
    for t in &targets {
        if !matches!(
            crate::reversible::probe(t, ctx.raw_base.as_deref()),
            crate::reversible::Recovery::NothingToDestroy
        ) {
            return truncate_deny();
        }
        if !has_unquoted_glob(t) {
            continue;
        }
        match glob_placement(rest, t, ctx) {
            GlobPlacement::AllowCell => {}
            GlobPlacement::Legacy => return truncate_deny(),
            GlobPlacement::Unresolved => {
                unresolved.get_or_insert_with(|| (*t).to_string());
            }
        }
    }
    match unresolved {
        Some(word) => Decision::ask(format!(
            "tee target `{word}` is a glob the shell expands to the files it matches, and \
blastguard cannot place it (only a final-component glob under a literal directory is placed, \
by that directory), so it cannot tell which files tee would truncate and refuses to guess"
        )),
        None => Decision::Allow,
    }
}

fn truncate_deny() -> Decision {
    Decision::deny("tee without -a/--append truncates and overwrites its target file(s)")
}

/// How the new layer places one `tee` target word that has an unquoted
/// glob metacharacter.
enum GlobPlacement {
    /// A final-component glob under a literal directory
    /// (`judge::table_b::literal_dir_glob`, user ruling 2026-11-19) whose
    /// Table C class is Scratch / OwnWorktree, not naming command-running
    /// config: every file it can match is in an Allow cell.
    AllowCell,
    /// Placed, in any other class: judged as an existing target.
    Legacy,
    /// Not placed (a middle-component glob, an expansion, an untied
    /// command): which files it names is unknown.
    Unresolved,
}

/// See [`GlobPlacement`]. The tied `tee` target whose value is the word's
/// own spelling is the one `literal_dir_glob` placed (it returns the raw
/// word); when there is none the word was not placed.
fn glob_placement(rest: &[&str], word: &str, ctx: &Ctx<'_>) -> GlobPlacement {
    let Some(tied) = tied_b1_targets("tee", rest) else {
        return GlobPlacement::Unresolved;
    };
    let pctx = path_ctx(ctx);
    let mut placed = false;
    for t in tied
        .iter()
        .filter(|t| matches!(&t.target, Value::Literal(v) if v == word))
    {
        placed = true;
        let cell = matches!(
            classify(t.effect, &t.target, &t.cwd, &pctx),
            PathClass::Scratch | PathClass::OwnWorktree
        ) && !names_command_running_config(&t.target, &t.cwd, &pctx);
        if !cell {
            return GlobPlacement::Legacy;
        }
    }
    if placed {
        GlobPlacement::AllowCell
    } else {
        GlobPlacement::Unresolved
    }
}

/// The first `tee -a` target that names command-running configuration
/// ([`names_command_running_config`]), as shown in the reason. The targets
/// are the AST's tied `tee` Append targets (quote removal, same-line
/// assignments resolved); when the command is not tied, each legacy operand
/// is read with one level of quoting removed ([`first_shell_word`]), and a
/// word with an expansion is `Undetermined` — which the predicate answers
/// as naming it (fail-closed).
fn append_command_running_config(
    rest: &[&str],
    legacy_targets: &[&str],
    ctx: &Ctx<'_>,
) -> Option<String> {
    if let Some(tied) = tied_b1_targets("tee", rest) {
        return tied
            .iter()
            .filter(|t| t.effect == Effect::Append)
            .find(|t| appends_onto_config(&t.target, &t.cwd, ctx))
            .map(|t| shown(&t.target));
    }
    let cwd = legacy_cwd(ctx);
    legacy_targets.iter().find_map(|raw| {
        let value = match first_shell_word(raw) {
            Some(w) if !raw.contains(['$', '`']) => Value::Literal(w),
            _ => Value::Undetermined(format!("`{raw}` is not determined by its text")),
        };
        appends_onto_config(&value, &cwd, ctx).then(|| (*raw).to_string())
    })
}

/// `true` when the placed append-redirect target `path` names
/// command-running configuration ([`names_command_running_config`]); for
/// rule 2b in `detect_bash` (`>>` / `&>>`, user ruling 2026-11-20).
pub(super) fn append_redirect_names_command_running_config(path: &str, ctx: &Ctx<'_>) -> bool {
    appends_onto_config(&Value::Literal(path.to_string()), &legacy_cwd(ctx), ctx)
}

/// [`names_command_running_config`] (fail-closed: unplaceable is `true`),
/// except for a relative `Literal` with no `..` when the session has no
/// known cwd at all: that spelling is judged lexically
/// ([`spelling_names_command_running_config`]), as
/// `protected::protected_path_lexical_block` judges it — the legacy layer
/// places such a target nowhere and keeps its own verdict for it.
fn appends_onto_config(target: &Value, cwd: &Value, ctx: &Ctx<'_>) -> bool {
    let pctx = path_ctx(ctx);
    match target {
        Value::Literal(t)
            if !t.starts_with('/')
                && ctx.raw_base.is_none()
                && ctx.scope.session_cwd().is_none()
                && !t.split('/').any(|c| c == "..") =>
        {
            spelling_names_command_running_config(t, &pctx)
        }
        _ => names_command_running_config(target, cwd, &pctx),
    }
}

/// The legacy layer's base for a relative target (`Ctx::raw_base`).
fn legacy_cwd(ctx: &Ctx<'_>) -> Value {
    ctx.raw_base.as_deref().map_or_else(
        || Value::Undetermined("no session cwd".into()),
        |b| Value::Literal(b.to_string()),
    )
}

/// The Ask for an append onto command-running configuration (`what`: the
/// spelling, `tee -a` or `>>`).
pub(super) fn command_running_config_append_ask(what: &str, target: &str) -> Decision {
    Decision::ask(format!(
        "{what} appends to {target}, which names command-running configuration (a hook wiring \
file, a plugin hook, a gate script) or cannot be placed and so may name it — one appended line \
adds a hook entry or a gate command, so blastguard cannot tell from the tool call whether this \
strengthens or disarms it, and refuses to guess (user ruling 2026-11-20)"
    ))
}

fn shown(v: &Value) -> String {
    match v {
        Value::Literal(s) => s.clone(),
        Value::Undetermined(why) => format!("an undetermined target: {why}"),
    }
}

fn path_ctx<'a>(ctx: &Ctx<'a>) -> PathCtx<'a> {
    PathCtx {
        scope: ctx.scope,
        hook_cwd: ctx.scope.session_cwd(),
        legacy: None,
    }
}
