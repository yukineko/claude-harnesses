//! `hypothesis draft` — interrogate an ambiguous task text and list what is
//! still unresolved before it can be recorded as a falsifiable bet.
//!
//! # Division of labour (why this module exists at all)
//!
//! Every side effect lives here: loading the store, printing, writing a record.
//! `harness_core::interrogate` stays pure — this module hands it a
//! [`Bundle`] and reads back [`OpenQuestion`]s, and the rigor gates below are
//! plain functions over that bundle. That split is what keeps
//! `harness-core/src/interrogate.rs` free of file/process/network IO (guarded
//! lexically by `crates/harness-core/tests/interrogate_purity.rs`), and it is
//! also what makes the interrogation drivable by a SKILL: the LLM asks the human,
//! the binary persists nothing until the answers are in.
//!
//! # Fail-closed direction
//!
//! While anything is open, **no record is written**. That refusal is the block:
//! `draft` exits 0 having printed the open items (the interrogation is expected
//! to continue, so a non-zero exit would read as a crash), but the store is
//! untouched. There is deliberately no "record a minimal hypothesis so the flow
//! can proceed" path — that would be this gate disabling itself.
//!
//! The scope half is enforced by a type rather than by this module's care:
//! [`crate::store::Store::add_draft`] takes a
//! [`ScopeDeclaration`](harness_core::interrogate::ScopeDeclaration), which
//! cannot be constructed except through
//! [`ScopeDraft::declare`](harness_core::interrogate::ScopeDraft::declare). So
//! there is no route into the store for a scope nobody resolved, even if a future
//! edit reordered or dropped the checks below.
//!
//! # Unresolved rigor points
//!
//! | gate | reference | asks |
//! |---|---|---|
//! | D1 | `scope` | which files this will write, and which it will read |
//! | D2 | `success_criterion` | the pre-registered bar validation must clear |
//! | D3 | `kill_criterion` | the measured value that would disprove the bet |
//! | D4 | `linked_goal` | which compass goal this serves |
//!
//! D2/D3 deliberately reuse the existing
//! [`Criterion`](crate::hypothesis::Criterion) fields rather than introducing a
//! new "metric surface" list: a criterion is `metric` + `comparator` +
//! `threshold`, i.e. a bar that `validate` can actually check a measurement
//! against, whereas a bare list of metric names is unfalsifiable — it names what
//! to look at without saying what would count as failure.

use anyhow::Result;
use harness_core::interrogate::{
    evaluate, Authority, Bundle, Fragment, OpenQuestion, RigorGates, ScopeDeclaration, ScopeDraft,
};
use harness_core::verdict::Required;

use crate::config::Config;
use crate::hypothesis::{Criterion, Hypothesis};
use crate::store::Store;

/// `source_path` markers for the fragments this module folds into the bundle.
///
/// The gates match on these exactly (not by prefix), which is why
/// `..._WRITE_EMPTY` can safely start with `..._WRITE`: an "answered nothing"
/// marker must never be mistaken for an entry named the empty string.
const SRC_TASK: &str = "draft:task";
const SRC_GOAL: &str = "draft:linked_goal";
const SRC_SUCCESS: &str = "draft:success_criterion";
const SRC_KILL: &str = "draft:kill_criterion";
const SRC_SCOPE_WRITE: &str = "draft:scope:write";
const SRC_SCOPE_WRITE_EMPTY: &str = "draft:scope:write:answered-empty";
const SRC_SCOPE_READ: &str = "draft:scope:read";
const SRC_SCOPE_READ_EMPTY: &str = "draft:scope:read:answered-empty";

/// The answers `hypothesis draft` was invoked with.
///
/// The scope fields are `Option<Vec<String>>`, not `Vec<String>`, because "the
/// question was never asked" and "asked, and the answer is nothing" are
/// different answers and `declare` treats them differently. Flattening them to
/// an empty vec here would destroy that distinction before the gates ever see it.
pub struct DraftArgs {
    pub text: String,
    pub goal: Option<String>,
    pub success: Option<Criterion>,
    pub kill: Option<Criterion>,
    pub write_paths: Option<Vec<String>>,
    pub read_paths: Option<Vec<String>>,
}

/// The rigor gates for a drafted hypothesis. Pure: reads only the bundle,
/// performs no IO, and never auto-resolves anything by authority.
struct DraftGates;

impl RigorGates for DraftGates {
    fn evaluate(&self, bundle: &Bundle) -> Vec<OpenQuestion> {
        let mut open = Vec::new();

        // D1 — scope. The gap text is `declare`'s own refusal reason verbatim, so
        // the machine-readable rule tag (`scope:write_paths_unasked` etc.) reaches
        // the operator and any downstream parser unaltered instead of being
        // re-worded here, where the two could drift apart.
        match scope_draft_from_bundle(bundle).declare().require() {
            Required::Determined(_declared) => {}
            Required::Blocked(verdict) => {
                let gap = match verdict.reason() {
                    Some(reason) => reason.as_str().to_string(),
                    // Unreachable: `require`'s blocked arm always carries
                    // `Verdict::Undetermined`, which always carries a reason.
                    // Stated non-silently anyway — an empty gap would read as
                    // "nothing is wrong here", which is the opposite of the truth.
                    None => format!("scope is undetermined and stated no reason: {verdict:?}"),
                };
                open.push(OpenQuestion {
                    gate: "D1".to_string(),
                    reference: "scope".to_string(),
                    gap,
                    sources: fragments_matching(
                        bundle,
                        &[
                            SRC_SCOPE_WRITE,
                            SRC_SCOPE_WRITE_EMPTY,
                            SRC_SCOPE_READ,
                            SRC_SCOPE_READ_EMPTY,
                        ],
                    ),
                    default: None,
                });
            }
        }

        if !has_nonblank(bundle, SRC_SUCCESS) {
            open.push(OpenQuestion {
                gate: "D2".to_string(),
                reference: "success_criterion".to_string(),
                gap: "no pre-registered success criterion: without a bar fixed \
                      before the work ships, any later measurement can be read as \
                      a success"
                    .to_string(),
                sources: fragments_matching(bundle, &[SRC_TASK]),
                default: None,
            });
        }

        if !has_nonblank(bundle, SRC_KILL) {
            open.push(OpenQuestion {
                gate: "D3".to_string(),
                reference: "kill_criterion".to_string(),
                gap: "no pre-registered kill criterion: a bet with no value that \
                      would disprove it is not falsifiable"
                    .to_string(),
                sources: fragments_matching(bundle, &[SRC_TASK]),
                default: None,
            });
        }

        if !has_nonblank(bundle, SRC_GOAL) {
            open.push(OpenQuestion {
                gate: "D4".to_string(),
                reference: "linked_goal".to_string(),
                gap: "not linked to a compass goal: an unattached bet cannot be \
                      prioritised against anything"
                    .to_string(),
                sources: fragments_matching(bundle, &[SRC_TASK]),
                default: None,
            });
        }

        open
    }
}

/// True iff the bundle carries a fragment at `source_path` whose text is not
/// blank. A blank answer is treated as no answer — the same rule
/// `ScopeDraft::declare` applies to a blank path entry, for the same reason: a
/// placeholder must not satisfy a rigor point.
fn has_nonblank(bundle: &Bundle, source_path: &str) -> bool {
    bundle
        .fragments
        .iter()
        .any(|f| f.source_path == source_path && !f.text.trim().is_empty())
}

fn fragments_matching(bundle: &Bundle, source_paths: &[&str]) -> Vec<Fragment> {
    bundle
        .fragments
        .iter()
        .filter(|f| source_paths.contains(&f.source_path.as_str()))
        .cloned()
        .collect()
}

/// Rebuild the [`ScopeDraft`] the bundle encodes.
///
/// Pure and total. The three-way distinction survives the round trip through the
/// bundle: no marker at all → `None` (unasked); an `answered-empty` marker →
/// `Some(vec![])`; one fragment per path → `Some(paths)` in bundle order (which
/// `declare` then preserves verbatim).
fn scope_draft_from_bundle(bundle: &Bundle) -> ScopeDraft {
    ScopeDraft {
        write_paths: answered_paths(bundle, SRC_SCOPE_WRITE, SRC_SCOPE_WRITE_EMPTY),
        read_paths: answered_paths(bundle, SRC_SCOPE_READ, SRC_SCOPE_READ_EMPTY),
    }
}

fn answered_paths(bundle: &Bundle, entry_src: &str, empty_src: &str) -> Option<Vec<String>> {
    let entries: Vec<String> = bundle
        .fragments
        .iter()
        .filter(|f| f.source_path == entry_src)
        .map(|f| f.text.clone())
        .collect();
    if !entries.is_empty() {
        return Some(entries);
    }
    if bundle.fragments.iter().any(|f| f.source_path == empty_src) {
        return Some(Vec::new());
    }
    None
}

fn fragment(text: impl Into<String>, source_path: &str, authority: Authority) -> Fragment {
    Fragment {
        text: text.into(),
        source_path: source_path.to_string(),
        authority,
        score: 0,
        anchor: None,
    }
}

/// Encode one scope answer as bundle fragments, preserving the
/// unasked / answered-empty / answered distinction.
fn push_scope(
    out: &mut Vec<Fragment>,
    answered: Option<&[String]>,
    entry_src: &str,
    empty_src: &str,
    authority: Authority,
) {
    match answered {
        // Unasked: contribute nothing, so the bundle cannot be read as an answer.
        None => {}
        Some([]) => out.push(fragment(String::new(), empty_src, authority)),
        Some(paths) => {
            for path in paths {
                out.push(fragment(path.clone(), entry_src, authority));
            }
        }
    }
}

/// Fold the invocation's answers, plus anything already recorded for this exact
/// task text, into the bundle the gates read.
///
/// Precedence is explicit rather than authority-driven: an answer given on this
/// invocation ([`Authority::High`]) wins, and a previously recorded one
/// ([`Authority::Mid`]) is only consulted where this invocation said nothing.
/// `Authority` stays a presentation hint, exactly as `interrogate`'s contract
/// requires — it is never used to resolve a conflict in code.
///
/// Pure: `recorded` is already-loaded data, so this function does no IO and the
/// same inputs always produce the same bundle.
fn build_bundle(args: &DraftArgs, recorded: Option<&Hypothesis>) -> Bundle {
    let mut fragments = vec![fragment(args.text.clone(), SRC_TASK, Authority::High)];

    let goal = args
        .goal
        .clone()
        .or_else(|| recorded.and_then(|h| h.linked_goal.clone()));
    if let Some(goal) = goal {
        fragments.push(fragment(goal, SRC_GOAL, Authority::High));
    }

    let success = args
        .success
        .clone()
        .or_else(|| recorded.and_then(|h| h.success_criterion.clone()));
    if let Some(c) = success {
        fragments.push(fragment(c.to_string(), SRC_SUCCESS, Authority::High));
    }

    let kill = args
        .kill
        .clone()
        .or_else(|| recorded.and_then(|h| h.kill_criterion.clone()));
    if let Some(c) = kill {
        fragments.push(fragment(c.to_string(), SRC_KILL, Authority::High));
    }

    let (write, write_authority) = match &args.write_paths {
        Some(paths) => (Some(paths.clone()), Authority::High),
        None => (
            recorded.and_then(|h| h.scope_write_paths.clone()),
            Authority::Mid,
        ),
    };
    push_scope(
        &mut fragments,
        write.as_deref(),
        SRC_SCOPE_WRITE,
        SRC_SCOPE_WRITE_EMPTY,
        write_authority,
    );

    let (read, read_authority) = match &args.read_paths {
        Some(paths) => (Some(paths.clone()), Authority::High),
        None => (
            recorded.and_then(|h| h.scope_read_paths.clone()),
            Authority::Mid,
        ),
    };
    push_scope(
        &mut fragments,
        read.as_deref(),
        SRC_SCOPE_READ,
        SRC_SCOPE_READ_EMPTY,
        read_authority,
    );

    Bundle { fragments }
}

/// Run `hypothesis draft`.
///
/// Prints one line per unresolved rigor point and records nothing; or, when
/// nothing is left open, records the hypothesis and prints its id.
pub fn run(cfg: &Config, args: DraftArgs) -> Result<()> {
    // IO: the existing store. A task text that already has a record contributes
    // its answers, so a second `draft` interrogates what is still missing instead
    // of asking everything again.
    let mut store = Store::load(cfg)?;
    let existing_id = crate::hypothesis::new_id(&args.text);
    let recorded = store.all().iter().find(|h| h.id == existing_id).cloned();

    let bundle = build_bundle(&args, recorded.as_ref());
    let open = evaluate(&DraftGates, &bundle);

    if !open.is_empty() {
        // Fail closed: nothing is written while anything is unresolved. Exit 0 —
        // an open question is the expected outcome of an interrogation, not an
        // error — but the store stays untouched.
        for q in &open {
            println!("[{}] {} — {}", q.gate, q.reference, q.gap);
        }
        return Ok(());
    }

    // Nothing is open, so `declare` must answer `Known`. It is called (rather
    // than the D1 result being cached) because `add_draft` demands a
    // `ScopeDeclaration` and this is the only way to make one.
    let declared: ScopeDeclaration = match scope_draft_from_bundle(&bundle).declare().require() {
        Required::Determined(declared) => declared,
        Required::Blocked(verdict) => {
            // Unreachable while the D1 gate above returns early on this same
            // answer. Kept as a refusal rather than a substituted empty
            // declaration so that reordering or removing that gate cannot turn
            // "could not determine" into a recorded scope.
            let why = match verdict.reason() {
                Some(reason) => reason.as_str().to_string(),
                None => format!("{verdict:?}"),
            };
            anyhow::bail!("refusing to record a hypothesis whose scope is undetermined: {why}");
        }
    };

    if recorded.is_some() {
        println!("{existing_id} already recorded; nothing left open");
        return Ok(());
    }

    let id = store.add_draft(
        args.text.clone(),
        args.goal.clone(),
        args.success.clone(),
        args.kill.clone(),
        &declared,
    )?;
    println!("{id}");
    Ok(())
}

/// **Provenance caveat (CLAUDE.md §2(a)).** These tests were written by the same
/// agent that wrote the module above, so they are *not* independent verification
/// — an implementer and their own tests share their blind spots by construction.
/// They are here because an untested module is worse than a self-tested one, and
/// they are reported as implementer-written so a disinterested agent can replace
/// or extend them. Read them as documentation of intended behaviour that at least
/// executes, not as evidence that the intent is right.
#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> DraftArgs {
        DraftArgs {
            text: text.to_string(),
            goal: None,
            success: None,
            kill: None,
            write_paths: None,
            read_paths: None,
        }
    }

    /// A maximally vague task with nothing answered leaves every gate open.
    #[test]
    fn all_four_gates_open_for_a_bare_task() {
        let bundle = build_bundle(&args("make it better somehow"), None);
        let open = evaluate(&DraftGates, &bundle);
        let gates: Vec<&str> = open.iter().map(|q| q.gate.as_str()).collect();
        assert_eq!(gates, ["D1", "D2", "D3", "D4"]);
    }

    /// The scope gate reports `declare`'s rule tag verbatim, so the reason a
    /// scope was refused is observable rather than re-worded.
    #[test]
    fn scope_gap_carries_the_declare_rule_tag() {
        let bundle = build_bundle(&args("t"), None);
        let open = evaluate(&DraftGates, &bundle);
        let scope = open
            .iter()
            .find(|q| q.reference == "scope")
            .expect("D1 open");
        assert!(
            scope
                .gap
                .contains(harness_core::interrogate::SCOPE_WRITE_PATHS_UNASKED),
            "gap must carry the rule tag verbatim, got {:?}",
            scope.gap
        );
    }

    /// The unasked / answered-empty distinction survives the bundle round trip:
    /// an answered-empty read set is accepted, an answered-empty write set is not.
    #[test]
    fn answered_empty_read_set_round_trips_and_is_accepted() {
        let mut a = args("t");
        a.write_paths = Some(vec!["crates/hypothesis/src/draft.rs".to_string()]);
        a.read_paths = Some(vec![]);
        let bundle = build_bundle(&a, None);

        let draft = scope_draft_from_bundle(&bundle);
        assert_eq!(
            draft.read_paths,
            Some(vec![]),
            "answered-empty, not unasked"
        );

        let open = evaluate(&DraftGates, &bundle);
        assert!(
            !open.iter().any(|q| q.reference == "scope"),
            "a fully answered scope must close D1, got {open:?}"
        );
    }

    #[test]
    fn answered_empty_write_set_is_refused_with_its_own_tag() {
        let mut a = args("t");
        a.write_paths = Some(vec![]);
        a.read_paths = Some(vec![]);
        let bundle = build_bundle(&a, None);
        let open = evaluate(&DraftGates, &bundle);
        let scope = open
            .iter()
            .find(|q| q.reference == "scope")
            .expect("D1 open");
        assert!(
            scope
                .gap
                .contains(harness_core::interrogate::SCOPE_WRITE_PATHS_EMPTY),
            "got {:?}",
            scope.gap
        );
    }

    /// Paths reach `declare` verbatim — order kept, duplicates kept, padding
    /// kept — because the bundle encodes one fragment per entry in order.
    #[test]
    fn scope_paths_reach_declare_verbatim() {
        let mut a = args("t");
        a.write_paths = Some(vec![
            "  b.rs  ".to_string(),
            "a.rs".to_string(),
            "  b.rs  ".to_string(),
        ]);
        a.read_paths = Some(vec![]);
        let draft = scope_draft_from_bundle(&build_bundle(&a, None));
        assert_eq!(
            draft.write_paths,
            Some(vec![
                "  b.rs  ".to_string(),
                "a.rs".to_string(),
                "  b.rs  ".to_string(),
            ])
        );
    }

    /// A blank answer is no answer: `--goal ""` must not close D4.
    #[test]
    fn blank_goal_does_not_close_the_goal_gate() {
        let mut a = args("t");
        a.goal = Some("   ".to_string());
        let bundle = build_bundle(&a, None);
        let open = evaluate(&DraftGates, &bundle);
        assert!(open.iter().any(|q| q.reference == "linked_goal"));
    }

    /// A previously recorded answer is consulted only where this invocation said
    /// nothing, and it can close a gate on its own.
    #[test]
    fn recorded_scope_closes_the_scope_gate_when_not_overridden() {
        let mut h = Hypothesis::draft("t", Some("goal-1".to_string()));
        h.scope_write_paths = Some(vec!["a.rs".to_string()]);
        h.scope_read_paths = Some(vec![]);

        let bundle = build_bundle(&args("t"), Some(&h));
        let open = evaluate(&DraftGates, &bundle);
        assert!(!open.iter().any(|q| q.reference == "scope"));
        assert!(!open.iter().any(|q| q.reference == "linked_goal"));
    }

    #[test]
    fn invocation_scope_overrides_the_recorded_one() {
        let mut h = Hypothesis::draft("t", None);
        h.scope_write_paths = Some(vec!["recorded.rs".to_string()]);
        h.scope_read_paths = Some(vec![]);

        let mut a = args("t");
        a.write_paths = Some(vec!["fresh.rs".to_string()]);
        let draft = scope_draft_from_bundle(&build_bundle(&a, Some(&h)));
        assert_eq!(
            draft.write_paths,
            Some(vec!["fresh.rs".to_string()]),
            "this invocation's answer wins; the recorded one must not be merged in"
        );
        assert_eq!(
            draft.read_paths,
            Some(vec![]),
            "read_paths was not answered here, so the recorded answer stands"
        );
    }
}
