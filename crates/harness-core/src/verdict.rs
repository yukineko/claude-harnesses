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
//!    returning `Result<T, Verdict>`. There is no `unwrap_or`, no `ok()`, no
//!    `unwrap_or_default` — so "could not determine" cannot be swapped for a
//!    permissive default, and `?` makes fail-closed the *shortest* path.
//! 5. **Every channel conversion sends `Undetermined` to the restricted side,**
//!    and no conversion mapping it to the permissive side exists. The blocking
//!    channel differs per gate (a Stop hook blocks via a JSON `decision` field
//!    while exiting 0 toward Claude; a PreToolUse/precommit gate blocks via a
//!    non-zero exit code), so there is deliberately no single
//!    `From<Verdict> for ExitCode` that would hard-code one gate's convention
//!    onto all of them. Instead [`Verdict::stop_decision`] and
//!    [`Verdict::exit_code`] each map `Undetermined` exactly like `Violation`.

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
    Undetermined(Reason),
}

impl Verdict {
    /// Build a `Violation` with a reason.
    pub fn violation(reason: impl Into<String>) -> Self {
        Verdict::Violation(Reason::new(reason))
    }

    /// Build an `Undetermined` with a reason. Use this — never an empty
    /// findings list — when a check could not run to a conclusion.
    pub fn undetermined(reason: impl Into<String>) -> Self {
        Verdict::Undetermined(Reason::new(reason))
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
            Verdict::Undetermined(Reason::joined(&undetermined))
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
            Verdict::Violation(r) | Verdict::Undetermined(r) => Some(r),
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
            Verdict::Violation(r) | Verdict::Undetermined(r) => Some(serde_json::json!({
                "decision": "block",
                "reason": r.as_str(),
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
/// `Result<T, Verdict>`. There is intentionally **no** `unwrap_or`, `ok`,
/// `unwrap_or_default`, or `Default`: those are the very APIs that turn "could
/// not determine" into a permissive value, so they do not exist here. `?` on the
/// result propagates a [`Verdict::Undetermined`], making fail-closed the
/// shortest path a caller can write.
#[must_use = "a Determination must be resolved with `require`, not dropped"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Determination<T> {
    /// The check ran and observed this value (which may be legitimately empty).
    Known(T),
    /// The check could not run to a conclusion. Carries why.
    Undetermined(Reason),
}

impl<T> Determination<T> {
    /// Build a `Known` observation.
    pub fn known(value: T) -> Self {
        Determination::Known(value)
    }

    /// Build an `Undetermined` with a reason.
    pub fn undetermined(reason: impl Into<String>) -> Self {
        Determination::Undetermined(Reason::new(reason))
    }

    /// The one and only extractor. `Known(v)` → `Ok(v)`; `Undetermined(why)` →
    /// `Err(Verdict::Undetermined(why))`. Using `?` therefore short-circuits a
    /// caller to a fail-closed `Undetermined` verdict with no extra code — the
    /// permissive path (substitute a default and continue) simply is not
    /// expressible, because no `unwrap_or`/`ok`/`unwrap_or_default` exists.
    pub fn require(self) -> Result<T, Verdict> {
        match self {
            Determination::Known(v) => Ok(v),
            Determination::Undetermined(why) => Err(Verdict::Undetermined(why)),
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
    fn require_is_fail_closed_via_question_mark() {
        // `?` on an Undetermined determination short-circuits to a fail-closed
        // Undetermined verdict — the shortest path a caller can write.
        fn use_it(d: Determination<Vec<Reason>>) -> Verdict {
            let found = match d.require() {
                Ok(v) => v,
                Err(verdict) => return verdict, // fail closed
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
