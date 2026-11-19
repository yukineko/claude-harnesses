//! EFFECT x TARGET-PATH judgement over an analysed [`Script`] (task S1
//! `subset-spec`, user rulings 2026-11-17).
//!
//! The specification is `SUBSET.md` next to this file: Table A (the
//! supported node / expansion kinds — everything else is
//! `Cause::Unsupported`), Table B (which effect each modelled command has on
//! which target words), Table C ([`PathClass`] x [`Effect`] -> verdict). This
//! file freezes the types and the signatures.
//!
//! # Current state (S3b, user ruling 2026-11-18)
//!
//! [`judge`] is wired into `detect` (through [`super::table_b_verdict`],
//! called from `detect/readings.rs::detect_bash_every_reading`) ONLY for the
//! commands that have a Table B **B.1** row (`table_b.rs`):
//!
//! * an option not in the row's list, an undetermined word that may be an
//!   option, or a `dd` operand that is not `key=value` →
//!   [`Judgement::Undetermined`] (`Cause::Unsupported`);
//! * a target word whose value the evaluator cannot determine (unquoted
//!   glob, `~user`, `$( )`, an unbound / positional parameter, a relative
//!   path after an undetermined `cd`, an `xargs` operand read from stdin) →
//!   a [`PathClass::Unresolved`] finding → [`Judgement::Ask`]. Except (user
//!   ruling 2026-11-19): an unquoted glob whose metacharacters are only in
//!   its final component, under a literal directory prefix with no `..`
//!   (`table_b::literal_dir_glob`; a bare glob's parent is the cwd), is
//!   placed by that parent directory like a literal target there;
//! * a target whose value IS determined gets no finding: [`classify`] is
//!   still the S1 stub, and the legacy string layer (raise-only, ruling 14)
//!   alone judges placed paths. So [`Judgement::Allow`] here means "S3b found
//!   nothing undetermined", not "every Table C cell is Allow";
//! * a relative target whose cwd is one of a finite set (`cd x; cmd rel`;
//!   [`super::SimpleCommand::cwd_candidates`], user ruling 2026-11-18) is
//!   judged once per candidate cwd by the whole detector
//!   ([`PathCtx::legacy`], `judge_candidates`); an Ask / Deny at any
//!   candidate is a [`PathClass::AtCandidate`] finding (a Deny there makes
//!   the judgement [`Judgement::Deny`]), and a candidate that cannot be
//!   judged is `Unresolved`;
//! * a command without a B.1 row — unknown program, empty `argv`,
//!   undetermined command word, B.2 redirects, B.3 wrappers, B.4 git, B.5
//!   interpreters — contributes nothing: its handling is unchanged (the
//!   ruling), so the "undetermined command word → Undetermined" contract
//!   below is NOT yet enforced by this module.
//!
//! An `analyze` result of `Undetermined` is not judged here (analyze-wide
//! Undetermined handling unchanged). S6 removes the string readings (see
//! "Stage map" in `SUBSET.md`).
//!
//! # Contract (holds from the first caller on)
//!
//! * [`Judgement::Undetermined`] means "cannot judge" and is resolved by the
//!   caller to Ask (main session) / Deny (subagent, via
//!   [`crate::interactive::ask_available`]); it is never folded into Allow.
//!   On that path the legacy string harm detection may only RAISE the verdict
//!   (Ask -> Deny), never lower it ("Undetermined handling" in `SUBSET.md`).
//! * A target that does not resolve to a literal path is
//!   [`PathClass::Unresolved`], which Table C answers restrictively.
//! * A command with no effect model in Table B (an unknown program) has no
//!   finding: it contributes Allow. Its redirects are still judged (they are
//!   the shell's effect, not the program's). An undetermined command word is
//!   [`Judgement::Undetermined`], not an unknown program.
//! * [`PathClass`] is computed ONLY from predicates that already exist (listed
//!   on each variant); this module adds no path predicate.

use std::ops::Range;

use super::{Cause, Script, Value};
use crate::scope::SafeRoots;

pub(super) mod deny_targets;
mod table_b;

/// What a modelled command (Table B) does to one target path.
#[allow(
    dead_code,
    reason = "frozen in S1; constructed by S3 (path-cutover), which wires `judge` into detect"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    /// The entry is removed (`rm`, `rmdir`, `unlink`, `find -delete`, a `mv`
    /// source, `git clean`). `recursive` = a whole tree may go (`rm -r`,
    /// `find -delete`, `git clean -d`, a `mv` source that may be a directory).
    Delete { recursive: bool },
    /// The content is replaced from empty: `>` / `>|` / `&>`, `tee`,
    /// `truncate`, `dd of=` (without `conv=notrunc`), the `cp` / `install` /
    /// `mv` destination, `find -fprint*` files.
    Truncate,
    /// Bytes are added at the end: `>>` / `&>>`, `tee -a`.
    Append,
    /// The content is edited in place: `sed -i`, `<>`, `dd conv=notrunc`.
    Rewrite,
    /// Mode / owner / group changes: `chmod`, `chown`, `chgrp`.
    Metadata,
    /// A link name is created or replaced: `ln` (the link, not its target).
    Link,
    /// A new directory tree is created: `cp -r` destination (the existing
    /// scratch-place rule, `detect/scratch_place.rs`).
    CreateTree,
    /// Uncommitted work-tree content may be discarded: `git reset --hard`,
    /// `git checkout -- <paths>` / `git checkout .`, `git restore` (work tree).
    GitDiscard,
    /// Every change under the target is staged: `git add -A` / `--all` / `.`.
    GitStage,
}

/// Where one target path lands. Table C is indexed by this x [`Effect`].
///
/// Precedence when several apply (first wins): `System`, `Protected`,
/// `Workflow`, `Persistence`, `Scratch`, `OwnWorktree`, `OtherWorktree`,
/// `Project`, `Other`.
/// `Unresolved` is exclusive (no predicate could be consulted).
#[allow(
    dead_code,
    reason = "frozen in S1; constructed by S3 (path-cutover) in `classify`"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathClass {
    /// System harm: `harms_system` in `detect/harm_mark.rs` — `/`,
    /// [`crate::scope::is_inside_system_dir`], a raw disk device
    /// (`is_raw_disk`), the home directory itself,
    /// [`SafeRoots::irreplaceable_home_asset`] — on the lexical and the
    /// symlink-resolved spelling; plus `real_system_path_block`'s landing
    /// (`detect/protected.rs`). A system path strictly inside a safe root
    /// (`Ctx::confined_root`, a project under `/opt`) is not `System`.
    System,
    /// A gate / hook / policy path: [`crate::exclude::is_protected_path`]
    /// (and `exclude::touches_protected` where the verb is not modelled),
    /// lexical and through `protected::physical_landing`. Lifted (falls
    /// through to the next class) strictly inside a linked worktree checkout:
    /// [`SafeRoots::classify_worktree_checkout`] `Inside`.
    Protected,
    /// A GitHub Actions workflow file: [`crate::exclude::is_workflow_path`]
    /// (CLAUDE.md §7).
    Workflow,
    /// A NEW file in a per-user `Library/LaunchAgents`:
    /// `launch_agents_new_file_ask` (`detect/protected.rs`).
    Persistence,
    /// Every temp and cache deletion root (`/tmp`, `/private/tmp`, `$TMPDIR`,
    /// `$HOME/.cache`, `$HOME/Library/Caches`; the session scratchpad lives
    /// under `/private/tmp`): [`SafeRoots::classify_disposable`] `Inside`
    /// (strictly inside; `IsRoot` is not `Scratch`). User ruling 2026-11-17.
    Scratch,
    /// Inside the git checkout that contains the session cwd (user ruling
    /// 2026-11-17): the target is [`SafeRoots::classify_worktree_checkout`]
    /// `Inside { root: R }` and [`SafeRoots::session_cwd`] is `R` or inside
    /// `R` by the same predicate. Any `Undetermined` is not `OwnWorktree`.
    OwnWorktree,
    /// Strictly inside a worktree storage root (`$HOME/.condukt/worktrees`,
    /// `.harness-worktrees`; [`SafeRoots::classify_worktree`] `Inside`) but
    /// not `OwnWorktree`: today's verdict is kept (legacy; a recursive `rm`
    /// strictly inside a worktree storage root is Allow, ruling 9 as corrected
    /// 2026-11-18).
    OtherWorktree,
    /// Inside a session safe root (payload cwd / `CLAUDE_PROJECT_DIR`):
    /// [`SafeRoots::classify`] `Inside` / `IsRoot`; judged by the existing
    /// recoverability rules (`crate::deletion::judge_rm`, `crate::reversible`).
    Project,
    /// Resolved and none of the above (`Placement::Outside`).
    Other,
    /// The target could not be placed: an `Undetermined` value, a relative
    /// path with no known base, a `..` component, a predicate answering
    /// `Undetermined`. Always restrictive in Table C.
    Unresolved(String),
    /// A relative target placed against ONE candidate cwd of a command whose
    /// cwd is one of a finite set (user ruling 2026-11-18) and judged there by
    /// the whole detector, which answered Ask / Deny (the reason says which
    /// candidate and why). Until `classify` lands the detector's predicates
    /// are the Table C cells for a placed path.
    AtCandidate(String),
}

/// One judged (effect, target) pair.
#[allow(
    dead_code,
    reason = "frozen in S1; constructed by S3 (path-cutover) in `judge`"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub effect: Effect,
    /// The target as evaluated, relative paths represented as in
    /// [`super::RedirTarget::Path`].
    pub target: Value,
    pub class: PathClass,
    /// Byte span of the simple command (or redirect) in the analysed text.
    pub span: Range<usize>,
    pub reason: String,
}

/// The verdict of [`judge`]: the MAIN-session column of Table C. The
/// subagent column is the same with every Ask hardened to Deny by
/// [`crate::interactive::ask_available`]; no cell needs another rule.
#[allow(
    dead_code,
    reason = "every variant is produced since the 2026-11-18 per-candidate cwd ruling; kept for the S1 freeze"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub(crate) enum Judgement {
    /// Every finding's cell is Allow, or no command has an effect model. In
    /// S3b: no B.1 command had an undetermined option or target (placed
    /// targets are not classified yet; see the module doc).
    Allow,
    /// At least one Ask cell and no Deny cell.
    Ask(Vec<Finding>),
    /// At least one Deny cell. `system_harm` when one of them is
    /// [`PathClass::System`] (emitted through `Decision::deny_system_harm`).
    Deny {
        findings: Vec<Finding>,
        system_harm: bool,
    },
    /// Cannot judge: outside Table A (`Cause::Unsupported`), an undetermined
    /// command word, a modelled command with an unmodelled option, a parse
    /// stage cause, or (S1 only) `Cause::NotImplemented`. Never Allow.
    Undetermined(Cause),
}

/// What [`classify`] places paths against: the session's location model and
/// the base for a relative literal when no `cd` applies (the hook's cwd,
/// `Ctx::raw_base` at the start of a line).
#[allow(
    dead_code,
    reason = "frozen in S1; built by S3 (path-cutover) in detect"
)]
pub(crate) struct PathCtx<'a> {
    pub scope: &'a SafeRoots,
    pub hook_cwd: Option<&'a str>,
    /// The verdict of the whole detector for a command text (the legacy
    /// string layer and this judge, raise-only), used to judge one candidate
    /// cwd of a [`SimpleCommand::cwd_candidates`] command. `None`: such a
    /// command's relative targets stay `Unresolved`.
    pub legacy: Option<&'a dyn Fn(&str) -> crate::model::Decision>,
}

/// Candidate-cwd judgements ([`judge_candidates`]) one [`judge`] may run
/// before the rest are `Unresolved`.
const MAX_CANDIDATE_RUNS: usize = 32;

/// Judge `script` by Table B B.1 and the Table C `Unresolved` row (S3b; see
/// the module doc for what is and is not judged yet).
///
/// The first command whose B.1 row is undetermined makes the whole script
/// [`Judgement::Undetermined`]; otherwise every unresolved target is an
/// [`Judgement::Ask`] finding; otherwise [`Judgement::Allow`].
pub(crate) fn judge(script: &Script, ctx: &PathCtx) -> Judgement {
    let mut findings = Vec::new();
    let mut denied = Vec::new();
    let mut system_harm = false;
    let mut runs = 0;
    for c in &script.commands {
        let targets = match table_b::extract(c) {
            table_b::Row::None => continue,
            table_b::Row::Undetermined(why) => {
                return Judgement::Undetermined(Cause::Unsupported(why))
            }
            table_b::Row::Targets(t) => t,
        };
        // User ruling 2026-11-18: a relative target after a `cd` that may
        // have failed, when the cwd is one of a known finite set, is judged
        // once per candidate cwd; the most restrictive verdict stands.
        // Only a `OneOf` cwd is split per candidate. A single cwd and an
        // `Undetermined` one take the `resolve_at` path below, where a
        // relative target after an undetermined cwd is an `Unresolved`
        // finding (Ask / Deny).
        let (relative, targets): (Vec<_>, Vec<_>) = targets.into_iter().partition(|(_, t)| {
            matches!(c.cwd_candidates, super::CwdCandidates::OneOf(_))
                && matches!(t, Value::Literal(p) if !p.starts_with('/'))
        });
        if let (super::CwdCandidates::OneOf(set), false) = (&c.cwd_candidates, relative.is_empty())
        {
            for f in judge_candidates(c, set, &relative, ctx, &mut runs) {
                match f {
                    CandidateFinding::Ask(f) => findings.push(f),
                    CandidateFinding::Deny(f, harm) => {
                        system_harm |= harm;
                        denied.push(f);
                    }
                }
            }
        }
        for (effect, target) in targets {
            let resolved = super::eval::resolve_at(&c.cwd, target);
            if let Value::Undetermined(why) = &resolved {
                findings.push(Finding {
                    effect,
                    class: PathClass::Unresolved(why.clone()),
                    reason: format!("{effect:?} target cannot be placed: {why}"),
                    target: resolved.clone(),
                    span: c.span.clone(),
                });
            }
        }
    }
    if !denied.is_empty() {
        denied.extend(findings);
        Judgement::Deny {
            findings: denied,
            system_harm,
        }
    } else if findings.is_empty() {
        Judgement::Allow
    } else {
        Judgement::Ask(findings)
    }
}

/// A non-Allow verdict for one candidate cwd ([`judge_candidates`]).
enum CandidateFinding {
    Ask(Finding),
    /// The finding and whether it is a system-harm Deny.
    Deny(Finding, bool),
}

/// The relative `targets` of `c`, whose cwd is one of
/// [`SimpleCommand::cwd_candidates`] (user ruling 2026-11-18): the command is
/// judged once per candidate, as if it ran there — `<argv>` for the hook's
/// cwd, `cd <dir> && <argv>` for a known directory — by the whole detector
/// (`ctx.legacy`, which places the operands against that base by the same
/// predicates as a literal spelling). A candidate is Allow only when that
/// verdict is Allow; an Ask / Deny is a finding (most restrictive wins in
/// [`judge`]). When a candidate cannot be judged — no `ctx.legacy`, the
/// hook's cwd unknown, an argv word that is not a literal the synthetic text
/// can spell, or more than [`MAX_CANDIDATE_RUNS`] runs — every target is
/// `Unresolved` (Ask / Deny), never Allow.
fn judge_candidates(
    c: &super::SimpleCommand,
    cwds: &[Option<String>],
    targets: &[(Effect, Value)],
    ctx: &PathCtx,
    runs: &mut usize,
) -> Vec<CandidateFinding> {
    let names = || {
        targets
            .iter()
            .map(|(_, t)| match t {
                Value::Literal(p) => format!("`{p}`"),
                Value::Undetermined(r) => r.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let effect = targets[0].0;
    let finding = |class: PathClass, reason: String| Finding {
        effect,
        target: targets[0].1.clone(),
        class,
        span: c.span.clone(),
        reason,
    };
    let unresolved = |why: &str| {
        let reason = format!(
            "{effect:?} target cannot be placed: relative path {} after a cd that may have \
             failed, and {why}",
            names()
        );
        vec![CandidateFinding::Ask(finding(
            PathClass::Unresolved(reason.clone()),
            reason,
        ))]
    };
    let Some(legacy) = ctx.legacy else {
        return unresolved("no per-directory judgement is available");
    };
    let Some(argv) = spell_argv(c) else {
        return unresolved("the command cannot be re-spelled for a per-directory judgement");
    };
    if *runs + cwds.len() > MAX_CANDIDATE_RUNS {
        return unresolved("the per-directory judgement budget is spent");
    }
    let mut out = Vec::new();
    for cand in cwds {
        let (text, label) = match cand {
            None => match ctx.hook_cwd {
                Some(h) => (argv.clone(), format!("the hook's cwd `{h}`")),
                None => return unresolved("the hook's cwd is not known"),
            },
            Some(d) => match spell(d) {
                Some(q) => (format!("cd {q} && {argv}"), format!("`{d}`")),
                None => return unresolved("a candidate directory cannot be spelled"),
            },
        };
        *runs += 1;
        let verdict = legacy(&text);
        let say = |r: &str| {
            format!(
                "{effect:?} target {} in {label} (the cd before it may have failed): {r}",
                names()
            )
        };
        match verdict {
            crate::model::Decision::Allow => {}
            crate::model::Decision::Ask(ref r) => {
                let reason = say(r);
                out.push(CandidateFinding::Ask(finding(
                    PathClass::AtCandidate(reason.clone()),
                    reason,
                )));
            }
            crate::model::Decision::Deny(ref r) => {
                let reason = say(r);
                out.push(CandidateFinding::Deny(
                    finding(PathClass::AtCandidate(reason.clone()), reason),
                    verdict.is_system_harm(),
                ));
            }
        }
    }
    out
}

/// `c`'s argv as shell text that runs the same words, or `None` when a word
/// is not a `Literal` the text can spell (a control character), or the
/// command word would need quoting. A literal-dir glob argument
/// ([`table_b::literal_dir_glob`], user ruling 2026-11-19) is spelled as
/// written, unquoted, so the per-candidate text expands it in that directory.
fn spell_argv(c: &super::SimpleCommand) -> Option<String> {
    let mut words = Vec::with_capacity(c.argv.len());
    for (i, w) in c.argv.iter().enumerate() {
        if i > 0 {
            if let Some(glob) = table_b::literal_dir_glob(w) {
                words.push(glob);
                continue;
            }
        }
        let Value::Literal(v) = &w.value else {
            return None;
        };
        let s = spell(v)?;
        if i == 0 && (s != *v || v.contains('=')) {
            return None;
        }
        words.push(s);
    }
    (!words.is_empty()).then(|| words.join(" "))
}

/// `v` as one shell word: bare when every character means itself, else
/// single-quoted. `None` for a control character.
fn spell(v: &str) -> Option<String> {
    if v.chars().any(char::is_control) {
        return None;
    }
    let plain = !v.is_empty()
        && v.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '/' | '.' | '_' | '+' | ',' | ':' | '@' | '%' | '^' | '=' | '-'
                )
        });
    Some(if plain {
        v.to_string()
    } else {
        format!("'{}'", v.replace('\'', r"'\''"))
    })
}

/// The [`PathClass`] of `target` under `effect` (some classes depend on the
/// effect: `Persistence` is for new files only, `IsRoot` is acceptable only
/// for a search root). `cwd` is the command's [`super::SimpleCommand::cwd`].
///
/// S1 stub: always [`PathClass::Unresolved`] (the restrictive answer).
#[allow(
    dead_code,
    reason = "frozen in S1; implemented by S3 (path-cutover) from the predicates named on `PathClass`"
)]
pub(crate) fn classify(effect: Effect, target: &Value, cwd: &Value, ctx: &PathCtx) -> PathClass {
    let _ = (effect, target, cwd, ctx);
    PathClass::Unresolved("PathClass classification is implemented in stage S3".into())
}
