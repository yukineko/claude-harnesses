//! harness-core::verdict — the one three-valued gate verdict, built so that a
//! fail-open is a **compile error** rather than a code-review finding.
//!
//! # Why this exists
//!
//! The three-valued gate answer — "checked, clean" / "checked, violation" /
//! "could not check" — was reinvented at least four times across the gate
//! crates: `blastguard::Decision`, `propguard::CheckOutcome`,
//! `reviewgate::ReviewerResult`, and the `ChangeScan` copied into
//! donegate/propguard/reviewgate/tdd. Every reinvention re-litigates the same
//! hazard, and several lost: the recurring defect in this repo is that the
//! *third* answer ("could not check") silently collapses into the *first*
//! ("clean / allow") — a `.unwrap_or(false)`, a `Default`, an empty set read as
//! "nothing wrong", a `From<bool>` that cannot represent "unknown". That
//! collapse is a fail-open, and the whole point of the gates is to not have one.
//!
//! `git_probe::RepoProbe` already encodes this discipline for one specific
//! question ("is this a repo?"). This module lifts it to the general gate
//! verdict and makes the fail-open **unrepresentable** rather than merely
//! discouraged:
//!
//! 1. **[`Clean`] cannot be forged.** It carries an [`Evidence`] witness whose
//!    only field is a private `()`, so no crate outside `harness-core` can write
//!    `Verdict::Clean(..)`. The single public path to a `Clean` is
//!    [`Verdict::from_findings`] / [`Verdict::adjudicate`], each of which forces
//!    the caller to hand over the findings a check *actually collected*. You
//!    cannot mint a pass without expressing that a check ran.
//! 2. **No free `Clean`.** [`Verdict`] implements neither `Default` nor
//!    `From<bool>` nor `Into<bool>`, so a pass can never appear from
//!    `Default::default()`, `.into()`, or `?`-elision. It is `#[must_use]`, so a
//!    verdict cannot be computed and dropped unread.
//! 3. **No `#[non_exhaustive]` escape.** Adding a variant here is *meant* to
//!    break every downstream `match` at compile time — that error is the feature
//!    (every gate is forced to say what the new answer means), so the attribute
//!    that would suppress it is deliberately absent.
//! 4. **[`Determination<T>`] has exactly one extractor: [`Determination::require`]**
//!    returning [`Required<T>`] — deliberately **not** `std::Result`. Neither
//!    type has `unwrap_or`, `ok()`, `unwrap_or_default`, `unwrap_or_else`, or
//!    `is_ok`, so "could not determine" cannot be swapped for a permissive
//!    default by any *method call*; the caller has to `match` both arms, and the
//!    undetermined arm hands over an already-fail-closed [`Verdict`] carrying its
//!    reason. `Result` was the original return type and it leaked the seal one
//!    call deeper — `.require().unwrap_or_default()` reopened the exact collapse
//!    `Determination` refuses to grow, using `std`'s inherent methods, which this
//!    crate cannot remove (pinned red-then-green by
//!    `tests/ui/verdict/require_result_erasure.rs`).
//!
//!    **What this does not seal**, stated plainly so no reader mistakes the
//!    scope: a hand-written `match d.require() { Determined(v) => v, Blocked(_)
//!    => Vec::new() }` still substitutes a permissive default, and no type can
//!    forbid it — the caller wrote that default themselves. The type's job is to
//!    make the collapse unreachable *by accident* and to force the deliberate
//!    one to appear in a diff as an explicit arm; catching that residue is a
//!    separate, lexical gate's job (backlog b4baf3d7). There is also no `?`
//!    support: `std::ops::Try` is unstable (E0658, rust#84277), and `.require()?`
//!    appears nowhere in this repo, so nothing is lost.
//! 5. **Every channel conversion sends `Undetermined` to the restricted side,**
//!    and no conversion mapping it to the permissive side exists. The blocking
//!    channel differs per gate (a Stop hook blocks via a JSON `decision` field
//!    while exiting 0 toward Claude; a PreToolUse/precommit gate blocks via a
//!    non-zero exit code), so there is deliberately no single
//!    `From<Verdict> for ExitCode` that would hard-code one gate's convention
//!    onto all of them. Instead [`Verdict::stop_decision`] and
//!    [`Verdict::exit_code`] each map `Undetermined` exactly like `Violation`.
//! 6. **`Undetermined` cannot be minted un-counted.** The same private-field
//!    trick as (1), aimed the other way: the payload is [`Undet`], not a bare
//!    [`Reason`], so `Undetermined(..)` is unwriteable outside this crate and the
//!    only way in is [`Verdict::undetermined`] / [`Determination::undetermined`],
//!    which record one caller-attributed telemetry event per give-up (see
//!    [`crate::undetermined`]). This exists because "cannot determine" resolving
//!    to the restricted side is only half the discipline — a fleet that blocks
//!    correctly but cannot say *how often* it is blocking on ignorance has no way
//!    to tell a hardening gate from a broken one. Forwarding an existing
//!    `Undetermined` upward stays free and deliberately does not re-record: the
//!    origin already counted it once.

use std::fmt;

/// A human-readable reason attached to a non-clean verdict. A `Violation` and an
/// `Undetermined` must each carry *why*, so the operator (or the next gate) is
/// never handed a bare "blocked" with no cause. There is intentionally no
/// `Default` and no empty constructor: a reason is always a real string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason(String);

impl Reason {
    /// Build a reason from any string-like message.
    #[must_use]
    pub fn new(msg: impl Into<String>) -> Self {
        Reason(msg.into())
    }

    /// The underlying message.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Fold several findings into one reason (used when many findings collapse
    /// into a single `Violation`). Joins with `; `. The caller only reaches this
    /// with a non-empty list — an empty findings list is a `Clean`, not a
    /// `Violation` — but if handed an empty slice it yields a generic,
    /// non-silent message rather than an empty string.
    #[must_use]
    pub fn joined(findings: &[Reason]) -> Self {
        if findings.is_empty() {
            return Reason("violation with no stated finding".to_string());
        }
        Reason(
            findings
                .iter()
                .map(Reason::as_str)
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unforgeable proof that a check ran and had nothing to report.
///
/// The single field is a **private** `()`, so `Evidence` can be constructed only
/// inside `harness-core`. That is the whole mechanism behind
/// "[`Verdict::Clean`] cannot be forged": an external crate cannot write
/// `Evidence(())` (the field is private) and therefore cannot write
/// `Verdict::Clean(..)` either. It can *name* the type (it appears in the public
/// enum) and *match* on `Clean(_)`, but it cannot mint one. The only public way
/// to obtain a `Clean` is through [`Verdict::from_findings`] /
/// [`Verdict::adjudicate`], which mint the `Evidence` internally *after* the
/// caller has expressed that a check produced zero findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence(());

/// The payload of an `Undetermined`, and the same private-field trick as
/// [`Evidence`] pointed at the opposite end: an `Undetermined` cannot be
/// *minted* outside `harness-core` either.
///
/// The reason is telemetry. [`Verdict::undetermined`] / [`Determination::undetermined`]
/// record one event per give-up so the fleet-wide rate of "could not decide" is
/// observable; a branch that wrote `Verdict::Undetermined(Reason::new(..))`
/// directly would be a real give-up that the counter never saw, making the
/// measurement understate itself — the same silence the measurement exists to
/// end. Rather than police that with a grep gate (which fails open on every
/// spelling it did not anticipate, and which this repo cannot even wire into
/// `.githooks` today), the field is private: `Undet(..)` is unwriteable
/// outside this module, so the only way to reach the variant is through the
/// recording constructors. Bypassing the counter is a **compile error**, not a
/// review finding.
///
/// *Propagating* an existing one stays free — `Undetermined(why) => return
/// Undetermined(why)` moves the payload and rightly does not re-record, since
/// the origin already did. That is the distinction the type draws for you:
/// minting is instrumented, forwarding is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undet(Reason);

impl Undet {
    /// The message. Mirrors [`Reason::as_str`] so `Undetermined(why)` arms read
    /// the same as they did when the payload was a bare `Reason`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The underlying reason, for the (rare) caller that needs the `Reason`
    /// itself — e.g. to fold it into a findings list.
    #[must_use]
    pub fn reason(&self) -> &Reason {
        &self.0
    }
}

impl std::fmt::Display for Undet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

/// The three answers a gate can give — "checked, clean", "checked, violation",
/// and "could not check" — kept as three distinct answers so the third can
/// never masquerade as the first.
///
/// Deliberately **not** `#[non_exhaustive]`: adding a fourth answer should break
/// every downstream `match`, forcing each gate to decide what it means (see the
/// module docs). It is `#[must_use]` so a computed verdict cannot be silently
/// dropped, and it implements neither `Default` nor `From<bool>`/`Into<bool>` so
/// a `Clean` can never appear for free.
#[must_use = "a gate verdict must be acted on (blocked/allowed), never computed and dropped"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A check ran to completion and found nothing wrong. Carries an [`Evidence`]
    /// witness that cannot be built outside this crate.
    Clean(Evidence),
    /// A check ran to completion and found a violation.
    Violation(Reason),
    /// The check could not be run to a conclusion (IO error, parse failure,
    /// subprocess crash, timeout, ambiguous output). This is **not** clean and
    /// **not** a violation — it resolves to the restricted side on every
    /// channel. Never produce this to mean "nothing found".
    ///
    /// The payload is [`Undet`], not a bare `Reason`, so this variant cannot be
    /// minted outside `harness-core` — see [`Verdict::undetermined`].
    Undetermined(Undet),
}

impl Verdict {
    /// Build a `Violation` with a reason.
    pub fn violation(reason: impl Into<String>) -> Self {
        Verdict::Violation(Reason::new(reason))
    }

    /// Build an `Undetermined` with a reason. Use this — never an empty
    /// findings list — when a check could not run to a conclusion.
    ///
    /// Emits one telemetry record (see [`crate::undetermined`]) attributed to
    /// the CALLER via `#[track_caller]`, so the crate/file/line in the stream is
    /// the branch that gave up rather than this line. Best-effort and never a
    /// gate: a telemetry failure cannot change this verdict.
    ///
    /// This is the *only* way to mint one: the [`Undet`] payload has a private
    /// field, so `Verdict::Undetermined(..)` cannot be written outside this
    /// crate and no give-up can slip past the counter.
    #[track_caller]
    pub fn undetermined(reason: impl Into<String>) -> Self {
        let reason = Reason::new(reason);
        crate::undetermined::record(reason.as_str(), std::panic::Location::caller());
        Verdict::Undetermined(Undet(reason))
    }

    /// The sanctioned Clean-minting path for a check that **cannot itself be
    /// undetermined** (a pure, in-memory check that always runs). Hand over the
    /// findings you actually collected: an empty list means "ran, found nothing"
    /// and yields `Clean`; a non-empty list yields `Violation`.
    ///
    /// A check that *can* fail to run must not call this with an empty list to
    /// stand in for "could not check" — that is the exact fail-open this module
    /// exists to prevent. Such a check returns [`Verdict::undetermined`], or
    /// carries its outcome as a [`Determination`] and uses [`Verdict::adjudicate`].
    pub fn from_findings(findings: Vec<Reason>) -> Self {
        if findings.is_empty() {
            // The evidence is minted here, inside harness-core, only on the
            // observed-empty path — never handed in by a caller.
            Verdict::Clean(Evidence(()))
        } else {
            Verdict::Violation(Reason::joined(&findings))
        }
    }

    /// Combine many partial verdicts under one priority: any `Violation`
    /// present wins over any `Undetermined`, which wins over `Clean`. This is
    /// the shared shape behind every hand-rolled "combine sub-analysis
    /// results, worst wins" accumulator this repo keeps reinventing (e.g.
    /// blastguard's `detect::VerdictAcc`, which ranks its own three answers
    /// `Deny > Ask > Allow` — the identical ordering, just named differently).
    ///
    /// All `Violation` reasons are joined (via [`Reason::joined`]) into one
    /// `Violation`, exactly like [`Verdict::from_findings`] joins multiple
    /// findings; likewise all `Undetermined` reasons are joined into one
    /// `Undetermined` when no `Violation` is present. An empty iterator
    /// yields `Clean` — mirroring `from_findings(vec![])`'s existing
    /// "observed nothing, ran to completion" semantics — so a caller that
    /// combines zero sub-verdicts gets the same "checked, found nothing"
    /// answer as a check with no findings, not a free pass minted some other
    /// way.
    pub fn worst_of(verdicts: impl IntoIterator<Item = Verdict>) -> Self {
        let mut violations = Vec::new();
        let mut undetermined = Vec::new();
        for v in verdicts {
            match v {
                Verdict::Violation(r) => violations.push(r),
                Verdict::Undetermined(r) => undetermined.push(r),
                Verdict::Clean(_) => {}
            }
        }
        if !violations.is_empty() {
            Verdict::Violation(Reason::joined(&violations))
        } else if !undetermined.is_empty() {
            // A fold of give-ups that were each already recorded at their origin.
            // Minting the combined payload directly (rather than through
            // `Verdict::undetermined`) is the correct choice here: re-recording
            // would count one give-up several times over as it bubbles up.
            let reasons: Vec<Reason> = undetermined.iter().map(|u| u.0.clone()).collect();
            Verdict::Undetermined(Undet(Reason::joined(&reasons)))
        } else {
            Verdict::from_findings(vec![])
        }
    }

    /// The sanctioned Clean-minting path for a check that **may fail to run**.
    /// The caller expresses the outcome as a [`Determination`]:
    ///
    /// * `Known(findings)` — the check ran; empty → `Clean`, else → `Violation`.
    /// * `Undetermined(why)` — the check could not run → `Undetermined`.
    ///
    /// Because the caller must choose `Known` vs `Undetermined`, a "could not
    /// run" outcome cannot silently arrive as an empty `Known` unless the caller
    /// explicitly writes one — which is greppable and intentional, not an
    /// accident of a defaulted or unwrapped value.
    pub fn adjudicate(outcome: Determination<Vec<Reason>>) -> Self {
        match outcome {
            Determination::Known(findings) => Verdict::from_findings(findings),
            Determination::Undetermined(why) => Verdict::Undetermined(why),
        }
    }

    /// True iff this verdict blocks. `Clean` is the only non-blocking answer;
    /// both `Violation` and `Undetermined` block. Named (not a `From<bool>` /
    /// `Into<bool>`, which are deliberately absent) so the fail-closed direction
    /// is explicit at every call site.
    #[must_use]
    pub fn blocks(&self) -> bool {
        !matches!(self, Verdict::Clean(_))
    }

    /// The reason, if this verdict carries one (`Violation`/`Undetermined`).
    /// `Clean` has none.
    #[must_use]
    pub fn reason(&self) -> Option<&Reason> {
        match self {
            Verdict::Clean(_) => None,
            Verdict::Violation(r) => Some(r),
            Verdict::Undetermined(u) => Some(u.reason()),
        }
    }

    /// The **Stop-hook channel**. A Stop hook exits 0 toward Claude and blocks
    /// via the `decision` field, so this returns the JSON to print, or `None`
    /// for `Clean` (print nothing, let the stop through). Both non-clean arms
    /// block; `Undetermined` blocks exactly like `Violation`. There is
    /// deliberately no arm that lets an undetermined verdict end the turn.
    #[must_use]
    pub fn stop_decision(&self) -> Option<serde_json::Value> {
        match self {
            Verdict::Clean(_) => None,
            Verdict::Violation(r) => Some(serde_json::json!({
                "decision": "block",
                "reason": r.as_str(),
            })),
            Verdict::Undetermined(u) => Some(serde_json::json!({
                "decision": "block",
                "reason": u.as_str(),
            })),
        }
    }

    /// The **exit-code channel** (a PreToolUse deny, a precommit-mode block).
    /// `Clean` is the only zero; both non-clean arms return `block_code`. The
    /// concrete blocking code is the caller's because it differs per gate (2 for
    /// a PreToolUse deny; 1/2/3 for precommit modes) — which is why there is no
    /// blanket `From<Verdict> for ExitCode`.
    ///
    /// A `block_code` of 0 would turn a block into a passing exit — the exact
    /// fail-open this type exists to prevent — so it is **clamped to a non-zero**
    /// (`1`) rather than honored: a caller's mistake fails closed (still blocks),
    /// never open. The clamp is a plain runtime branch (not a `debug_assert`) so
    /// it is exercised by the same test suite that ships, not silently compiled
    /// out of release.
    #[must_use]
    pub fn exit_code(&self, block_code: i32) -> i32 {
        let block = if block_code == 0 { 1 } else { block_code };
        match self {
            Verdict::Clean(_) => 0,
            _ => block,
        }
    }
}

/// A value that a check tried to determine, keeping "could not determine" as its
/// own answer instead of folding it into a permissive default. The generic
/// sibling of [`git_probe::RepoProbe`](crate::git_probe::RepoProbe): `Known(T)`
/// is a real observation (possibly an empty `Vec`, an empty `String`, `None` —
/// a genuine "ran, found nothing"), while `Undetermined` is "could not observe".
///
/// The distinction is the recurring lesson of this repo encoded as a type: a
/// `read_dir` that hit `NotFound` returns `Known(vec![])` (legitimately empty),
/// but one that hit `PermissionDenied` returns `Undetermined` (could not tell) —
/// never the same empty value for both.
///
/// It has exactly one extractor, [`require`](Determination::require), returning
/// [`Required<T>`] — this crate's own type, not `std::Result`. There is
/// intentionally **no** `unwrap_or`, `ok`, `unwrap_or_default`, or `Default` on
/// *either* type: those are the very APIs that turn "could not determine" into a
/// permissive value, so they do not exist here, and the extractor no longer
/// hands the caller a `std` type whose inherent methods this crate cannot
/// remove. Resolving a `Required` therefore means writing both arms, with the
/// undetermined arm receiving a ready-made fail-closed [`Verdict`].
///
/// The seal is over *method calls*, not over intent: a caller who writes
/// `Required::Blocked(_) => Vec::new()` by hand still collapses the answer. See
/// [`Required`] for why that residue is deliberately left to a lexical gate.
#[must_use = "a Determination must be resolved with `require`, not dropped"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Determination<T> {
    /// The check ran and observed this value (which may be legitimately empty).
    Known(T),
    /// The check could not run to a conclusion. Carries why. The payload is
    /// [`Undet`], not a bare `Reason`, so this variant cannot be minted outside
    /// `harness-core` — see [`Determination::undetermined`].
    Undetermined(Undet),
}

impl<T> Determination<T> {
    /// Build a `Known` observation.
    pub fn known(value: T) -> Self {
        Determination::Known(value)
    }

    /// Build an `Undetermined` with a reason.
    ///
    /// Telemetry-emitting and caller-attributed, exactly as
    /// [`Verdict::undetermined`] — and likewise the only way to mint one, since
    /// the [`Undet`] payload is unwriteable outside this crate.
    #[track_caller]
    pub fn undetermined(reason: impl Into<String>) -> Self {
        let reason = Reason::new(reason);
        crate::undetermined::record(reason.as_str(), std::panic::Location::caller());
        Determination::Undetermined(Undet(reason))
    }

    /// The one and only extractor. `Known(v)` → [`Required::Determined`]`(v)`;
    /// `Undetermined(why)` → [`Required::Blocked`] carrying the fail-closed
    /// `Verdict::Undetermined(why)` — the reason travels with it, so a caller
    /// that returns the blocked verdict loses nothing.
    ///
    /// The return type is [`Required`], **not** `std::Result`: `Result` would
    /// hand the caller `unwrap_or` / `unwrap_or_default` / `ok` / `is_ok`, whose
    /// whole effect is to turn "could not determine" back into a permissive
    /// value one call after `Determination` refused to offer exactly those. The
    /// permissive path is therefore not expressible as a method call on either
    /// type; the caller must `match` and say, in the diff, what the undetermined
    /// answer means.
    pub fn require(self) -> Required<T> {
        match self {
            Determination::Known(v) => Required::Determined(v),
            Determination::Undetermined(why) => Required::Blocked(Verdict::Undetermined(why)),
        }
    }

    /// Map the observed value while preserving `Undetermined`. Convenience for
    /// adapting a `Determination<A>` into a `Determination<B>` without touching
    /// the undetermined arm (which keeps its reason). Does not provide any way
    /// to *read* the value without going through [`require`].
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Determination<U> {
        match self {
            Determination::Known(v) => Determination::Known(f(v)),
            Determination::Undetermined(why) => Determination::Undetermined(why),
        }
    }
}

/// What [`Determination::require`] returns: the observed value, or the
/// fail-closed [`Verdict`] that stands in for "could not determine".
///
/// **Why this is not `std::Result`.** It used to be, and that placed the seal
/// one call too shallow. [`Determination`] refuses to grow an `unwrap_or` — but
/// `Result` already has one, plus `unwrap_or_default`, `unwrap_or_else`, `ok`
/// and `is_ok`, and they are `std`'s inherent methods, which this crate cannot
/// remove or bound. So `d.require().unwrap_or_default()` re-opened the exact
/// collapse the refusal existed to prevent: a `read_dir` that could not be read
/// became an empty `Vec` (read downstream as "the directory is empty", i.e.
/// clean), and `.is_ok()` flattened the whole verdict into a bool where
/// "undetermined" and "legitimately absent" are the same `false`.
/// `Required` simply has none of those methods, so each of those forms is an
/// `E0599`. `tests/ui/verdict/require_result_erasure.rs` pins that as a compiler
/// error (it was a committed RED fixture until this type existed).
///
/// **What it deliberately does NOT seal.** A hand-written
///
/// ```ignore
/// match d.require() {
///     Required::Determined(v) => v,
///     Required::Blocked(_) => Vec::new(), // still a permissive default
/// }
/// ```
///
/// collapses the answer just as thoroughly, and no type can forbid it — the
/// caller supplied that default themselves. Sealing it would mean forbidding the
/// blocked arm from producing a `T` at all, which would also forbid the
/// legitimate uses (a caller that has a genuinely conservative fallback), so the
/// API stops here on purpose. The type buys two things instead: the collapse is
/// unreachable *by accident* (no method spells it), and a deliberate one shows
/// up in a diff as an explicit arm a reviewer or a lexical gate can see
/// (backlog b4baf3d7).
///
/// **No `?`.** Implementing `std::ops::Try` would need the unstable trait
/// (E0658, rust#84277). Nothing is lost: `.require()?` occurs nowhere in this
/// repo — every existing call site already writes the two arms.
#[must_use = "a Required carries either the value or a blocking verdict; both arms must be handled"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Required<T> {
    /// The check ran and observed this value (which may be legitimately empty).
    Determined(T),
    /// The check could not run to a conclusion. Carries the already fail-closed
    /// [`Verdict::Undetermined`], reason intact, so the caller's shortest honest
    /// move is to return it.
    Blocked(Verdict),
}

impl<T> Required<T> {
    /// The value, panicking with `msg` and the blocking reason when the
    /// determination could not be made.
    ///
    /// This is **not** a permissive escape hatch and is not the counterpart of
    /// `unwrap_or`: it produces no value on the blocked path, it aborts. In a
    /// gate that matters — the Stop-hook panic barrier
    /// ([`crate::gate::run::run_guarded`]) resolves a panic to *block*, the
    /// restricted side — so this stays fail-closed. It exists for tests and for
    /// the rare call site where an undetermined answer is a genuine bug rather
    /// than a runtime possibility.
    ///
    /// # Panics
    ///
    /// Panics when `self` is [`Required::Blocked`].
    #[track_caller]
    pub fn expect(self, msg: &str) -> T {
        match self {
            Required::Determined(v) => v,
            Required::Blocked(verdict) => {
                let why = match verdict.reason() {
                    Some(r) => r.as_str().to_string(),
                    None => format!("{verdict:?}"),
                };
                panic!("{msg}: {why}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_findings_empty_is_clean_nonempty_is_violation() {
        assert!(matches!(Verdict::from_findings(vec![]), Verdict::Clean(_)));
        let v = Verdict::from_findings(vec![Reason::new("a"), Reason::new("b")]);
        match v {
            Verdict::Violation(r) => assert_eq!(r.as_str(), "a; b"),
            other => panic!("expected Violation, got {other:?}"),
        }
    }

    #[test]
    fn adjudicate_undetermined_stays_undetermined() {
        let d: Determination<Vec<Reason>> = Determination::undetermined("io error");
        assert!(matches!(Verdict::adjudicate(d), Verdict::Undetermined(_)));
        // A Known-but-empty is the ONLY empty that becomes Clean.
        let d: Determination<Vec<Reason>> = Determination::Known(vec![]);
        assert!(matches!(Verdict::adjudicate(d), Verdict::Clean(_)));
    }

    #[test]
    fn undetermined_blocks_exactly_like_violation() {
        // The core invariant: the third answer resolves to the restricted side.
        for v in [
            Verdict::undetermined("could not read"),
            Verdict::violation("found a problem"),
        ] {
            assert!(v.blocks(), "{v:?} must block");
            assert_eq!(v.exit_code(2), 2, "{v:?} must map to the block code");
            let d = v.stop_decision().expect("non-clean emits a decision");
            assert_eq!(d["decision"], "block", "{v:?} must block the stop");
        }
    }

    #[test]
    fn clean_passes_on_every_channel() {
        let v = Verdict::from_findings(vec![]);
        assert!(!v.blocks());
        assert_eq!(v.exit_code(2), 0, "clean is the only zero");
        assert!(
            v.stop_decision().is_none(),
            "clean emits no decision (lets the stop through)"
        );
        assert!(v.reason().is_none());
    }

    #[test]
    fn require_hands_the_undetermined_arm_a_fail_closed_verdict() {
        // `require` forces both arms; the blocked arm arrives as a ready-made
        // fail-closed verdict, so returning it is the shortest honest path.
        fn use_it(d: Determination<Vec<Reason>>) -> Verdict {
            let found = match d.require() {
                Required::Determined(v) => v,
                Required::Blocked(verdict) => return verdict, // fail closed
            };
            Verdict::from_findings(found)
        }
        assert!(matches!(
            use_it(Determination::undetermined("perm denied")),
            Verdict::Undetermined(_)
        ));
        assert!(matches!(
            use_it(Determination::Known(vec![])),
            Verdict::Clean(_)
        ));
    }

    #[test]
    fn require_blocked_carries_the_reason_downstream() {
        // The seal must not cost information: the blocked arm still knows WHY,
        // otherwise the caller is handed a bare "blocked" with no cause.
        let d: Determination<u8> = Determination::undetermined("perm denied on /x");
        match d.require() {
            Required::Blocked(v) => {
                assert!(v.blocks(), "the blocked arm must carry a blocking verdict");
                assert_eq!(
                    v.reason().map(Reason::as_str),
                    Some("perm denied on /x"),
                    "the reason must survive `require`"
                );
                assert!(matches!(v, Verdict::Undetermined(_)));
            }
            Required::Determined(_) => panic!("expected Blocked"),
        }
    }

    #[test]
    fn require_determined_yields_the_observed_value() {
        match Determination::Known(7u8).require() {
            Required::Determined(v) => assert_eq!(v, 7),
            Required::Blocked(v) => panic!("expected Determined, got {v:?}"),
        }
        assert_eq!(Determination::Known(7u8).require().expect("known"), 7);
    }

    #[test]
    #[should_panic(expected = "needed the listing: ledger unreadable")]
    fn required_expect_aborts_on_blocked_rather_than_substituting_a_value() {
        // `expect` is the only extractor shortcut, and it produces NO value on
        // the blocked path — it panics, which the Stop-hook panic barrier
        // resolves to `block`. It is not an `unwrap_or` in disguise.
        let d: Determination<u8> = Determination::undetermined("ledger unreadable");
        let _ = d.require().expect("needed the listing");
    }

    #[test]
    fn exit_code_clamps_zero_block_code_so_a_block_never_exits_zero() {
        // A caller bug (block_code = 0) must not turn a block into a passing 0:
        // it clamps to a non-zero. Clean still exits 0 regardless.
        assert_ne!(
            Verdict::undetermined("x").exit_code(0),
            0,
            "a blocking verdict must never map to a passing exit 0"
        );
        assert_ne!(Verdict::violation("y").exit_code(0), 0);
        assert_eq!(
            Verdict::from_findings(vec![]).exit_code(0),
            0,
            "clean is 0 on any block_code"
        );
    }

    #[test]
    fn determination_map_preserves_undetermined() {
        let d: Determination<u8> = Determination::undetermined("nope");
        assert!(matches!(d.map(|n| n + 1), Determination::Undetermined(_)));
        let d: Determination<u8> = Determination::Known(1);
        assert!(matches!(d.map(|n| n + 1), Determination::Known(2)));
    }

    #[test]
    fn worst_of_empty_is_clean() {
        assert!(matches!(Verdict::worst_of(vec![]), Verdict::Clean(_)));
    }

    #[test]
    fn worst_of_violation_outranks_undetermined_and_clean() {
        let v = Verdict::worst_of(vec![
            Verdict::from_findings(vec![]),
            Verdict::undetermined("could not read"),
            Verdict::violation("found a problem"),
        ]);
        match v {
            Verdict::Violation(r) => assert_eq!(r.as_str(), "found a problem"),
            other => panic!("expected Violation, got {other:?}"),
        }
    }

    #[test]
    fn worst_of_undetermined_outranks_clean_when_no_violation_present() {
        let v = Verdict::worst_of(vec![
            Verdict::from_findings(vec![]),
            Verdict::undetermined("io error"),
        ]);
        match v {
            Verdict::Undetermined(r) => assert_eq!(r.as_str(), "io error"),
            other => panic!("expected Undetermined, got {other:?}"),
        }
    }

    #[test]
    fn worst_of_all_clean_is_clean() {
        let v = Verdict::worst_of(vec![
            Verdict::from_findings(vec![]),
            Verdict::from_findings(vec![]),
        ]);
        assert!(matches!(v, Verdict::Clean(_)));
    }

    #[test]
    fn worst_of_joins_multiple_violations_like_from_findings() {
        let v = Verdict::worst_of(vec![Verdict::violation("a"), Verdict::violation("b")]);
        match v {
            Verdict::Violation(r) => assert_eq!(r.as_str(), "a; b"),
            other => panic!("expected Violation, got {other:?}"),
        }
    }
}
