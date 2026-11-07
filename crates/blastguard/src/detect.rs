//! Destructive-operation detection: `(tool_name, tool_input) -> Decision`, no
//! globals beyond the static allowlist in [`crate::exclude`]. The rules are
//! lexical, with filesystem reads only where the verdict depends on what a
//! path really names: the recoverability probe ([`crate::reversible`]), the
//! symlink-resolved landing ([`physical_landing`]) of a write to a system
//! directory and of a write to a protected or workflow path.
//!
//! The bias is deliberately asymmetric: we only Deny on *clearly* destructive,
//! hard-to-undo patterns (recursive/wildcard deletion, full-file truncation,
//! disk-level writes, working-tree discards) — and, since the egress/
//! remote-exec closure landed, the exfil/remote-exec exit of the
//! prompt-injection "lethal trifecta". This is NOT scoped to a literal `|`
//! pipe: a fetch (`curl`/`wget`/`fetch`/httpie) or a decode stage (`base64
//! -d`, or its `openssl base64 -d`/`openssl enc -base64 -d`/`-a -d` mirror)
//! reaching a shell/interpreter is denied whether the two are joined by a
//! `|` pipeline (`curl … | bash`, `curl … | base64 -d | sh`, [`analyze_pipe_egress`]),
//! bridged through `xargs`'s trailing command (`curl … | xargs -0 bash -c`,
//! [`stage_is_interpreter_terminal`]), or reached via a process-substitution
//! ARGUMENT with no top-level pipe on the line at all (`bash <(curl …)`,
//! `source <(curl …)`, [`analyze_process_substitution_egress`]), or via a
//! here-string (`bash <<<"$(curl …)"`, [`analyze_here_string_egress`]) — the
//! `<<<` mirror of the same sink, since a shell/interpreter reads a
//! here-string operand as its own script exactly like it reads a process
//! substitution's stdout. The process-substitution scan is itself RECURSIVE,
//! not a single direct-word check: it descends through every `|` stage
//! inside the substitution's body AND through any `<(...)` nested inside
//! that body ([`scan_body_for_fetch_or_decode`]), so `bash <(curl … |
//! base64 -d)` and `bash <(bash <(curl …))` are denied exactly like
//! `bash <(curl …)` — a trailing pipe stage or a nested substitution is not
//! a way to hide the fetch. A wrapper prefix
//! (`command curl …`, `env curl …`) cannot hide the fetch from any of these —
//! every stage's effective command word is resolved through the same
//! wrapper-aware [`command_candidates`] the destructive-operation rules
//! already use, so the un-wrapped and wrapped spellings of a fetch or an
//! interpreter are recognised identically ([`stage_is_fetch_or_decode`]).
//! Separately, `curl`/`wget` uploading a file reference to a URL
//! ([`analyze_fetch`]), `nc`/`netcat` fed a file on stdin ([`analyze_nc`]),
//! and bash's `/dev/tcp/`/`/dev/udp/` raw-socket pseudo-devices
//! ([`contains_dev_tcp_or_udp`]) close the exfil side. Plain source-control
//! upload (`git push`) is deliberately OUT of this scope — that outbound-comms
//! tier is separate.
//!
//! There are three answers, not two. A command blastguard positively knows to
//! be destructive is DENIED; one it positively knows to be ordinary is ALLOWED;
//! and a construct it genuinely CANNOT ANALYSE is ASKED
//! ([`crate::model::Decision::Ask`]) rather than waved through. "Unknown" here
//! means specifically *unanalysable* — an unresolvable expansion sitting in a
//! position that gets executed, an unrecognised wrapper in front of a
//! destructive command line, or a command too complex to finish analysing.
//! It does NOT mean "not on an allowlist": there is no allowlist, and an
//! ordinary command this module has no rule for still falls through to Allow.
//!
//! An `Ask` is only emitted by the hook when a human is actually there to
//! answer it (see [`crate::interactive`]); otherwise it is hardened to a Deny.

use serde_json::Value;

use crate::exclude;
use crate::model::Decision;
use crate::scope::{Placement, SafeRoots};
use harness_core::verdict::Determination;

mod compound;
mod cwd;
mod denoise;
mod executors;
mod fileops;
mod find_xargs;
mod gh;
mod git;
mod git_stage;
mod git_unmerged;
mod harm_mark;
mod heredoc;
mod high_blast;
mod interpreter;
mod payload;
mod protected;
mod readings;
mod readonly;
mod redirect;
mod rm;
mod segment;
mod sinks;
mod substitution;
mod system_paths;
mod vars;
mod verdict_acc;
mod words;

use verdict_acc::VerdictAcc;

use denoise::denoised_eval_rescan;
use gh::analyze_gh;
use readings::detect_bash_every_reading;

use high_blast::{analyze_high_blast_tier, high_blast_outside_tree_rm};

use sinks::{
    analyze_fetch, analyze_here_string_egress, analyze_nc, analyze_pipe_egress,
    analyze_process_substitution_egress, contains_dev_tcp_or_udp, here_string_operands,
    here_string_spans, split_statements_into_pipe_stages, upload_flag_operand,
};

use git::{analyze_git, git_subcommand_index, git_worktree_eligible};

use find_xargs::{analyze_find, analyze_xargs, xargs_command_start, xargs_reads_arg_file};

use rm::{
    analyze_rm, checkout_exempt_eligible, deletion_rm_eligible, literal_cd_and_rest,
    worktree_rm_eligible, DeletionEligibility,
};

use fileops::{
    analyze_copy_move, analyze_sed, analyze_sort, analyze_uniq, analyze_unlink_rmdir,
    chmod_mode_and_targets, chown_owner_and_targets, dotgit_link_deny, has_glob_meta,
    home_asset_deny, mode_removes_exec, mode_removes_read, positional_operands,
};

use readonly::{is_recognized_command, unknown_verb_protected_ask};

use protected::{
    glob_literal_prefix, protected_disarm_deny, protected_glob_deny, protected_landing_block,
    protected_path_block, protected_path_block_at, protected_path_lexical_block,
    protected_reown_ask, protected_tree_deny, real_system_path_block, system_path_block,
};

use interpreter::{
    analyze_code_interpreter, interpreter_code_verdict, irreversible_device_verb,
    irreversible_git_subcommand, is_code_interpreter, quoted_string_literals, strip_version_suffix,
};

use payload::{
    analyze_interpreter_stdin_exec, dash_c_payloads, inline_command_payloads,
    interpreter_payloads_after, interpreter_stdin_operand, payloads_after,
    split_substitution_opening_assignments, strip_wrapping_quotes,
};

use compound::{
    analyze_expanded_data, compound_views, loop_variants, LoopBinding, LoopVariants, SegmentView,
    MAX_LOOP_VARIANTS,
};
#[cfg(test)]
use cwd::verb_targets;

use cwd::{
    advance_cwd_and_rewrite, cwd_rewrite_was_lost, is_relative_operand, line_changes_cwd_before,
    segment_cwd_states, verb_operands_are_rewritten, CwdState, CwdWalks,
};

use substitution::{
    backtick_body_as_run, command_substitution_payloads, executed_substitution_payloads,
    process_substitution_payloads, scan_balanced, scan_balanced_strict, Stop,
};

use heredoc::{
    first_line_quoted_here_document_body, here_document_delimiter_is_quoted, here_document_layout,
    here_document_openers, interpreter_program_body_segments, opens_here_document,
    python_program_body_segments, quoted_here_document_body_walks,
    redirect_view_without_quoted_here_document_body, segment_closes_here_document,
    strip_data_here_document_bodies,
};
#[cfg(test)]
use heredoc::{opener_consumer, permitted_prefix_line};

use segment::{
    assignment_execution_is_unconditional, assignments_reach_parent_shell, closed_substitution_end,
    closed_substitution_end_strict, here_doc_body_spans, split_segments,
    split_segments_paren_aware, split_segments_with_separators, SegmentSep, SeparatedSegment,
};

use words::{
    basename, command_candidates, first_shell_word, foreign_shell_runs_code, has_operand,
    has_short, is_assignment, is_bare_redirect_op, is_exec_wrapper, is_foreign_shell,
    is_redirect_token, is_shell, is_short_flag, normalized_command, takes_shell_command_flag,
    unclosed_quote_char, wrapped_command_heads, xargs_shell,
};

use vars::{
    has_unresolvable_expansion, is_shell_identifier, opens_unmodelled_construct, quote_aware_words,
    referenced_variable_name, resolve_expanded_command_word, unquote_literal_value,
    unresolvable_command_word, CommandWordOrigin,
};

use redirect::{
    append_redirect_targets, append_target_occurrences, physical_redirect_spelling,
    place_redirect_target, redirect_target_is_safe, redirect_target_occurrences, redirect_targets,
    resolve_redirect_target_at, temp_disposable_redirect_target, unplaceable_redirect_ask,
};

/// Top-level entry WITH a location model: same dispatch as [`detect`], but the
/// destructive-shape rules may resolve their targets and, for targets provably
/// confined to a safe root, return an `Ask` instead of a `Deny`.
///
/// The hook binary calls this one (it knows the session's cwd and can
/// canonicalise paths); [`detect`] is the no-model form every other consumer
/// keeps using. See [`crate::scope`] for why the relaxation cannot leak into
/// unattended execution.
pub fn detect_scoped(tool_name: &str, tool_input: Option<&Value>, scope: &SafeRoots) -> Decision {
    match tool_name {
        "Bash" => {
            let cmd = tool_input
                .and_then(|v| v.get("command"))
                .and_then(|c| c.as_str());
            match cmd {
                Some(c) => detect_bash_every_reading(c, scope),
                // A Bash call IS in jurisdiction, and the command could not be
                // read out of it (absent key, non-string value, schema drift).
                // That is "failed to determine", not "nothing to judge" — see
                // [`UNREADABLE_OPERAND`].
                None => unreadable_operand("Bash", "tool_input.command"),
            }
        }
        "Write" => detect_write(tool_input, &Ctx::new(scope, "")),
        // Edit / MultiEdit / NotebookEdit are partial edits, not full-file
        // destruction, so they are allowed for ordinary files. They are NOT
        // unconditionally allowed: a one-line edit is all it takes to disarm a
        // gate (`.claude/settings.json`, `.githooks/pre-commit`, a shell rc),
        // and "partial" says nothing about blast radius. Classifying the TARGET
        // is what distinguishes the two.
        "Edit" | "MultiEdit" | "NotebookEdit" => detect_edit(tool_input, &Ctx::new(scope, "")),
        // An UNMATCHED TOOL is a different situation and stays a silent Allow:
        // blastguard claims no jurisdiction over Read/Grep/WebFetch/Task, so
        // there is genuinely nothing here to judge. Do not fold this arm into
        // the unreadable-operand case above — turning it into an Ask would
        // prompt on every read in the session, which is a broken gate, not a
        // stricter one.
        _ => Decision::Allow,
    }
}

/// Top-level entry: dispatch on the tool name, with NO location model.
///
/// Every destructive-shape rule answers exactly as it did before
/// [`crate::scope`] existed, because [`SafeRoots::none`] makes every placement
/// question `Undetermined`. This is the entry point for every library consumer
/// that hands commands to `sh -c` with no human present (condukt's check
/// runner, specguard's forge, daily's task runner): they get the strict gate,
/// unchanged.
pub fn detect(tool_name: &str, tool_input: Option<&Value>) -> Decision {
    detect_scoped(tool_name, tool_input, &SafeRoots::none())
}

/// The location model, plus the working directory the CURRENT segment resolves
/// its relative operands against.
///
/// # Why there are two bases and not one
///
/// [`rewrite_relative_operands`] already rewrites the relative operands of the
/// verbs [`verb_targets`] knows (`rm`, `unlink`/`rmdir`, `chmod`) into
/// `<tracked dir>/<operand>` before any rule sees them — so for THOSE verbs the
/// operand is already expressed relative to the session's own cwd, and using
/// the tracked directory as the base again would double-prefix it. Every other
/// verb (`find`, `truncate`, `shred`, `git clean`) is left untouched by that
/// rewrite, so its relative operand really is relative to the tracked
/// directory.
///
/// Getting this wrong is not cosmetic in either direction, which is why it is
/// modelled explicitly rather than approximated:
///
///   * one base for everything (the session cwd) would judge
///     `cd /usr && truncate -s 0 lib/x` against the project tree and call it
///     confined — a weakening;
///   * one base for everything (the tracked dir) would judge the REWRITTEN
///     `cd crates && rm -rf ../../etc` (rewritten to `crates/../../etc`)
///     against `<project>/crates` and land on `<project>/etc` — also a
///     weakening, and a subtler one.
#[derive(Clone)]
struct Ctx<'a> {
    scope: &'a SafeRoots,
    /// Base for operands `rewrite_relative_operands` has already re-expressed
    /// (the [`verb_targets`] verbs). `None` = not known, so relative operands
    /// of those verbs cannot be placed.
    rewritten_base: Option<&'a str>,
    /// Base for operands that rewrite left alone. `None` = not known.
    raw_base: Option<String>,
    /// Whether the TOP-LEVEL command may use the worktree-storage `Allow` at
    /// all — see [`worktree_rm_eligible`]. Computed once from the whole command
    /// line in [`Ctx::new`] and carried unchanged into every derived context.
    worktree_rm_eligible: bool,
    /// Whether the TOP-LEVEL command may use the deletion-principle verdicts
    /// at all — literal ([`deletion_rm_eligible`]) and `/tmp` wildcard — see
    /// [`DeletionEligibility`]. Computed once in [`Ctx::new`], carried unchanged.
    deletion_rm_eligible: DeletionEligibility,
    /// Whether the TOP-LEVEL command may lift the protected-path rule for a
    /// path inside a linked worktree checkout at all — see
    /// [`Ctx::in_worktree_checkout`] and [`checkout_exempt_eligible`].
    /// Computed once in [`Ctx::new`], carried unchanged into derived contexts.
    checkout_exempt_eligible: bool,
    /// `Some(words after git)` ONLY when the TOP-LEVEL command line (or the
    /// `REST` of a [`literal_cd_and_rest`] line) is ONE plain `git` command
    /// ([`git_worktree_eligible`]): the precondition of the git
    /// working-tree-discard `Allow` inside a linked worktree checkout (user
    /// ruling 2026-11-07). The git arm compares it with the words it is
    /// judging, so a NESTED git invocation (an alias body, `bisect run git …`)
    /// never inherits it. Computed once in [`Ctx::new`], carried unchanged.
    git_eligible_rest: Option<String>,
    /// `Some` ONLY while [`interpreter_code_verdict`] runs its shell pass over
    /// interpreter PROGRAM TEXT (user ruling R2, 2026-10-14). A truncating
    /// redirect whose target cannot be determined (it carries a shell
    /// expansion, or is relative with no known base — see
    /// [`redirect_target_undeterminable`]) and that would otherwise be denied
    /// is recorded here instead of denied; the caller turns the record into an
    /// Ask carrying [`RECOMMEND_NO`] unless something else in the program is a
    /// Deny. In program text such a `>` is far more often an operator inside a
    /// string or expression (`print(1 > 0)`, `'x > \`y\`'`) than a redirect,
    /// but it cannot be shown NOT to be one, so it is never cleared to Allow.
    /// `None` everywhere else: an ordinary command line keeps today's Deny.
    program_text_redirects: Option<&'a std::cell::RefCell<Vec<String>>>,
}

/// Marker every Ask produced under the user's 2026-10-14 rulings R1/R2 carries
/// in its reason (ruling R0): the Ask states its recommended answer is the
/// negative one.
const RECOMMEND_NO: &str = "Recommended: do not run this.";

/// `d` with [`RECOMMEND_NO`] appended when it is an Ask (and not already
/// marked); `Allow`/`Deny` are returned unchanged.
fn recommend_against(d: Decision) -> Decision {
    match d {
        Decision::Ask(reason) if !reason.contains(RECOMMEND_NO) => {
            Decision::Ask(format!("{reason} {RECOMMEND_NO}"))
        }
        other => other,
    }
}

/// Whether a redirect TARGET names no determinable file: it carries a shell
/// expansion, or it is relative and no base is known to place it against.
/// These are exactly the two pre-IO `Undetermined` rows of
/// [`crate::reversible::probe`] — not the git-failure rows, whose target IS
/// known and only its recoverability is not.
fn redirect_target_undeterminable(target: &str, base: Option<&str>) -> bool {
    target.contains('$')
        || target.contains('`')
        || crate::reversible::resolve(target, base).is_none()
}

impl<'a> Ctx<'a> {
    /// The context for a segment at the start of a command line: no `cd` seen
    /// yet, so both bases are the session's own working directory.
    ///
    /// Each eligibility predicate is read on the whole `command` and, when the
    /// line is `cd D && REST` with `D` a literal, existing, absolute directory
    /// tied to `REST` by `&&` alone ([`literal_cd_and_rest`]), ALSO on `REST`;
    /// either acceptance makes the line eligible. The second reading only
    /// removes the tying `cd` from the text the predicate sees — `REST`'s
    /// relative operands are still judged at `D/<operand>`.
    fn new(scope: &'a SafeRoots, command: &str) -> Ctx<'a> {
        let tied_rest = literal_cd_and_rest(scope, command);
        let eligible = |pred: fn(&str) -> bool| pred(command) || tied_rest.is_some_and(pred);
        Ctx {
            scope,
            rewritten_base: scope.session_cwd(),
            raw_base: scope.session_cwd().map(str::to_string),
            worktree_rm_eligible: eligible(worktree_rm_eligible),
            deletion_rm_eligible: DeletionEligibility::of(eligible(deletion_rm_eligible), eligible),
            checkout_exempt_eligible: eligible(checkout_exempt_eligible),
            git_eligible_rest: git_worktree_eligible(command)
                .or_else(|| tied_rest.and_then(git_worktree_eligible)),
            program_text_redirects: None,
        }
    }

    /// The context after a `cd`/`pushd` this analysis RESOLVED to `dir`.
    fn after_cd(&self, dir: &str) -> Ctx<'a> {
        let raw_base = if dir.starts_with('/') {
            Some(dir.to_string())
        } else {
            self.rewritten_base
                .map(|base| format!("{}/{}", base.trim_end_matches('/'), dir))
        };
        Ctx {
            scope: self.scope,
            rewritten_base: self.rewritten_base,
            raw_base,
            worktree_rm_eligible: self.worktree_rm_eligible,
            deletion_rm_eligible: self.deletion_rm_eligible,
            checkout_exempt_eligible: self.checkout_exempt_eligible,
            git_eligible_rest: self.git_eligible_rest.clone(),
            program_text_redirects: self.program_text_redirects,
        }
    }

    /// The context after a directory change this analysis could NOT resolve
    /// (`cd $VAR`, `cd -`, `popd`): no relative operand can be placed any more.
    /// Absolute operands are unaffected — their placement never depended on a
    /// base.
    fn after_unknown_cd(&self) -> Ctx<'a> {
        Ctx {
            scope: self.scope,
            rewritten_base: None,
            raw_base: None,
            worktree_rm_eligible: self.worktree_rm_eligible,
            deletion_rm_eligible: self.deletion_rm_eligible,
            checkout_exempt_eligible: self.checkout_exempt_eligible,
            git_eligible_rest: self.git_eligible_rest.clone(),
            program_text_redirects: self.program_text_redirects,
        }
    }

    /// The base to resolve a relative operand of `verb` against.
    fn base_for(&self, verb: &str) -> Option<&str> {
        if verb_operands_are_rewritten(verb) {
            self.rewritten_base
        } else {
            self.raw_base.as_deref()
        }
    }

    /// The safe root that contains EVERY one of `operands` as a strict
    /// descendant, or `None` if any operand is elsewhere, is a root itself, or
    /// could not be resolved.
    ///
    /// `None` is the fail-closed answer for all three of those, and the caller
    /// must keep the Deny it already had. An empty operand list is `None` too:
    /// "nothing to place" is not "placed inside".
    fn confined_root<S: AsRef<str>>(&self, verb: &str, operands: &[S]) -> Option<String> {
        self.confined(verb, operands, false)
    }

    /// Like [`Ctx::confined_root`], but a safe root ITSELF is acceptable.
    ///
    /// Only for verbs whose operand is not the thing destroyed: `git clean`
    /// never removes tracked files or `.git`, and a `find` search root is
    /// walked rather than deleted (and `find` adds its own condition on top —
    /// see `find_is_narrowed`). `rm` must never use this: `rm -rf <project>`
    /// takes `.git` with it, which is precisely what [`Placement::IsRoot`]
    /// exists to keep distinguishable.
    fn confined_root_or_root_itself<S: AsRef<str>>(
        &self,
        verb: &str,
        operands: &[S],
    ) -> Option<String> {
        self.confined(verb, operands, true)
    }

    fn confined<S: AsRef<str>>(
        &self,
        verb: &str,
        operands: &[S],
        root_itself_ok: bool,
    ) -> Option<String> {
        if operands.is_empty() {
            return None;
        }
        let base = self.base_for(verb);
        let mut found: Option<String> = None;
        for operand in operands {
            let operand = operand.as_ref();
            // The single choke point for "location cannot buy anything here".
            // Every protected path IS inside this project tree, so a placement
            // check alone would hand `truncate -s 0 .claude/settings.json` an
            // Ask on the grounds that the file is conveniently nearby. The
            // verb-specific rules above still emit their own, more precise
            // Deny; this line is what stops the relaxation from reaching them
            // at all, for every verb, including ones added later.
            if exclude::touches_protected(operand) {
                return None;
            }
            let placement = match self.scope.classify(operand, base) {
                Determination::Known(p) => p,
                Determination::Undetermined(_) => return None,
            };
            let root = match placement {
                Placement::Inside { root, .. } => root,
                Placement::IsRoot { root } if root_itself_ok => root,
                Placement::IsRoot { .. } | Placement::Outside { .. } => return None,
            };
            // Different safe roots on one command line is fine (a `/tmp`
            // target next to a project one); the reason string names the first.
            found.get_or_insert(root);
        }
        found
    }
}

/// True for the verbs whose relative operands [`rewrite_relative_operands`]
/// has already re-expressed against the session cwd.
///
/// Kept as its own list rather than derived from [`verb_targets`] at runtime
/// (that function needs a token slice to answer), and pinned to it by
/// `verb_rewrite_list_matches_verb_targets` in the test module below — if a verb
/// is added to `verb_targets` and not here, that test fails rather than this
/// module silently resolving its operands against the wrong base.
/// The verdict for a rule whose only objection is the SHAPE of the command:
/// an Ask when every target resolved strictly inside a safe root, otherwise the
/// Deny that rule always returned.
///
/// `verb` selects the base for relative operands (see [`Ctx::base_for`]);
/// `action` is what the Ask calls the operation.
fn shape_deny_or_confined_ask<S: AsRef<str>>(
    ctx: &Ctx<'_>,
    verb: &str,
    action: &str,
    targets: &[S],
    deny_reason: &str,
) -> Decision {
    match ctx.confined_root(verb, targets) {
        Some(root) => confined_ask(action, &root, targets),
        None => Decision::deny(deny_reason),
    }
}

impl Decision {
    /// Second chance for a rule whose operand is NOT the thing destroyed
    /// (`chmod -R`, `chown -R`): if the shape-only Deny stands, retry the
    /// placement accepting a safe root itself.
    ///
    /// Written as a post-hoc widening rather than a flag on
    /// [`shape_deny_or_confined_ask`] so the narrow rule stays the default: a
    /// verb that must NOT accept its own root (every deleting verb) cannot pick
    /// this up by forgetting to pass `false`.
    fn or_root_itself<S: AsRef<str>>(
        self,
        ctx: &Ctx<'_>,
        verb: &str,
        action: &str,
        targets: &[S],
    ) -> Decision {
        match self {
            Decision::Deny(reason) => match ctx.confined_root_or_root_itself(verb, targets) {
                Some(root) => confined_ask(action, &root, targets),
                None => Decision::Deny(reason),
            },
            other => other,
        }
    }
}

/// The `Ask` for a destructive command whose every target resolved inside a
/// safe root.
///
/// The wording is load-bearing in two ways: it names the ROOT (so the operator
/// can see what "confined" was measured against, rather than trusting the
/// word), and it carries the fixed phrase `is confined to`, which
/// [`crate::rule_id`] keys the `scoped-destructive-confined` id on.
fn confined_ask<S: AsRef<str>>(action: &str, root: &str, targets: &[S]) -> Decision {
    Decision::ask(format!(
        "{action} is confined to {root} — a safe root for this session (its project tree, its \
worktree, or a temp dir), and every target ({}) resolved to a real path strictly inside it, \
symlinks included. The blast radius is bounded, so this is a question rather than a refusal: \
approve it if that is the tree you meant.",
        targets
            .iter()
            .map(|t| t.as_ref())
            .collect::<Vec<&str>>()
            .join(" ")
    ))
}

/// The shared wording for "this call is mine to judge and I could not read its
/// operand", so [`crate::rule_id`] gives the whole class ONE stable signature.
///
/// Deliberately ONE id across Bash/Write/Edit: they are the same recurring
/// failure — blastguard's idea of the payload schema no longer matches what it
/// is being sent — and splitting by tool would fragment the very signature that
/// makes the drift visible when it recurs.
pub(crate) const UNREADABLE_OPERAND: &str = "blastguard could not read";

/// The phrase every `unknown_wrapper_ask` reason carries, so the recursion can
/// recognise its OWN ask coming back from the tail.
///
/// # Why this is a permission list, and what that costs
///
/// The safer-looking polarity is the opposite one: forward every nested Ask
/// except a named few. It was tried first, and it over-blocks. Two measured
/// false positives killed it:
///
///   * `echo $(date 2>&1)` became an Ask. In tail position the analyser reads
///     `$(date` as a command word and raises the unresolvable-expansion Ask —
///     a real doubt when an expansion is in COMMAND position, a misread when it
///     is in ARGUMENT position, and this frame cannot tell which.
///   * `echo hi` wrapped in 7 quote-only `sh -c` layers became an Ask, via the
///     depth-exhaustion Ask. `tests/backslash_escape_nesting_fail_open.rs`
///     pins that as ALLOW, and is right to: blocking there means gating on the
///     over-escaped SHAPE rather than on any destructive command word.
///
/// So the forward set is deliberately the ONE class that is unambiguous: an
/// unrecognised verb with a destructive line behind it. That is the sentence
/// this function itself emits, and dropping it was the whole defect.
///
/// The cost is stated rather than hidden: every OTHER nested Ask class still
/// collapses to `Allow` in this frame, including depth exhaustion. That is not
/// a regression — before this change the recursion tested `.is_deny()` and so
/// dropped all of them — but it is a KNOWN PERMISSIVE SET, not a cleared one,
/// and a new Ask class added later will be dropped here silently. Closing it
/// needs the expansion Ask to distinguish command position from argument
/// position, which is a different piece of work.
///
/// Matching on the reason string is how `rule_id` already classifies decisions
/// in this crate, so this is the existing idiom rather than a new mechanism, and
/// `two_unknown_wrappers_are_not_safer_than_one` pins it — a reword that broke
/// the match fails a test rather than failing open.
pub(crate) const UNKNOWN_VERB_ASK: &str = "is not a command blastguard recognises";

/// Verdict for a call blastguard has jurisdiction over whose operand it could
/// not extract.
///
/// # Why `Ask` and not `Allow`
///
/// This is the crate's own front door failing the test the crate exists to
/// enforce. `model.rs:5` names the defect: a two-valued form "forced every
/// construct blastguard cannot analyse into `Allow`, i.e. 'I don't understand
/// this' was recorded as 'this is fine'". The 25-odd sub-analysers in this file
/// were all converted to say `Ask` for constructs they cannot parse — but the
/// entry points, where the input itself is unreadable, were left binary. An
/// analyser that refuses to guess about a nested `$(...)` while its front door
/// waves through a payload it could not parse at all is not fail-closed; it is
/// fail-closed in the part that was looked at.
///
/// # Why `Ask` and not `Deny`
///
/// Same reason as [`protected_path_block`]: the unreadability is certain, the
/// HARM is not. A payload shape blastguard does not recognise is usually a
/// version skew between the hook and Claude Code, not an attack. `Ask` puts it
/// to the human when one is present, and `Decision::hardened` collapses it to
/// `Deny` in every headless/agent context where nobody can answer — so the
/// autonomous threat model gets the strict answer either way.
fn unreadable_operand(tool: &str, field: &str) -> Decision {
    Decision::ask(format!(
        "{UNREADABLE_OPERAND} the operand of this {tool} call ({field} is \
         missing, empty, or not a string), so it was never analysed — refusing \
         to guess. If this recurs, blastguard's payload schema has drifted from \
         what Claude Code sends and needs updating."
    ))
}

// ---------------------------------------------------------------------------
// File-write handling
// ---------------------------------------------------------------------------

fn extract_path(ti: Option<&Value>) -> Option<String> {
    let v = ti?;
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = v.get(key).and_then(|p| p.as_str()) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

fn detect_write(ti: Option<&Value>, ctx: &Ctx<'_>) -> Decision {
    let path = match extract_path(ti) {
        Some(p) => p,
        None => return unreadable_operand("Write", "file_path"),
    };
    // Git internals first: a whole-file Write over `.git/<x>` has always been a
    // flat Deny. `.git/config` (and `.git/hooks/**`) are ALSO protected paths,
    // whose verdict is the weaker Ask — checking the protected list first would
    // downgrade that Deny (backlog 03885f11). The stricter verdict wins.
    if exclude::is_git_internal(&path) {
        return Decision::deny(format!(
            "Write would overwrite git internals ({path}) — refusing"
        ));
    }
    // BEFORE the config-file allowlist, not after: `.claude/**`, `*.toml` and
    // `settings.local.json` are all matched by `is_config_file`, so the
    // exemption used to swallow exactly the paths that decide whether the gates
    // run at all. "It is a config file" is the reason to look harder here, not
    // the reason to stop looking.
    if let Some(deny) = protected_path_block(ctx, "Write", &path) {
        return deny;
    }
    // Also before the config-file allowlist: `/etc/<x>.toml` is a config file
    // by name and a system-directory drop-in by location.
    if let Some(deny) = file_tool_system_block(ctx, "Write", &path) {
        return deny;
    }
    if exclude::is_config_file(&path) {
        return Decision::Allow;
    }
    let content = ti
        .and_then(|v| v.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if content.trim().is_empty() {
        return Decision::deny(format!(
            "Write would replace {path} with empty content, wiping the file"
        ));
    }
    Decision::Allow
}

/// Edit / MultiEdit / NotebookEdit: a partial edit of an ordinary file is
/// allowed, but a partial edit of a protected gate/hook/policy path is exactly
/// how a guard gets switched off, and an edit inside a system directory is the
/// same payload as a redirect there ([`file_tool_system_block`]), so the target
/// is classified rather than assumed harmless. `NotebookEdit` addresses its
/// target via `notebook_path`, which `extract_path` already covers.
fn detect_edit(ti: Option<&Value>, ctx: &Ctx<'_>) -> Decision {
    let path = match extract_path(ti) {
        Some(p) => p,
        None => return unreadable_operand("Edit", "file_path/notebook_path"),
    };
    if let Some(deny) = protected_path_block(ctx, "Edit", &path) {
        return deny;
    }
    if let Some(deny) = file_tool_system_block(ctx, "Edit", &path) {
        return deny;
    }
    Decision::Allow
}

/// The system-directory axis for the file tools (Write / Edit / MultiEdit /
/// NotebookEdit), user ruling 2026-11-01: the same rule, reason and directory
/// set as a Bash redirect — [`system_path_block`] on the path as written, then
/// [`real_system_path_block`] on the path the kernel really writes, so a
/// dangling file symlink, a chain of them, or a directory symlink into `/etc`
/// is the same Deny as the direct spelling. The `confined_root` exemption in
/// [`system_path_block`] applies unchanged (a project checked out under `/opt`).
///
/// When the landing cannot be resolved (a symlink loop, an `lstat`/`read_link`
/// error other than "does not exist") the answer is Ask, never Allow: "could
/// not resolve" is not "not a system directory" (CLAUDE.md §3).
fn file_tool_system_block(ctx: &Ctx<'_>, action: &str, path: &str) -> Option<Decision> {
    if let Some(deny) = system_path_block(action, path, ctx) {
        return Some(deny);
    }
    match real_system_path_block(action, path, ctx) {
        Ok(verdict) => verdict,
        Err(why) => Some(Decision::ask(format!(
            "{action} target ({path}) could not be resolved through its symlinks ({why}) — \
blastguard cannot tell whether it lands in a system directory, so it refuses to guess"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Bash handling
// ---------------------------------------------------------------------------

/// How deep we recurse into shell-evaluation wrappers (`eval`, `sh -c`, …)
/// before giving up. Bounds work and guards against pathological nesting; no
/// realistic command line wraps a destructive payload this many layers deep.
const MAX_SHELL_DEPTH: usize = 8;

/// Total number of `detect_bash` invocations allowed for ONE top-level command.
///
/// D4 (verified availability defect, fixed): a depth cap alone does NOT bound
/// the work, because the re-analysis arms FAN OUT. `analyze_find` loops over
/// every `-exec` position and re-analyses the whole remaining tail, which still
/// contains the other `-exec` tokens, so the branching factor is the number of
/// `-exec` tokens and the tree is O(N^MAX_SHELL_DEPTH). Measured on a single
/// invocation of `find . ` + `-exec find . `×N + `-exec echo {} ` + `+ `×(N+1):
/// N=20 → 2.2s, N=24 → 8.2s, N=28 → 30s, N=32 (a 503-byte command) → >60s,
/// killed. blastguard is a PreToolUse hook, so that hangs the user's turn.
///
/// This caps total NODE VISITS across the whole recursion tree, not just its
/// depth. Ordinary commands use fewer than a dozen visits; the cap is three
/// orders of magnitude above that, so it cannot be reached by real work.
const MAX_ANALYSIS_NODES: u32 = 2_000;

thread_local! {
    /// Remaining node budget for the top-level command currently being analysed.
    static ANALYSIS_BUDGET: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// Test-only work probe: when installed, counts EVERY `detect_bash` entry
    /// (admitted or budget-rejected) on this thread. It is an atomic shared via
    /// `Arc` so a watcher thread can observe the count while the analysis is
    /// still running, and fail on an unbounded tree instead of hanging. It is
    /// deliberately independent of `ANALYSIS_BUDGET`: removing the budget must
    /// make this count explode, not leave it untouched.
    static VISIT_PROBE: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicU64>>> =
        const { std::cell::RefCell::new(None) };
}

/// Consume one unit of the analysis budget. Returns false when it is exhausted.
fn take_budget() -> bool {
    ANALYSIS_BUDGET.with(|b| {
        let left = b.get();
        if left == 0 {
            false
        } else {
            b.set(left - 1);
            true
        }
    })
}

/// Re-analyse a payload string that a shell-evaluation position will EXECUTE
/// (`sh -c <payload>`, `eval <payload>`, `flock -c <payload>`).
///
/// Two outcomes beyond the ordinary rules:
///   * the payload analyses as destructive → propagate that `Deny` unchanged;
///   * the payload's command word is an unresolvable expansion → `Ask`. The
///     text that will actually be executed is not in the command line at all,
///     so "no rule matched" here means "nothing was examined", not "safe".
///
/// Deny is checked FIRST so a payload that is both destructive and partly
/// unresolvable (`sh -c "rm -rf $DIR"` — command word `rm`, resolvable) keeps
/// its Deny.
/// Tail of the "command word is an expansion" Ask. The interpreter embedded-string
/// pass drops exactly this Ask (a literal such as `"$HOME"` is data, not a
/// command); see `interpreter::is_expansion_head_ask`.
pub(super) const EXPANSION_HEAD_TAIL: &str = " is an expansion whose value only exists at run time — blastguard cannot tell what program this runs";

fn analyze_shell_payload(payload: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let d = detect_bash(payload, depth + 1, ctx);
    if d.is_blocking() {
        return d;
    }
    if let Some(word) = unresolvable_command_word(payload) {
        return Decision::ask(format!(
            "the command executed here comes from `{word}`, whose value only exists at run time — blastguard cannot see what would actually run"
        ));
    }
    Decision::Allow
}

/// The recursion cap was reached, so analysis DID NOT FINISH. That is the same
/// condition as budget exhaustion a few lines up (`detect_bash`'s D4 guard) —
/// "we did not finish looking, so we have no verdict" — and it gets the same
/// answer: an Ask, which `Decision::hardened` turns into the pre-existing Deny
/// whenever no human is available to answer.
///
/// Every one of the five depth-capped sites used to resolve to `Allow`, either
/// by returning it outright or by skipping the recursion and falling through to
/// `acc.finish()`. A destructive payload nested past the cap was therefore
/// waved through *unanalysed* — the exact fail-open this module exists to
/// prevent, sitting one line away from the sibling condition that handles it
/// correctly.
///
/// The reason deliberately differs from the budget ask so the two unfinished-
/// analysis causes stay distinguishable in the logs.
fn depth_exhausted() -> Decision {
    Decision::ask(
        "command nests shell/wrapper invocations deeper than blastguard analyses \
         (recursion depth limit) — analysis did not finish, so blastguard cannot \
         vouch for it either way",
    )
}

fn detect_bash(cmd: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    #[cfg(test)]
    VISIT_PROBE.with(|p| {
        if let Some(c) = p.borrow().as_ref() {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    });
    // D4: when the budget is exhausted we never Allow. A command too complex to
    // analyse within a bounded amount of work must not be waved through
    // unanalysed — an unanalysed destructive command is exactly the fail-open
    // this module exists to prevent.
    //
    // Budget exhaustion is the definition of "unknown": we did not finish
    // looking, so we have no verdict about the command. That is an ASK, which
    // hardens to the pre-existing DENY whenever no human is available to answer
    // (see `crate::interactive` and `Decision::hardened`) — so the fallback is
    // exactly as safe as the deny this replaced, never weaker.
    if !take_budget() {
        return Decision::ask(
            "command is too complex to analyse within the safety budget — blastguard cannot vouch for it either way",
        );
    }

    // 0. When the command is effectively ONE here-document fed to a data-only
    // reader (`cat > notes.txt <<'EOF'`, `git commit -F - <<'EOF'`), with no
    // other command before or after it, its body is removed before EVERY rule
    // below — the line-level scans as much as the per-segment loop — so its
    // text is not judged as a command (backlog fa1fce21 / 7037df97). In every
    // other shape the body stays and is judged as code. RESIDUAL, not covered:
    // a data file written here can still be executed by a LATER, separate tool
    // call. See `strip_data_here_document_bodies` for the exact shape rule.
    let stripped = strip_data_here_document_bodies(cmd);
    let cmd: &str = &stripped;

    // 1. Fork bomb (whitespace-insensitive signature).
    let compact: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(":(){") && compact.contains(":|:") {
        return Decision::deny("fork bomb pattern detected");
    }

    // 1b. Egress: bash's `/dev/tcp/`/`/dev/udp/` pseudo-devices open a raw
    // network socket in ANY position (redirect, `exec N<>`, bare read), not
    // only the `>`-redirect form rule 2 below happens to catch. See
    // `contains_dev_tcp_or_udp`.
    if contains_dev_tcp_or_udp(cmd) {
        return Decision::deny(
            "`/dev/tcp/`/`/dev/udp/` opens a raw network socket — this is a network egress \
primitive, not a filesystem path",
        );
    }

    // 2. Truncating `>` redirects (quote-aware, ignores >>, &>> and fd dups
    //    like `2>&1`/`>&2`; but catches EVERY `N>file` truncating form for any
    //    fd N, plus the combined stdout+stderr truncating form `&>`). Scan
    // *every* redirect on the line, not just the first: a safe early redirect
    // (`> /dev/null`) must not blind the gate to a later truncating redirect in
    // a subsequent `;`/`&&`/`|` segment.
    // Asks recorded by the two line-level scans below (they run before the
    // `VerdictAcc` the per-segment loop uses exists). They must NOT
    // short-circuit: `echo x > target/log && rm -rf /usr` has a confined
    // redirect AND an unconfined delete, and the Deny has to win.
    let mut line_level_asks: Vec<Decision> = Vec::new();
    // The redirect scans of 2 and 2b read `rcmd`, not `cmd`: the body of a
    // QUOTED here-document fed to `cat`/`tee`/`git commit -F -` is removed there
    // whatever the opener writes to, because the shell never reads a `>` in
    // that body as a redirect (backlog 1fd5a731). The opener line stays, so its
    // own target is still judged. Every rule from 2c on reads `cmd`. See
    // `redirect_view_without_quoted_here_document_body` for the shape rule.
    let redirect_view = redirect_view_without_quoted_here_document_body(cmd);
    let rcmd: &str = &redirect_view;
    // Per-occurrence segment indices, or `None` when the per-segment scan could
    // not be corroborated against this line-level one. `None` resolves nothing
    // and judges every raw token exactly as before this existed.
    let redirect_segments = redirect_target_occurrences(rcmd);
    // The cwd in force before each segment, so a RELATIVE redirect target can
    // be judged where it actually lands (f1c170ab). See `place_redirect_target`.
    let seg_cwds = segment_cwd_states(rcmd);
    // Which segments are interpreter program text (4a1cf1ad); see
    // `interpreter_program_body_segments`.
    let program_body = interpreter_program_body_segments(rcmd);
    for (occurrence, raw_target) in redirect_targets(rcmd).into_iter().enumerate() {
        let seg_idx = redirect_segments
            .as_ref()
            .and_then(|occ| occ.get(occurrence))
            .map(|(seg_idx, _)| *seg_idx);
        // Resolve `> "$P"` to the path it will actually name BEFORE any axis
        // judges it: every check below asks a question about a path, and the
        // unresolved token answers none of them (see
        // `resolve_redirect_target_at`). Falling back to the raw token keeps the
        // pre-existing verdict for everything that cannot be resolved.
        let target = seg_idx
            .and_then(|seg_idx| resolve_redirect_target_at(rcmd, seg_idx, &raw_target))
            .unwrap_or(raw_target);
        // `..` kept for the symlink resolver (see `physical_redirect_spelling`).
        let physical = physical_redirect_spelling(seg_idx, &seg_cwds, &target);
        let target = match place_redirect_target(rcmd, seg_idx, &seg_cwds, target) {
            Ok(placed) => placed,
            Err(raw) => {
                // Unplaced: no base to resolve symlinks against, so lexical
                // only; the unplaceable Ask below covers the rest.
                if let Some(deny) = protected_path_lexical_block(ctx, "redirect", &raw) {
                    return deny;
                }
                line_level_asks.push(unplaceable_redirect_ask(">", &raw));
                continue;
            }
        };
        if let Some(deny) = protected_path_block_at(ctx, "redirect", &target, &physical) {
            return deny;
        }
        if !redirect_target_is_safe(&target, ctx) {
            // SYSTEM-DIRECTORY axis, checked BEFORE recoverability for the same
            // reason the protected-path axis above is: the recoverability probe
            // reports `NothingToDestroy` for a path that does not exist yet, and
            // `continue`s past it into Allow. That is the correct answer to the
            // question it was asked and the wrong question for `/etc/sudoers.d`.
            if let Some(deny) = system_path_block("redirect", &target, ctx) {
                return deny;
            }
            // The same axis on the path the kernel really writes (2287ebae):
            // a symlink in the text can land a NEW file in `/etc` that the
            // lexical check above never sees.
            let landing_undetermined = match real_system_path_block("redirect", &physical, ctx) {
                Ok(Some(deny)) => return deny,
                Ok(None) => None,
                Err(why) => Some(why),
            };
            // RECOVERABILITY runs FIRST (operator ruling 2026-09-09). The two
            // verdicts this precedes — the confined ask and the flat deny —
            // fired 63 times across 25 transcripts, and neither had ever asked
            // the only question that decides whether the write costs anything:
            // are these bytes recoverable afterwards? The deny even asserted
            // `truncates and overwrites an existing file` while NOTHING in the
            // crate checked existence — a §4 divergence between the reason a
            // human reads and the finding actually made.
            //
            // Order matters. Confinement is a LOCATION answer ("inside your
            // own tree"), which is weaker than a RECOVERY answer ("git holds
            // these bytes"): a scratch file with an hour of uncommitted work in
            // it is confined and still unrecoverable, while a tracked, clean
            // file outside the session root is recoverable and worth nobody's
            // attention. Asking recovery first lets the strong answer win.
            //
            // The protected-path check above already ran and is a SEPARATE
            // axis: a gate/config file is surfaced however recoverable it is,
            // because a silently disabled gate voids every later verdict. Only
            // targets that cleared it reach here.
            let recovery = crate::reversible::probe(&target, ctx.raw_base.as_deref());
            // RECOVERABLE **AND MINE**. Recoverability alone used to allow, and
            // that was a fail-open across project boundaries: any git-tracked
            // file in ANY checkout on this machine probes as recoverable, so
            // `echo x > <other project>/CLAUDE.md` was silently ALLOWED while
            // the same write inside this session's own tree was merely asked
            // about. Measured 2026-09-14 against 0.2.63.
            //
            // Operator ruling (2026-09-14): work inside THIS project that git
            // can restore may run unattended; another project is someone
            // else's tree and is an Ask whose recommended answer is "do not
            // run". "git can undo it" answers the COST question, not the
            // WHOSE-TREE question, so both must hold to skip the prompt.
            let runs_unattended = match recovery {
                // Nothing to lose, and that is true in ANY tree: a file that
                // does not exist has no prior bytes for the write to destroy.
                // Gating this on confinement was wrong and regressed
                // `bg2_system_dir_rule_does_not_swallow_ordinary_absent_targets`
                // — the location question only arises once there are bytes.
                // ...unless where it really lands could not be determined: then
                // the system-directory axis above did not actually look, and
                // "could not check" is not "clean" (CLAUDE.md §3).
                crate::reversible::Recovery::NothingToDestroy => {
                    if let Some(why) = &landing_undetermined {
                        line_level_asks.push(Decision::ask(format!(
                            "'> {target}' creates a file, and {why} — blastguard cannot tell \
whether it lands in a system directory"
                        )));
                        continue;
                    }
                    true
                }
                // THIS is the whose-tree case. Any git-tracked file in ANY
                // checkout on the machine probes as restorable, so before this
                // gate `echo x > <other project>/CLAUDE.md` was silently
                // ALLOWED while the same write inside this session's own tree
                // was merely asked about (measured 2026-09-14 against 0.2.63).
                // "git can undo it" answers the COST question, not the
                // WHOSE-TREE one, so both must hold to skip the prompt.
                //
                // A linked worktree checkout under this session's worktree
                // storage roots is this session's tree too (user ruling
                // 2026-10-03: refusing edits inside a worktree is itself the
                // defect), even when the session runs in the main tree and the
                // checkout is therefore not one of its safe roots. Judged by
                // `Ctx::in_worktree_checkout`: absolute target, one plain
                // command, placement observed with no symlink below the
                // checkout.
                crate::reversible::Recovery::RecoverableFromGit => {
                    ctx.confined_root("redirect", &[target.as_str()]).is_some()
                        || ctx.in_worktree_checkout(&target)
                }
                // An existing file whose bytes git does not hold, but which
                // sits in a fixed temp root (`/tmp`, `/private/tmp`; never
                // `$TMPDIR`) outside any git tree: disposable work (user ruling
                // 2026-10-28: a truncating redirect inside /private/tmp is
                // recoverable work and must not ask). Only for a fully
                // determined placement — see `temp_disposable_redirect_target`;
                // and never when where it really lands could not be determined.
                crate::reversible::Recovery::Unrecoverable(_) => {
                    landing_undetermined.is_none() && temp_disposable_redirect_target(&target, ctx)
                }
                // Undetermined keeps falling through, per CLAUDE.md §3, and so
                // does every Unrecoverable target the arm above refuses.
                _ => false,
            };
            if runs_unattended {
                continue;
            }
            // NOT SPLIT THREE WAYS HERE, deliberately. `Undetermined` and
            // `Unrecoverable` both fall through to the Deny below, so this arm
            // blocks exactly the set 0.2.58 blocked minus the recoverable ones
            // — it never turns a former Deny into something weaker.
            //
            // Splitting them was tried and backed out. By `model.rs`'s own
            // doctrine an undetermined answer is a refusal and refusals are
            // `Ask`, which would also make an unplaceable `$TMPDIR` target
            // passable instead of a dead end. But every relative target in an
            // unscoped `detect::detect` call is undetermined for want of a cwd,
            // so the split moved about a dozen established denies to asks —
            // relitigating deny-vs-ask for a whole class this change had no
            // need to touch, and buying nothing measurable (2067 real commands:
            // 163 non-allow either way, the split only shifting 24 of them from
            // deny to ask). The distinction is still visible: the reason text
            // says which answer was reached, and `rule_id` files them under
            // different ids.
            // LOCATION axis. Safe to consult for every target that reaches
            // here: `place_redirect_target` has already re-expressed a relative
            // target against the cwd its segment runs in (or refused it above),
            // so `cd /usr && echo x > lib/f` is judged as `/usr/lib/f` and can
            // no longer come out "confined" to the session tree.
            if let Some(root) = ctx.confined_root("redirect", &[target.as_str()]) {
                line_level_asks.push(confined_ask(
                    "truncating redirect",
                    &root,
                    &[target.as_str()],
                ));
                continue;
            }
            // A recoverable target only reaches here when the whose-tree check
            // above failed, so the reason names that — it is why this refuses
            // (backlog 2dc70bc5: the bare description used to say the gate
            // "does not refuse or ask about" the very path it was refusing).
            let observed = match recovery {
                crate::reversible::Recovery::RecoverableFromGit => format!(
                    "{}, but not one of this session's trees (neither inside its safe roots \
nor a recognised worktree checkout), so git being able to restore it does not make the \
overwrite this session's to make",
                    recovery.describe()
                ),
                _ => recovery.describe(),
            };
            let reason =
                format!("'> {target}' destroys the file's current contents and {observed}");
            // User ruling R2 (2026-10-14): inside interpreter PROGRAM TEXT a
            // redirect whose target cannot be determined is not denied here;
            // it is handed back to `interpreter_code_verdict`, which resolves
            // it to an Ask (never Allow) unless the program holds a Deny. Only
            // the undeterminable-target rows qualify — an existing
            // unrecoverable target (`/etc/hosts`) is a definite finding and
            // still denies. See `Ctx::program_text_redirects`.
            if let Some(sink) = ctx.program_text_redirects {
                if matches!(recovery, crate::reversible::Recovery::Undetermined(_))
                    && redirect_target_undeterminable(&target, ctx.raw_base.as_deref())
                {
                    sink.borrow_mut().push(reason);
                    continue;
                }
            }
            // The same ruling at LINE level (4a1cf1ad): this `>` sits in the
            // body of a quoted here-document handed to a code interpreter, so
            // it is program text the shell never parses. Ask with the marker,
            // never Allow; a Deny elsewhere on the line still wins.
            if matches!(recovery, crate::reversible::Recovery::Undetermined(_))
                && redirect_target_undeterminable(&target, ctx.raw_base.as_deref())
                && seg_idx.is_some_and(|i| program_body.get(i) == Some(&true))
            {
                line_level_asks.push(recommend_against(Decision::ask(format!(
                    "the body of a quoted here-document fed to a code interpreter contains a `>` \
that a shell would read as a redirect whose target cannot be determined ({reason}); the shell \
does not parse that body, but blastguard cannot show the program does not use it as one."
                ))));
                continue;
            }
            return Decision::deny(reason);
        }
    }

    // 2b. APPEND redirects (`>>`, `&>>`). Appending does not truncate, so it is
    //     ordinarily allowed and deliberately skipped by `redirect_targets` —
    //     but "does not truncate" is a statement about the file's old bytes,
    //     not about blast radius. One appended line is enough to add a hook
    //     entry to a settings file or a command to `.githooks/pre-commit`, so a
    //     PROTECTED target is denied regardless of the append/truncate
    //     distinction. Ordinary appends (`echo x >> /tmp/log`) stay allowed.
    //
    //     The target is resolved exactly like the truncating one above
    //     (a83802ad): `P=/etc/fstab; echo x >> $P` names `/etc/fstab`, and
    //     judging the raw `$P` token instead let it through every axis.
    let append_segments = append_target_occurrences(rcmd);
    for (occurrence, raw_target) in append_redirect_targets(rcmd).into_iter().enumerate() {
        let seg_idx = append_segments
            .as_ref()
            .and_then(|occ| occ.get(occurrence))
            .map(|(seg_idx, _)| *seg_idx);
        let target = seg_idx
            .and_then(|seg_idx| resolve_redirect_target_at(rcmd, seg_idx, &raw_target))
            .unwrap_or(raw_target);
        let physical = physical_redirect_spelling(seg_idx, &seg_cwds, &target);
        let target = match place_redirect_target(rcmd, seg_idx, &seg_cwds, target) {
            Ok(placed) => placed,
            Err(raw) => {
                // Unplaced: lexical only, as for `>` above.
                if let Some(deny) = protected_path_lexical_block(ctx, "append redirect", &raw) {
                    return deny;
                }
                line_level_asks.push(unplaceable_redirect_ask(">>", &raw));
                continue;
            }
        };
        if let Some(deny) = protected_path_block_at(ctx, "append redirect", &target, &physical) {
            return deny;
        }
        // The system-directory axis does not care about the truncate/append
        // distinction either: `>>` is how a `paths.d` entry or an
        // `authorized_keys` line gets added, and one appended line is the
        // whole payload.
        if let Some(deny) = system_path_block("append redirect", &target, ctx) {
            return deny;
        }
        // ...and on the path the kernel really writes (2287ebae): a symlink in
        // the text hides the system directory from the lexical check.
        match real_system_path_block("append redirect", &physical, ctx) {
            Ok(Some(deny)) => return deny,
            Ok(None) => {}
            Err(why) => line_level_asks.push(Decision::ask(format!(
                "'>> {target}' appends to a path whose real location is unknown ({why}) — \
blastguard cannot tell whether it lands in a system directory"
            ))),
        }
        // A target that is STILL an expansion after resolution names no path
        // either axis above could place, so both answered "not mine" without
        // having looked (`echo x >> $SOMEWHERE` with `SOMEWHERE` from the
        // environment). "Could not check" is not "clean" (CLAUDE.md §3): Ask,
        // recorded rather than returned so a Deny elsewhere on the line wins.
        if has_unresolvable_expansion(&target) {
            line_level_asks.push(Decision::ask(format!(
                "'>> {target}' appends to a path that only exists at run time — blastguard \
cannot tell whether it is a protected gate/config file or a system directory"
            )));
        }
    }

    // 3. Per-command-segment analysis. Ranked: a Deny in ANY segment outranks
    //    an Ask in any other (see `VerdictAcc`).
    //
    // BG-cwd (verified bypass, ea1355f5): every rule above (and every rule
    // `analyze_segment` reaches) judges ONE segment's operands as literal
    // path TEXT, with no memory of what an EARLIER segment on the same line
    // did to the shell's working directory. Every protected pattern in
    // `exclude.rs` is path-shaped, so `cd .githooks && rm pre-commit` never
    // matched any of them: `.githooks` alone (the `cd` operand) touches a
    // protected path and is read-only, so it is exempt; `pre-commit` alone
    // (the `rm` operand) is a bare basename — and `./pre-commit`/
    // `sub/../pre-commit` are relative operands of the same shape — that
    // match no protected glob on their own. Neither segment, judged
    // independently, is the direct-path form
    // `rm .githooks/pre-commit` that every rule above already denies.
    //
    // `cwd`/`aliases` are threaded through the loop below (not recomputed per
    // segment) so a `cd`/`pushd` — or a `ln -s` alias resolved through one —
    // in an EARLIER segment is visible to a LATER one; see
    // `advance_cwd_and_rewrite`.
    let mut acc = VerdictAcc::default();
    // Fold in the line-level Asks collected above, now that there is an
    // accumulator to rank them against.
    for ask in line_level_asks {
        if let Some(deny) = acc.record(ask) {
            return deny;
        }
    }

    // 2c. Egress across `|` pipelines: a fetch/decode stage feeding a
    // shell/interpreter stage downstream of it. Run BEFORE the per-segment
    // loop below (which judges each segment in isolation and cannot see this
    // cross-segment relationship) so a Deny here outranks whatever the
    // per-segment loop finds, per `VerdictAcc`. See `analyze_pipe_egress`.
    if let Some(deny) = acc.record(analyze_pipe_egress(cmd)) {
        return deny;
    }

    // 2c-bis. The stdin MIRRORS of an inline-eval invocation: a here-document
    // body or a pipe delivering the program. Cross-segment for the same reason
    // 2c is — the per-segment loop sees tokens only, so the body and the
    // upstream stage are invisible to it. See `analyze_interpreter_stdin_exec`.
    if let Some(deny) = acc.record(analyze_interpreter_stdin_exec(cmd, depth, ctx)) {
        return deny;
    }

    // 2d. Egress via process substitution (`bash <(curl …)`): the same
    // remote-exec sink as 2c's `|` pipeline, reached through an ARGUMENT
    // instead of a pipe, so 2c cannot see it. See
    // `analyze_process_substitution_egress`.
    if let Some(deny) = acc.record(analyze_process_substitution_egress(cmd)) {
        return deny;
    }

    // 2e. Egress via here-string (`bash <<<"$(curl …)"`): the here-string
    // mirror of 2d's process-substitution scan, reached through the `<<<`
    // redirection operator instead of a `<(...)` argument. See
    // `analyze_here_string_egress`.
    if let Some(deny) = acc.record(analyze_here_string_egress(cmd)) {
        return deny;
    }

    // ENUMERATED, so `unknown_wrapper_ask` can ask `resolve_expanded_command_word`
    // what an expansion-valued command word in THIS segment was assigned by an
    // EARLIER one. `advance_cwd_and_rewrite` below rewrites a segment's text but
    // maps one segment to one segment, so `seg_idx` stays the segment's index in
    // `split_segments(cmd)` — which is what that resolver's parameter means.
    // Compound-command syntax (`for … in`, `do`, `then`, `{`, `case` patterns)
    // separated from the command each segment really runs; see
    // `compound_views`. Index-aligned with `split_segments(cmd)`.
    let views = compound_views(cmd); // The cwd, alias table and subshell-group bookkeeping, one walk for the
                                     // line and one per quoted here-document body (backlog 547cf2f6); see
                                     // `CwdWalks`. `segment_cwd_states` replays the same walks.
    let mut walks = CwdWalks::new(cmd, &views);
    // `for`/`select` variables bound so far on this line; see `loop_variants`.
    let mut bindings: Vec<LoopBinding> = Vec::new();
    for (seg_idx, seg) in split_segments(cmd).into_iter().enumerate() {
        let walk = walks.walk_for(seg_idx);
        let cwd = walk.cwd.clone();
        // The base for THIS segment's relative operands is the cwd state as it
        // stands BEFORE the segment runs — the same state
        // `advance_cwd_and_rewrite` rewrites the segment against — so it is
        // computed before `cwd` is advanced. See `Ctx` for why a resolved `cd`
        // moves only one of the two bases.
        let seg_ctx = match &cwd {
            CwdState::Root => ctx.clone(),
            CwdState::Known(dir) => ctx.after_cd(dir),
            CwdState::Unknown => ctx.after_unknown_cd(),
        };
        // A view that does not line up with this segment would mean the two
        // splitters disagree; judge the raw segment exactly as before rather
        // than a view of some other text.
        let (command, data, binding) = match views.get(seg_idx) {
            Some(v) => (v.command.as_str(), v.data.as_str(), v.binding.clone()),
            None => (seg.as_str(), "", None),
        };
        if !data.trim().is_empty() {
            if let Some(deny) = acc.record(analyze_expanded_data(data, depth, &seg_ctx)) {
                return deny;
            }
        }
        if let Some(b) = binding {
            bindings.push(b);
        }
        let cwd_before = cwd;
        let aliases_before = walk.aliases.clone();
        walk.scopes.enter(views.get(seg_idx));
        let (effective_seg, extra, next_cwd) =
            advance_cwd_and_rewrite(command, &cwd_before, &mut walk.aliases);
        walk.cwd = walk
            .scopes
            .leave(views.get(seg_idx), &cwd_before, next_cwd)
            .0;
        // The same segment once more for every value its loop variables take
        // (see `loop_variants`), IN ADDITION to the judgement of the text as
        // written below — so this can only add verdicts, never remove one.
        match loop_variants(command, &bindings) {
            LoopVariants::Unbound => {}
            LoopVariants::TooMany { name, count } => {
                if let Some(deny) = acc.record(Decision::ask(format!(
                    "the loop variable `${name}` takes {count} combinations of values here, more \
than blastguard evaluates ({MAX_LOOP_VARIANTS}) — it cannot tell what path this command reaches \
through it, and refuses to guess"
                ))) {
                    return deny;
                }
            }
            LoopVariants::Variants(variants) => {
                for variant in variants {
                    let mut variant_aliases = aliases_before.clone();
                    let (variant_seg, variant_extra, _) =
                        advance_cwd_and_rewrite(&variant, &cwd_before, &mut variant_aliases);
                    if let Some(d) = variant_extra {
                        if let Some(deny) = acc.record(d) {
                            return deny;
                        }
                    }
                    if let Some(deny) =
                        acc.record(analyze_segment(&variant_seg, depth, cmd, seg_idx, &seg_ctx))
                    {
                        return deny;
                    }
                    if let Some(d) = high_blast_outside_tree_rm(&variant_seg) {
                        if let Some(deny) = acc.record(d) {
                            return deny;
                        }
                    }
                }
            }
        }
        if let Some(extra_decision) = extra {
            if let Some(deny) = acc.record(extra_decision) {
                return deny;
            }
        }
        if let Some(deny) = acc.record(analyze_segment(
            &effective_seg,
            depth,
            cmd,
            seg_idx,
            &seg_ctx,
        )) {
            return deny;
        }
        // High-blast Ask tier (C): an outside-tree rm/unlink. Judged on
        // `effective_seg` (the cwd-resolved segment, same input
        // `analyze_segment` just used above), not the raw one — see
        // `high_blast_outside_tree_rm`'s doc comment for why that matters.
        if let Some(d) = high_blast_outside_tree_rm(&effective_seg) {
            if let Some(deny) = acc.record(d) {
                return deny;
            }
        }
    }

    // 4. High-blast Ask tier (A, B): novel, narrowly-bounded
    // destructive/irreversible shapes with no rule arm above (git
    // force-push, schemeless curl/wget upload). Run LAST so any Deny already
    // recorded above (including from this same loop) outranks it — see
    // `analyze_high_blast_tier`.
    if let Some(deny) = acc.record(analyze_high_blast_tier(cmd)) {
        return deny;
    }

    acc.finish()
}

/// Judge ONE `;`/`&&`/`|`-separated segment of a command line.
///
/// `line`/`seg_idx` locate this segment inside the command line it came from,
/// and exist for two consumers: [`unknown_wrapper_ask`]'s head check (reads an
/// expansion-valued command word back off EARLIER segments, see
/// [`resolve_expanded_command_word`]) and `find_xargs::xargs_pipe_feed` (reads
/// the stage that feeds an `xargs rm`). Nothing else here uses them.
fn analyze_segment(seg: &str, depth: usize, line: &str, seg_idx: usize, ctx: &Ctx<'_>) -> Decision {
    let tokens = split_substitution_opening_assignments(seg.split_whitespace().collect());
    let mut acc = VerdictAcc::default();

    // D2: a wrapper that takes a shell command string via `-c` (flock) runs that
    // string through `sh -c`. Re-analyse it exactly like a shell's own `-c`
    // payload. Scanned position-independently so both `flock FILE -c '…'` and
    // `flock -c '…' FILE` are covered.
    if depth < MAX_SHELL_DEPTH {
        for (i, t) in tokens.iter().enumerate() {
            if takes_shell_command_flag(&normalized_command(t)) {
                let rest = &tokens[i + 1..];
                // Both spellings of flock's command flag: the short `-c` (via
                // `dash_c_payloads`) and the long `--command`/`--command=…`.
                let mut payloads = dash_c_payloads(rest);
                if let Some(pos) = rest
                    .iter()
                    .position(|t| *t == "--command" || t.starts_with("--command="))
                {
                    // Detached long form `--command <payload>`: payload is the
                    // token(s) AFTER the flag.
                    payloads.extend(payloads_after(rest, pos));
                    // Attached long form `--command=<payload>`: the value is
                    // glued to the flag token by `=`. Reconstruct it so the
                    // same candidate logic applies — otherwise the inline value
                    // is never extracted and `flock /l --command='rm -rf /'`
                    // stayed Allow at depth 0 (138607bc).
                    if let Some(eq) = rest[pos].find('=') {
                        let inline = &rest[pos][eq + 1..];
                        payloads.extend(inline_command_payloads(rest, pos, inline));
                    }
                }
                for payload in payloads {
                    // Shell-eval position: an unresolvable command word here is
                    // an Ask, not a silent Allow.
                    if let Some(deny) = acc.record(analyze_shell_payload(&payload, depth, ctx)) {
                        return deny;
                    }
                }
                // Backstop for backslash-over-escaped `flock -c` payloads that
                // defeat the structured extraction above (99b506b7, twin of the
                // shell `-c` arm). See `denoised_eval_rescan`.
                if let Some(deny) = acc.record(denoised_eval_rescan(&rest.join(" "), depth, ctx)) {
                    return deny;
                }
            }
        }
        // daf7611b: `env -S "<line>"` / `--split-string` splits ONE quoted
        // operand into a command line and execs it. That operand is a
        // multi-word quoted string, which `command_candidates` deliberately
        // never reads as a command word, so it must be extracted here — the
        // same position-independent scan as flock's `-c` above.
        for (i, t) in tokens.iter().enumerate() {
            if normalized_command(t) != "env" {
                continue;
            }
            for payload in executors::env_split_string_payloads(&tokens[i + 1..]) {
                if let Some(deny) = acc.record(analyze_shell_payload(&payload, depth, ctx)) {
                    return deny;
                }
            }
        }
        // 675eb991: `GIT_PAGER='rm -rf ~/x' git log` — an environment
        // variable whose value a program runs as a command line. See
        // `executors::env_exec_var_verdict`.
        if let Some(deny) = acc.record(executors::env_exec_var_verdict(seg, line, depth, ctx)) {
            return deny;
        }
        // 06345780: a `$(...)`/backtick body is EXECUTED to produce its text,
        // wherever it stands — command word, operand, assignment value or
        // inside double quotes. Judge each body with the full analyser, so
        // `echo $(rm -rf ~/work)` gets the verdict `rm -rf ~/work` gets. The
        // egress scans (`command_substitution_payloads`) only ever asked
        // "does it fetch/decode?", which left every other destructive body
        // unexamined. See `executed_substitution_payloads`.
        for payload in executed_substitution_payloads(seg) {
            if let Some(deny) = acc.record(analyze_shell_payload(&payload, depth, ctx)) {
                return deny;
            }
        }
    } else {
        // Cap reached: the recursion above did NOT run, so this segment was not
        // fully analysed. Record the unfinished-analysis Ask into `acc` rather
        // than returning early, so a Deny found by a later non-recursive rule in
        // this same function still outranks it (see `VerdictAcc::record`).
        let _ = acc.record(depth_exhausted());
    }

    // Memory rule: backticks in a double-quoted note / commit message are
    // command-substituted and silently eaten. Recorded (not returned) so a
    // Deny elsewhere on the segment still outranks this Ask.
    if let Some(ask) = backtick_in_double_quoted_text(seg) {
        let _ = acc.record(ask);
    }

    if let Some(deny) = acc.record(rm::known_path_harm(&tokens, line, seg_idx, ctx)) {
        return deny;
    }
    // D3: analyse EVERY candidate command-word position; deny if ANY is
    // destructive. See `command_candidates`.
    for idx in command_candidates(&tokens) {
        if let Some(deny) = acc.record(harm_mark::analyze_command_at(
            &tokens,
            idx,
            depth,
            Some(seg),
            ctx,
        )) {
            return deny;
        }
    }
    if let Some(deny) = acc.record(find_xargs::xargs_pipe_feed(&tokens, line, seg_idx, ctx)) {
        return deny;
    }

    // ASK-2: an UNRECOGNISED wrapper standing in front of a destructive command
    // line. See `unknown_wrapper_ask`.
    if let Some(deny) = acc.record(unknown_wrapper_ask(&tokens, depth, line, seg_idx, ctx)) {
        return deny;
    }

    interpreter::python_body_segment_verdict(line, seg_idx, acc.finish())
}

/// ASK-2: an unrecognised command word standing in front of something that
/// parses as a DESTRUCTIVE command line.
///
/// `command_candidates` only widens the candidate set behind a KNOWN wrapper
/// name (`sudo`, `env`, `timeout`, …). Behind anything else it returns the head
/// position alone, so `my-cleanup-wrapper rm -rf /some/path` resolved to the
/// single word `my-cleanup-wrapper`, matched no rule arm, and was ALLOWED with
/// its payload never examined. The wrapper list cannot be completed — a local
/// script, a `just` recipe or a `Makefile` shim is a wrapper blastguard has
/// never heard of.
///
/// It is NOT a Deny: unlike `sudo`, we do not know that this program execs its
/// arguments at all. It genuinely is the unknown middle, so it asks.
///
/// Narrowness, which is the whole difficulty here:
///   * the head must be UNRECOGNISED — every rule arm and every known wrapper
///     has already had its say above, and a Deny from there outranks this;
///   * only ONE tail is examined: the first token after the head that is not a
///     flag, not empty, and not the opening fragment of a quoted STRING
///     (`opens_unclosed_quote`, so `mytool -x 'rm -rf' file` reads `'rm` as the
///     DATA it is and never asks);
///   * that tail must itself analyse to a full DENY. `mytool report.txt`,
///     `cargo test`, `npm run build`, `gh pr list` all analyse to Allow and are
///     silent. Only a genuinely destructive command line behind an unknown word
///     asks.
///
/// Examining exactly one tail (rather than every position, as
/// `command_candidates` does behind a known wrapper) also keeps the fan-out at
/// one child per segment. That matters: fanning out per position is what made
/// the recursion tree exponential in D4.
///
/// Missing an ask because the real payload sat at a position this scan skipped
/// is not a regression — that case was, and remains, the pre-existing Allow.
fn unknown_wrapper_ask(
    tokens: &[&str],
    depth: usize,
    line: &str,
    seg_idx: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    if depth >= MAX_SHELL_DEPTH {
        return depth_exhausted();
    }
    // Resolve the head exactly as `analyze_segment` did.
    let candidates = command_candidates(tokens);
    // Behind a known wrapper `command_candidates` returns many positions; this
    // rule is only about the single-candidate (non-wrapper head) shape.
    let [idx] = candidates[..] else {
        return Decision::Allow;
    };
    // CA-blastguard-017 (verified bypass): the command word ITSELF may be an
    // unresolvable expansion (`$RM`, `${RM}`, backtick/`$()` substitution).
    // Falling through to the tail-only destructiveness check below silently
    // assumed a benign program: the tail is only the ARGUMENTS (`-rf /path`),
    // and a bare path/flag list rarely parses as destructive on its own, so
    // `$RM -rf /path` reached Allow with the actual program never examined.
    // This is exactly the condition `unresolvable_command_word` already asks
    // about for shell-eval payloads (`sh -c "$CMD"`) — the top-level command
    // word deserves the same treatment, checked on the RAW token for the same
    // reason `unresolvable_command_word` does (see its own comment).
    //
    // "Unresolvable" is where this arm STARTS, not where it ends: the same
    // command line very often assigns the name a literal a few segments
    // earlier (`BIN=/path/to/tool` … `"$BIN" status`), and then the program
    // this runs IS in the text. `resolve_expanded_command_word` answers that
    // in three values, and only its `Literal` is permissive enough to act on.
    //
    // NOT a command word at all (FP2, backlog ef015a66): a backtick-headed word
    // on a BODY line of a quoted-delimiter here-document read by Python
    // (`python3 - <<'PY'` … `s.replace('a','Run \`x y\`')` … `PY`). The shell
    // does not parse that body, and Python never runs a backtick as a shell
    // command, so nothing ever executes this "command word". The word is then
    // judged like any other unrecognised head below, and the program itself
    // is still audited by `interpreter_code_verdict`. Perl/Ruby/PHP bodies,
    // where a backtick IS a shell substitution, are not covered and keep the
    // Ask.
    let literal_backtick_in_python_body = tokens[idx].starts_with('`')
        && python_program_body_segments(line).get(seg_idx) == Some(&true);
    if has_unresolvable_expansion(tokens[idx]) && !literal_backtick_in_python_body {
        match resolve_expanded_command_word(line, seg_idx, tokens[idx]) {
            // SUBSTITUTIVE, never a skip. The resolved program replaces the
            // expansion token and the rewritten segment goes back through the
            // full rule engine, so `RM=/bin/rm; $RM -rf /path` reaches the same
            // DENY that `rm -rf /path` does.
            //
            // Suppressing the ask here instead — "a literal was found, so stop
            // asking" — would re-open CA-blastguard-017 under a new name: the
            // tail this function goes on to examine is only the ARGUMENTS, and
            // `-rf /path` is a flag and a bare path that does not parse as
            // destructive on its own. That is precisely how the unexamined head
            // reached Allow in the first place.
            //
            // The recursion terminates: `Literal` is only returned for a value
            // that `has_unresolvable_expansion` rejected, so the rewritten head
            // cannot re-enter this arm. `depth + 1` respects `MAX_SHELL_DEPTH`
            // like every other re-analysis in this file, and the guard at the
            // top of this function is the one that stops the descent.
            CommandWordOrigin::Literal(program) => {
                let mut rewritten: Vec<&str> = tokens.to_vec();
                rewritten[idx] = program.as_str();
                let resolved = rewritten.join(" ");
                // The rewritten text is one whole segment, so it is its own
                // line and sits at index 0 of it. Passing the ORIGINAL
                // `(line, seg_idx)` would be wrong: the tokens no longer come
                // from that position.
                let verdict = analyze_segment(&resolved, depth + 1, &resolved, 0, ctx);
                // …but one thing the tokens DID carry from that position is
                // gone, and it is not recoverable here. `advance_cwd_and_rewrite`
                // rewrites a segment's relative operands against the cwd an
                // earlier `cd`/`pushd` established, and it does so only for a
                // head `verb_targets` recognises. `$BIN` is not one, so by the
                // time the head is resolved the operand has already lost its
                // directory prefix. Verified: `cd .githooks; rm pre-commit` is
                // DENIED, while `cd .githooks; BIN=/bin/rm; $BIN pre-commit`
                // re-judges the bare `rm pre-commit` and reached Allow —
                // resolving the head made the verdict WEAKER than spelling it
                // out. The cwd state lives in `detect_bash`'s loop and is not
                // threaded this far, so this arm cannot redo that rewrite; what
                // it can do is refuse to call the result clean. A Deny still
                // wins (it was found on strictly less information than the real
                // command has); anything softer falls back to the Ask this arm
                // owed before the head was resolved.
                if !verdict.is_deny()
                    && cwd_rewrite_was_lost(tokens, &rewritten)
                    && line_changes_cwd_before(line, seg_idx)
                {
                    let head = tokens[idx];
                    return Decision::ask(format!(
                        "the command word `{head}` resolves to `{program}`, but an earlier `cd`/`pushd` on this line moved the working directory and this segment's relative operands were never resolved against it"
                    ));
                }
                return verdict;
            }
            // Both restrictive answers keep the pre-existing Ask. They are
            // reported separately by the resolver because they are different
            // facts, but this arm owes the same thing to each: it still cannot
            // name the program.
            // MEASURED FRICTION, DELIBERATELY LEFT IN PLACE. This is the single
            // largest ask class in the field — 67 of 223 non-allow verdicts
            // across 25 transcripts (2026-09-09) — and every one of them was a
            // local tool held in a variable: `$B brief --json …`,
            // `"$C/bin/specforge" --help`, `$SF draft --help`. Not one
            // destroyed anything, so by the operator's ruling the gate is
            // spending attention it should not.
            //
            // It is NOT flattened to Allow, because the arm is over-firing, not
            // superfluous: the eleven tests around
            // `a_head_whose_value_the_line_does_not_pin_down_still_asks` show
            // it is what catches `$RM -rf /some/path`, and a delete is not one
            // of the reads and writes the ruling freed. Two narrowings were
            // considered and rejected here rather than shipped:
            //   * allow when the operands are recoverable — makes the verdict
            //     depend on whether a path happens to exist today, so
            //     `$RM -rf ~/work` would be asked about only until the first
            //     time it succeeded;
            //   * allow when no force/recursive flag appears — a denylist of
            //     flags, which is the same unsound shape as the v0.2.57 effect
            //     tokens.
            // The real repair is upstream, in what `CommandWordOrigin` can
            // resolve: most of the 67 are `AssignedButUnknowable` because the
            // value came from `$(mktemp -d)` or contained `$HOME`, and those
            // are paths, not verbs. That is its own measurement and its own
            // change.
            CommandWordOrigin::NoReachingAssignment | CommandWordOrigin::AssignedButUnknowable => {
                let head = tokens[idx];
                return Decision::ask(format!("the command word `{head}`{EXPANSION_HEAD_TAIL}"));
            }
        }
    }
    if is_recognized_command(&normalized_command(tokens[idx])) {
        return Decision::Allow;
    }
    // First plausible command-line start behind the unknown head. Redirect
    // punctuation is skipped here and truncates the tail below; see the note on
    // `end` for why.
    //
    // A token that OPENS an unclosed quote begins a quoted STRING, and the
    // whole string — not just that first token — is data. Skipping only the
    // opening token left the scan free to resume INSIDE the string, so
    // `echo 'eval rm -rf /'` skipped `'eval` and then started the tail at `rm`,
    // asking about `echo` over text that is an argument, never a command. So
    // `in_quote` runs to the token that closes the quote.
    let mut in_quote: Option<char> = None;
    let mut start = None;
    for (j, &t) in tokens.iter().enumerate().skip(idx + 1) {
        if let Some(q) = in_quote {
            // The closing token ends the string; it is still data itself. An
            // unterminated quote never closes, so the whole rest of the line is
            // data and no ask is raised — the safe direction here, since an
            // unbalanced quote means the shell would not run it as written.
            if t.contains(q) {
                in_quote = None;
            }
            continue;
        }
        if let Some(q) = unclosed_quote_char(t) {
            in_quote = Some(q);
            continue;
        }
        if t.is_empty() || t.starts_with('-') || is_redirect_token(t) {
            continue;
        }
        start = Some(j);
        break;
    }
    let Some(start) = start else {
        return Decision::Allow;
    };
    // The tail STOPS at the first redirect token (and at a bare operator's
    // target, which is punctuation too). Redirects belong to the outer command
    // line and were already judged in full by the caller before this rule ran;
    // re-analysing them here attributes the OUTER line's redirect to the head,
    // which turned `cargo test 2>&1`, `make >&2` and `echo x >&2` into asks
    // about `cargo` / `make` / `echo`. Re-joining tokens with a single space
    // also re-serialises redirect punctuation into a shape the redirect
    // classifier reads differently from the original text, so the re-parse was
    // not even asking about the same redirect.
    //
    // Dropping them costs nothing: a destructive REDIRECT is a deny from the
    // outer analysis regardless of what the head is, so nothing is missed.
    let end = tokens
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, t)| is_redirect_token(t))
        .map_or(tokens.len(), |(j, _)| j);
    let tail = tokens[start..end].join(" ");
    if tail.trim().is_empty() {
        return Decision::Allow;
    }
    // A DESTRUCTIVE tail asks — and so does an UNDETERMINED one.
    //
    // This used to read `if detect_bash(&tail, depth + 1, ctx).is_deny()`, with the
    // comment: "An Ask from the tail is not propagated: it would already have
    // been reported by whichever construct produced it if that construct were
    // reachable." That is a claim about reachability, and it is false.
    // Measured on 0.2.38, before this change:
    //
    //     dlx rm -rf /                   -> ask
    //     pnpm dlx rm -rf /              -> ALLOW
    //     cargo run rm -rf /             -> ALLOW
    //     myrunner myrunner rm -rf /     -> ALLOW
    //     a b rm -rf /                   -> ALLOW
    //
    // Nothing reports the inner Ask, because the only construct that would have
    // is THIS one, one frame down, and its answer was being discarded here. So
    // prefixing any unrecognised token defeated the unknown-wrapper rule
    // outright, and the comment above documented that as deliberate.
    //
    // The nested Ask is not a weaker signal than a Deny. It is the same
    // sentence this function itself emits — "there is a destructive command
    // line here and I cannot tell whether this verb runs it" — so collapsing it
    // to Allow converts "could not determine" into "fine" (CLAUDE.md 3),
    // through the crate's own recursion rather than through a raw IO edge.
    //
    // An `Allow` tail is still an Allow: `a b ls` analyses `b ls`, gets Allow,
    // and stays Allow. Only the undetermined answer propagates, so this widens
    // what is questioned without widening what is refused.
    //
    // NOT every nested Ask propagates, and the first attempt at this fix showed
    // why. Propagating all of them made `echo $(date 2>&1)` an Ask: the tail
    // `$(date 2>&1)` raises the UNRESOLVABLE-EXPANSION Ask, because in tail
    // position the analyser reads `$(date` as a command word. That doubt is
    // real when an expansion sits in COMMAND position and a misread when it
    // sits in ARGUMENT position, and this frame cannot tell which — so
    // forwarding it would ask about ordinary work on the strength of a
    // distinction not being made.
    //
    // A nested ask from THIS SAME RULE propagates; see `UNKNOWN_VERB_ASK` for
    // why the forward set is that one class and what the other classes still
    // cost.
    match detect_bash(&tail, depth + 1, ctx) {
        Decision::Deny(_) => {
            let head = tokens[idx];
            Decision::ask(format!(
                "`{head}` is not a command blastguard recognises, and what follows it parses as a destructive command line — blastguard cannot tell whether `{head}` runs it"
            ))
        }
        Decision::Ask(inner) if inner.contains(UNKNOWN_VERB_ASK) => {
            let head = tokens[idx];
            Decision::ask(format!(
                "`{head}` is not a command blastguard recognises, and neither is what follows it — {inner}"
            ))
        }
        Decision::Ask(_) | Decision::Allow => Decision::Allow,
    }
}

/// `seg` is the raw segment text `tokens` were whitespace-split from, when
/// there is one (`None` for a de-noised rescan). It is consulted by exactly one
/// consumer, the whole-tree-staging check ([`git_stage`]), which needs the
/// argument text WITH its newlines to read a here-document inside a
/// `-m "$(cat <<'EOF' … EOF)"` value; every other rule judges `tokens`.
fn analyze_command_at(
    tokens: &[&str],
    idx: usize,
    depth: usize,
    seg: Option<&str>,
    ctx: &Ctx<'_>,
) -> Decision {
    // CA-blastguard-013 (verified bypass): the command word must be UNQUOTED
    // before basename matching. `\rm -rf /some/path` (the standard alias-bypass
    // idiom) and `"rm" -rf /some/path` previously matched no rule arm and fell
    // through to Allow. See `normalized_command` for what this covers and what
    // stays out of reach.
    let cmd = normalized_command(tokens[idx]);
    let cmd = cmd.as_str();
    let rest = &tokens[idx + 1..];
    // Collects verdicts from the shell-evaluation re-analysis arms below, so an
    // Ask raised there survives the `match` that follows while still losing to
    // any Deny (see `VerdictAcc`).
    let mut acc = VerdictAcc::default();

    // A *genuine* help invocation never destroys anything — but only when the
    // help flag is the command's ONLY argument.
    //
    // CA-blastguard-011 (verified bypass): the previous rule short-circuited on
    // `rest.iter().any(is_help_flag)`, so appending `--help` anywhere disabled
    // every Bash rule. That assumed the flag makes the command print help and
    // exit, which is false for the BSD userland on macOS: BSD `rm` has no
    // `--help` option, so the token is taken as a plain filename operand and
    // `rm -rf <dir> --help` exits 0 after DELETING `<dir>`. The same holds for
    // any other operand ordering, because a command that does not know the flag
    // treats it as data, and a command that does know it may still have already
    // consumed the destructive operands (GNU getopt permutes, BSD getopt stops
    // at the first non-option — neither is safe to assume).
    //
    // The property that actually makes a help invocation safe is that there is
    // no OPERAND to destroy — not that the help flag is the only token.
    //
    // CA-blastguard-015 (false positive introduced by the `rest.len() == 1`
    // form): ANY second token re-enabled the unconditional-deny arms, including
    // a pure REDIRECT that carries no operand at all. `shred --help 2>&1 | head`,
    // `truncate --help 2>/dev/null` and `tee --help 2>&1` are ordinary usage,
    // and blastguard is a hard block whose stated bias is that ambiguity falls
    // through to Allow — denying them is exactly the kind of interference the
    // module header forbids.
    //
    // Narrowing justification (no fail-open): the operand test is computed the
    // same way `analyze_rm` computes operands (`!t.starts_with('-')`), so any
    // destructive target still counts and is still denied — `rm -rf /path --help`
    // (operand `/path`) and `rm --help *` (operand `*`) both keep their DENY,
    // which is the whole point of CA-blastguard-011 above. Only redirect
    // operators and their targets are excluded, and those cannot themselves be
    // destroyed by the command.
    //
    // BG-2: the previous version of this comment claimed a truncating redirect
    // "is already denied earlier in `detect_bash` (rule 2)". That was FALSE for
    // space-separated `N>` with N != 1 (`shred --help 2> /some/path`), because
    // rule 2 skipped every non-stdout fd — so this skip really did open a hole.
    // `redirect_targets` no longer has that fd-based skip: EVERY `N> target`
    // (any fd, attached or space-separated) is now read as truncating and
    // denied in rule 2 before segment analysis runs. What `has_operand` skips
    // is therefore only what rule 2 has already vetted (a truncating target it
    // judged safe) or what cannot truncate at all — append `>>`, fd dups
    // (`2>&1`), and input redirects (`<`). With no operand left, there is
    // nothing to
    // destroy whichever way the command parses the flag (GNU getopt permutes,
    // BSD getopt stops at the first non-option — neither assumption is needed).
    if rest.iter().any(|t| *t == "--help" || *t == "-h") && !has_operand(rest) {
        return Decision::Allow;
    }

    // Shell-evaluation wrappers that would otherwise smuggle a destructive
    // command past the per-command analysis. We re-analyse the *inline*
    // command line they evaluate; an opaque file argument (e.g.
    // `source .venv/bin/activate`) re-analyses to a harmless path and stays
    // allowed, preserving the no-false-positive bias.
    if depth < MAX_SHELL_DEPTH {
        // `eval`/`exec`/`source`/`.` run their remaining words as a command.
        // ASK-1: this is a shell-EVALUATION position, so `eval "$CMD"` asks
        // rather than falling through unanalysed.
        if matches!(cmd, "eval" | "exec" | "source" | ".") && !rest.is_empty() {
            let joined = rest.join(" ");
            let inline = strip_wrapping_quotes(&joined);
            if let Some(deny) = acc.record(analyze_shell_payload(inline, depth, ctx)) {
                return deny;
            }
            // Backstop for backslash-over-escaped payloads (99b506b7). See
            // `denoised_eval_rescan`.
            if let Some(deny) = acc.record(denoised_eval_rescan(&joined, depth, ctx)) {
                return deny;
            }
        }
        // `sh -c "<payload>"` and friends evaluate the `-c` argument.
        // ASK-1: likewise a shell-evaluation position — `bash -c "$CMD"` asks.
        if is_shell(cmd) {
            for payload in dash_c_payloads(rest) {
                if let Some(deny) = acc.record(analyze_shell_payload(&payload, depth, ctx)) {
                    return deny;
                }
            }
            // daf7611b: a shell whose language this module cannot parse. The
            // POSIX pass above may still Deny; if it did not, that is a
            // failure to analyse, not a clean result (CLAUDE.md §3).
            if is_foreign_shell(cmd) && foreign_shell_runs_code(rest) {
                if let Some(deny) = acc.record(Decision::ask(format!(
                    "`{cmd}` runs code in a non-POSIX shell language that blastguard cannot \
analyse — it cannot tell what this would do, so it refuses to guess"
                ))) {
                    return deny;
                }
            }
            // Backstop for backslash-over-escaped payloads that defeat the
            // structured extraction above (99b506b7). See `denoised_eval_rescan`.
            if let Some(deny) = acc.record(denoised_eval_rescan(&rest.join(" "), depth, ctx)) {
                return deny;
            }
        }
    } else {
        // Cap reached: the recursion above did NOT run, so this segment was not
        // fully analysed. Record the unfinished-analysis Ask into `acc` rather
        // than returning early, so a Deny found by a later non-recursive rule in
        // this same function still outranks it (see `VerdictAcc::record`).
        let _ = acc.record(depth_exhausted());
    }

    if let Some(deny) = acc.record(system_paths::system_mutation_verdict(cmd, rest, ctx)) {
        return deny;
    }
    let verdict = match cmd {
        // BG-3 (second instance): `split_segments` splits on `;`, so the `\;`
        // terminator of a `find -exec` CUTS THE LINE before `analyze_find` ever
        // sees it. `find . -name x -exec grep rm {} \; -exec rm -rf {} \;`
        // reaches this function as three segments, and the second one has
        // `-exec` as its command word — matching no rule, so the destructive
        // `-exec rm` was ALLOW while the `+`-terminated twin was correctly
        // denied. Recognising a segment whose command word is an exec predicate
        // and routing it back through `analyze_find` restores the multi-exec
        // scan for the `\;` form without depending on where the split fell.
        "-exec" | "-execdir" | "-ok" | "-okdir" => analyze_find(&tokens[idx..], depth, ctx),
        "rm" => analyze_rm(rest, ctx),
        "gh" => analyze_gh(rest),
        "git" => {
            // daf7611b: `git -c core.pager="rm -rf ~/x" log` — command-valued
            // config set for this one command. Either side's Deny wins.
            let mut git_acc = VerdictAcc::default();
            if let Some(deny) = git_acc.record(executors::git_exec_config_verdict(rest, depth, ctx))
            {
                return deny;
            }
            // fb338793: an exec-capable git shape is banned by `analyze_git`
            // whatever it runs. When the command it would run is spelled out
            // and is itself refused (`git bisect run rm -rf /usr`), that
            // refusal is reported instead, so its own reason — and its
            // system-harm marker, which keeps a repeat a Deny — survives.
            if let Some(deny) = git::git_exec_ban_embedded_deny(rest, depth, ctx) {
                return deny;
            }
            match git_acc.record(analyze_git(rest, seg, ctx)) {
                Some(deny) => deny,
                None => git_acc.finish(),
            }
        }
        // daf7611b: awk's `system()` and command pipes run shell lines.
        awk if executors::is_awk(awk) => executors::analyze_awk(awk, rest, depth, ctx),
        // Round 2 (adversarial verifier): every rule in this dispatch was about
        // DELETING or TRUNCATING, so the ordinary ways of REPLACING a file's
        // contents — copying/moving/linking something over it, or rewriting it
        // in place — had no arm at all and fell through to `_ => Allow`. That
        // left `cp evil.json .claude/settings.json` as a one-command bypass of
        // the whole protected-path rule. Only the DESTINATION matters here:
        // reading a protected file is harmless, writing one is the hazard.
        "cp" | "mv" | "install" | "ln" => analyze_copy_move(cmd, rest, ctx),
        // Round 2: the single-file and empty-directory twins of `rm`, which had
        // no arm at all. See `analyze_unlink_rmdir`.
        "unlink" | "rmdir" => analyze_unlink_rmdir(cmd, rest, ctx),
        // Egress: exfiltration via curl/wget upload flags and nc/netcat fed a
        // file on stdin. See `analyze_fetch` / `analyze_nc`. Remote-exec
        // (fetch piped into a shell/interpreter) is handled separately, across
        // segments, by `analyze_pipe_egress` in `detect_bash`.
        "curl" | "wget" => analyze_fetch(cmd, rest),
        "nc" | "ncat" | "netcat" => analyze_nc(rest),
        // Round 4: `sort`/`uniq` were miscategorised as read-only. Both have
        // output-FILE forms that truncate the named file, so they get dedicated
        // arms that Deny the write-onto-a-protected-path shape (pure reads stay
        // Allow). See `analyze_sort` / `analyze_uniq`.
        "sort" => analyze_sort(rest, ctx),
        "uniq" => analyze_uniq(rest, ctx),
        "sed" => analyze_sed(rest, ctx),
        "find" => analyze_find(rest, depth, ctx),
        "xargs" => analyze_xargs(rest, depth, ctx),
        // Both of these were location-blind: `truncate -s 0 target/log.txt`
        // and `truncate -s 0 /usr/lib/libc.so` were the same verdict. The
        // target list is computed the same way every other rule computes one
        // (`positional_operands`, so a `-s SIZE` value is never mistaken for a
        // file), and a protected target cannot be relaxed — see `Ctx::confined`.
        "truncate" => shape_deny_or_confined_ask(
            ctx,
            "truncate",
            "truncate",
            &positional_operands(
                rest,
                &["-s", "--size", "-r", "--reference", "-o", "--io-blocks"],
            ),
            "truncate can shrink a file to zero bytes",
        ),
        "shred" => shape_deny_or_confined_ask(
            ctx,
            "shred",
            "shred",
            &positional_operands(
                rest,
                &["-n", "--iterations", "-s", "--size", "--random-source"],
            ),
            "shred destroys file contents irreversibly",
        ),
        "dd" => {
            if rest.iter().any(|t| t.starts_with("of=")) {
                Decision::deny("dd with of= writes raw bytes over a device/file")
            } else {
                Decision::Allow
            }
        }
        "chmod" => {
            if has_short(rest, 'R') || rest.contains(&"--recursive") {
                // `chmod -R 755 target` and `chmod -R 000 /usr` were one
                // verdict. A recursive chmod does not delete the tree, so a
                // safe root ITSELF is acceptable here (unlike `rm`).
                shape_deny_or_confined_ask(
                    ctx,
                    "chmod",
                    "recursive chmod (-R)",
                    &chmod_mode_and_targets(rest).1,
                    "recursive chmod re-permissions a whole tree",
                )
                .or_root_itself(
                    ctx,
                    "chmod",
                    "recursive chmod (-R)",
                    &chmod_mode_and_targets(rest).1,
                )
            } else {
                // Round 3, shape 3 of the disarm-by-non-write class. Like the
                // `rm` arm, this rule measured BLAST RADIUS (`-R`) and never
                // asked what the target was. A git hook — or a
                // `.claude/hooks/*` script — that exists but is not executable
                // is silently SKIPPED by its loader: the file is intact byte
                // for byte and the gate is off, so no write-shaped rule can
                // see it.
                let (mode, targets) = chmod_mode_and_targets(rest);
                // A chmod whose mode could not be located is not a harmless
                // chmod; treat it as disarming (restrictive).
                //
                // Round 4: EXEC is not the only bit a hook needs. A shell script
                // hook is run by its shebang, and the kernel/interpreter must
                // READ the file to execute it — proven on this host:
                // `chmod -r hook.sh` (mode -wx--x--x) makes `./hook.sh` fail
                // "Permission denied" (exit 126) despite the exec bit. So a mode
                // that removes EITHER read or exec from the relevant class
                // disarms the hook, and both gate the protected-target deny.
                if mode
                    .map(|m| mode_removes_exec(m) || mode_removes_read(m))
                    .unwrap_or(true)
                {
                    // Round 2: the wildcard case. `chmod 000 .claude/*` was
                    // ALLOW because `protected_disarm_deny` was handed a token
                    // containing `*`, which cannot match a literal path glob.
                    // See `protected_glob_deny`. The DIRECTORY case is checked
                    // too: a non-recursive `chmod 000 .claude` clears the
                    // traverse bit and makes every protected file under it
                    // unreachable, which is a disarm without touching a byte of
                    // any of them.
                    targets
                        .into_iter()
                        .find_map(|t| {
                            protected_disarm_deny(ctx, "chmod", t)
                                .or_else(|| protected_tree_deny(ctx, "chmod", t))
                                .or_else(|| protected_glob_deny("chmod target", t))
                        })
                        .unwrap_or(Decision::Allow)
                } else {
                    Decision::Allow
                }
            }
        }
        verb @ ("chown" | "chgrp") => {
            if has_short(rest, 'R') || rest.contains(&"--recursive") {
                // The first positional operand of `chown` is the OWNER spec
                // (`yuki`, `yuki:staff`), not a path — dropping it is what
                // keeps `chown -R yuki target` from being judged as a chmod of
                // a file called `yuki`. An operand list that is only the owner
                // (no path at all) leaves nothing to place, and
                // `Ctx::confined` answers `None` for an empty list.
                let operands = positional_operands(rest, &[]);
                let targets = operands.get(1..).unwrap_or(&[]);
                shape_deny_or_confined_ask(
                    ctx,
                    "chown",
                    "recursive chown (-R)",
                    targets,
                    "recursive chown re-owns a whole tree",
                )
                .or_root_itself(ctx, "chown", "recursive chown (-R)", targets)
            } else {
                // Round 5, the chmod MIRROR. This rule measured BLAST RADIUS
                // (`-R`) and never asked what the target was, so every
                // non-recursive form was `Allow` — including
                // `chown nobody .githooks/pre-commit`, one verb away from the
                // chmod arm that has classified its targets since round 3.
                // `chgrp` was not matched at all.
                //
                // The verdict is `Ask` rather than `Deny`; see
                // `protected_reown_ask` for why that is the honest one.
                let (_owner, targets) = chown_owner_and_targets(rest);
                targets
                    .into_iter()
                    .find_map(|t| protected_reown_ask(ctx, verb, t))
                    .unwrap_or(Decision::Allow)
            }
        }
        // CA-blastguard-009: `tee FILE` (no -a/--append) truncates/overwrites
        // FILE identically to a `>` redirect. Append-mode tee is safe and
        // stays Allow.
        "tee" => {
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
            let all_absent = !targets.is_empty()
                && targets.iter().all(|t| {
                    matches!(
                        crate::reversible::probe(t, ctx.raw_base.as_deref()),
                        crate::reversible::Recovery::NothingToDestroy
                    )
                });
            if all_absent {
                return Decision::Allow;
            }
            Decision::deny("tee without -a/--append truncates and overwrites its target file(s)")
        }
        // CA-blastguard-006: a bare top-level command-interpreter invocation
        // (`python3 -c "…"`, no `find` wrapper) can run an arbitrary
        // destructive command, just like the same payload wrapped in
        // `find -exec`/`-ok` (handled in analyze_find). Judge it here too, not
        // only inside the find-exec path — and judge the PROGRAM, not the shape
        // of the invocation. See `analyze_code_interpreter`.
        cmd if is_code_interpreter(cmd) => analyze_code_interpreter(cmd, rest, depth, ctx),
        other => {
            if other.starts_with("mkfs") {
                Decision::deny("mkfs formats a filesystem, destroying all data")
            } else if let Some(reason) = irreversible_device_verb(other, rest) {
                Decision::deny(reason)
            } else {
                // Round 2: this arm WAS `Decision::Allow`, i.e. every verb
                // without a rule reached every protected path unclassified.
                // See `unknown_verb_protected_ask`.
                unknown_verb_protected_ask(other, rest)
            }
        }
    };
    // A Deny from the rule arms wins outright; otherwise an Ask banked by the
    // shell-evaluation arms above is reported instead of being dropped.
    match acc.record(verdict) {
        Some(deny) => deny,
        None => acc.finish(),
    }
}

/// A backtick inside a DOUBLE-quoted argument of `backlog add|edit` or
/// `git commit` is command substitution: the shell runs the span and replaces
/// it with its (usually empty) output, so the stored note / commit message
/// silently loses that text. Single-quoted backticks are literal and fine.
fn backtick_in_double_quoted_text(seg: &str) -> Option<Decision> {
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod heredoc_subst_tests;
