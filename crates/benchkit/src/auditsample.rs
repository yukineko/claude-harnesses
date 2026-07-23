//! Post-hoc sampling calibration loop (framework item 4: 事後サンプリング較正).
//!
//! The auto-gates (propguard / specguard / mutategate / blastguard) let a change
//! land the moment they all pass. Nothing measures how *effective* those gates
//! actually are — a threshold like mutategate's 0.80 stays a fixed rule of thumb
//! and silent decay goes unnoticed. This module closes that loop: it takes the
//! population of changes that passed **only** via auto-gates, draws a
//! deterministic random sample, routes the sample through a stricter audit, and
//! turns the *misses* the audit finds into two explicit feedback paths:
//!
//!   a) **new-invariant candidates** — proposals for propguard / specguard to
//!      grow a fresh property that would have caught the miss, and
//!   b) **threshold-adjustment proposals** — suggestions to tighten a gate's
//!      threshold, which are **never auto-applied**: they land on a ratify queue
//!      for a human to accept or reject.
//!
//! There are two ways to source the audit verdicts:
//!
//!   1. **Modeled input** (`--audits <file>`): a JSONL of [`AuditResult`] rows
//!      supplied by the caller. Kept for backward-compat and unit testing — but
//!      it audits *nothing* on its own, it just replays a hand-authored verdict.
//!   2. **Real cross-reference** (`--from-violations`): the audit is derived
//!      from **actual data**. overwatch's four gates (blastguard / propguard /
//!      specguard / mutategate) now emit real [`ViolationEvent`]s into a per-
//!      project store whenever a violation is later detected. A change that
//!      passed all auto-gates but *subsequently* shows up in that violation
//!      stream (matched by a stable key — its `change_id` against the
//!      violation's `task_key`) is, by definition, a **miss the gates let
//!      through**. Deriving [`AuditResult`]s from those matches makes the audit
//!      real (it reads observed violations, not a modeled file), deterministic
//!      (a pure cross-reference, no LLM), and closes the loop from the gates'
//!      emitted violations (B) back into calibration (A).
//!
//! Design invariants (this is a gate/tooling binary, so it must be hermetic):
//!   * **No LLM in the decision path.** The audit verdict is either structured
//!     input the loop consumes, or a deterministic cross-reference of the
//!     recorded violation stream. Modeling a real adversarial reviewer is out of
//!     scope; the real path substitutes *observed* violations for a reviewer.
//!   * **Deterministic sampling.** Sampling is driven by a caller-supplied
//!     `seed` and a splitmix64 PRNG — never `Date.now()` / unseeded rand — so a
//!     given (population, fraction, seed) always yields the same sample. That is
//!     what makes the loop reproducible and unit-testable.
//!   * **Pure routing.** The two-path feedback (`route_feedback`) is a pure
//!     function of the audit outcomes; it performs no I/O and never mutates a
//!     gate's live config. Ratification stays a separate, human step.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use overwatch::violation::{ViolationEvent, ViolationSource};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// One change that passed **only** via auto-gates — the population this loop
/// samples from. One record per gate-passed change; the CLI reads a JSONL of
/// these (`benchkit auditsample`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatePassedChange {
    /// Stable change id (e.g. a commit sha or task id) — used as the sort key so
    /// sampling is deterministic regardless of input line order.
    pub change_id: String,
    /// Which auto-gate(s) cleared this change (e.g. `["propguard","mutategate"]`).
    /// Kept so a threshold proposal can name the responsible gate.
    #[serde(default)]
    pub gates: Vec<String>,
}

/// The verdict a stricter audit returns for one sampled change. Modeled as
/// structured input (no real second AI): a `miss` is a defect the auto-gates
/// let through, carrying enough context to seed both feedback paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditResult {
    /// The `change_id` this audit verdict is about.
    pub change_id: String,
    /// `true` iff the stricter audit found a defect the auto-gates missed.
    pub miss: bool,
    /// The gate that *should* have caught the miss (e.g. `"mutategate"`). Drives
    /// the threshold-adjustment path. Empty when `miss` is false.
    #[serde(default)]
    pub gate: String,
    /// A short name for the invariant/property that would have caught the miss
    /// (e.g. `"auth_boundary_preserved"`). Drives the new-invariant path.
    #[serde(default)]
    pub invariant_hint: String,
}

/// A proposal to grow a new machine-checkable invariant on propguard/specguard
/// so the class of miss the audit found is caught automatically next time.
/// Path (a) — advisory; a human/agent still authors the actual property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantCandidate {
    /// The change whose audit surfaced this candidate.
    pub change_id: String,
    /// Named hint for the invariant to add (from the audit).
    pub invariant: String,
}

/// A proposal to tighten a gate's threshold, surfaced by the calibration loop.
/// Path (b) — **never auto-applied**: it lands on the ratify queue for a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThresholdProposal {
    /// The gate whose threshold the audit suggests tightening.
    pub gate: String,
    /// The change whose miss motivated the proposal (evidence for the reviewer).
    pub change_id: String,
    /// How many sampled misses were attributed to this gate — the strength of
    /// the signal the reviewer weighs before ratifying.
    pub miss_count: usize,
}

/// The result of routing audit misses through the two explicit feedback paths.
/// Both lists are advisory: nothing here is applied to a live gate. The
/// threshold proposals in particular are a *ratify queue*, not a mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    /// Path (a): new-invariant candidates for propguard / specguard.
    pub invariant_candidates: Vec<InvariantCandidate>,
    /// Path (b): threshold-adjustment proposals awaiting human ratification.
    /// **Never auto-applied.**
    pub ratify_queue: Vec<ThresholdProposal>,
}

/// A splitmix64 step — a tiny, well-mixed, fully deterministic PRNG. Given a
/// seed it produces a reproducible stream, so sampling is hermetic (no clock,
/// no OS entropy). Reference constants are the published splitmix64 ones.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministically sample a `fraction` of the gate-passed population.
///
/// The sample size is `round(fraction * population)`, clamped to `[0, len]`.
/// Selection is a seeded Fisher–Yates prefix over the population sorted by
/// `change_id`, so the result depends only on `(population set, fraction, seed)`
/// — never on input ordering or wall-clock time. `fraction` is clamped to
/// `[0.0, 1.0]`; a fraction of `0.0` (or an empty population) yields an empty
/// sample with no panic.
///
/// Returns the sampled changes sorted by `change_id` for a stable, diffable
/// output. Pure: no I/O, no clock, no env.
pub fn sample(population: &[GatePassedChange], fraction: f64, seed: u64) -> Vec<GatePassedChange> {
    let fraction = fraction.clamp(0.0, 1.0);
    let n = population.len();
    if n == 0 || fraction == 0.0 {
        return Vec::new();
    }

    // Sort by change_id so the sample is independent of input line order.
    let mut ordered: Vec<GatePassedChange> = population.to_vec();
    ordered.sort_by(|a, b| a.change_id.cmp(&b.change_id));

    // round-half-up sample size, clamped to the population.
    let take = ((fraction * n as f64).round() as usize).min(n);
    if take == 0 {
        return Vec::new();
    }

    // Seeded Fisher–Yates over indices; take the first `take` as the sample.
    let mut idx: Vec<usize> = (0..n).collect();
    let mut state = seed;
    for i in (1..n).rev() {
        // Unbiased-enough for tooling: draw in [0, i]. splitmix64 is the source.
        let j = (splitmix64(&mut state) % (i as u64 + 1)) as usize;
        idx.swap(i, j);
    }

    let mut sampled: Vec<GatePassedChange> = idx
        .into_iter()
        .take(take)
        .map(|i| ordered[i].clone())
        .collect();
    // Return in stable change_id order for a diffable report.
    sampled.sort_by(|a, b| a.change_id.cmp(&b.change_id));
    sampled
}

/// Route audit misses through the two explicit feedback paths (pure).
///
/// Only `miss == true` results feed back. For each miss:
///   * path (a): if it names an `invariant_hint`, emit an [`InvariantCandidate`]
///     for propguard / specguard, and
///   * path (b): if it names a responsible `gate`, contribute a
///     [`ThresholdProposal`] onto the ratify queue.
///
/// Threshold proposals are aggregated per gate: `miss_count` is how many sampled
/// misses were attributed to that gate, and `change_id` cites the first (by id)
/// miss as evidence. Nothing here is applied — the ratify queue is for a human.
///
/// Both output lists are sorted (candidates by `change_id`, the queue by `gate`)
/// for a deterministic, diffable report. Pure: no I/O, no clock, no env.
pub fn route_feedback(audits: &[AuditResult]) -> Feedback {
    let mut invariant_candidates = Vec::new();
    // gate -> (miss_count, first-by-id change_id evidence)
    let mut per_gate: BTreeMap<String, (usize, String)> = BTreeMap::new();

    for a in audits {
        if !a.miss {
            continue;
        }
        // Path (a): new-invariant candidate (only when the audit named a hint).
        if !a.invariant_hint.is_empty() {
            invariant_candidates.push(InvariantCandidate {
                change_id: a.change_id.clone(),
                invariant: a.invariant_hint.clone(),
            });
        }
        // Path (b): threshold-adjustment proposal, aggregated per responsible gate.
        if !a.gate.is_empty() {
            let entry = per_gate
                .entry(a.gate.clone())
                .or_insert((0, a.change_id.clone()));
            entry.0 += 1;
            // Keep the lexicographically-smallest change_id as stable evidence.
            if a.change_id < entry.1 {
                entry.1 = a.change_id.clone();
            }
        }
    }

    invariant_candidates.sort_by(|x, y| x.change_id.cmp(&y.change_id));

    // BTreeMap iterates gates in sorted order → the ratify queue is sorted.
    let ratify_queue = per_gate
        .into_iter()
        .map(|(gate, (miss_count, change_id))| ThresholdProposal {
            gate,
            change_id,
            miss_count,
        })
        .collect();

    Feedback {
        invariant_candidates,
        ratify_queue,
    }
}

/// Map an overwatch [`ViolationSource`] to the gate name this loop uses in its
/// threshold-adjustment proposals. Kept as an explicit match (not a `Debug`
/// derive) so the wire name is stable and reviewed.
fn gate_name(source: ViolationSource) -> &'static str {
    match source {
        ViolationSource::Blastguard => "blastguard",
        ViolationSource::Propguard => "propguard",
        ViolationSource::Specguard => "specguard",
        ViolationSource::Mutategate => "mutategate",
        ViolationSource::Donegate => "donegate",
        ViolationSource::Reviewgate => "reviewgate",
        ViolationSource::Tdd => "tdd",
        ViolationSource::Budgetguard => "budgetguard",
        ViolationSource::Autoflow => "autoflow",
        ViolationSource::Ctxrot => "ctxrot",
    }
}

/// Derive audit verdicts from the **real** violation stream by cross-referencing
/// the auto-gate-passed population against recorded [`ViolationEvent`]s.
///
/// This is the heart of the *real* audit source. A change that passed all
/// auto-gates but later appears in the violation stream — matched by a stable
/// key: the change's [`GatePassedChange::change_id`] equals the violation's
/// [`ViolationEvent::task_key`] — is a **miss** the gates let through. For each
/// such match this emits an [`AuditResult`] with `miss = true`, attributing the
/// responsible `gate` (from the violation's [`ViolationSource`]) and an
/// `invariant_hint` (the violation's normalized `signature`, which names the
/// property/rule that was breached). Changes with **no** matching violation are
/// **not** misses and produce no verdict (the caller treats an absent verdict as
/// "clean").
///
/// When one change matches multiple violations (e.g. it later breached two
/// different gates) each match yields its own verdict, so every responsible gate
/// is fed back. Verdicts are returned sorted by `(change_id, gate, invariant)`
/// for a deterministic, diffable result.
///
/// Pure: a total function of its two inputs. No I/O, no clock, no env — the I/O
/// (reading the violation store) happens in [`execute_from_violations`].
pub fn derive_misses_from_violations(
    changes: &[GatePassedChange],
    violations: &[ViolationEvent],
) -> Vec<AuditResult> {
    use std::collections::BTreeSet;

    // The set of change ids that passed auto-gates — the only keys a violation
    // can be a "gate let it through" miss against.
    let passed: BTreeSet<&str> = changes.iter().map(|c| c.change_id.as_str()).collect();

    let mut out: Vec<AuditResult> = violations
        .iter()
        .filter(|v| passed.contains(v.task_key.as_str()))
        .map(|v| AuditResult {
            change_id: v.task_key.clone(),
            miss: true,
            gate: gate_name(v.source).to_string(),
            invariant_hint: v.signature.clone(),
        })
        .collect();

    // Deterministic, diffable order; also de-dupe identical (change,gate,sig)
    // matches so a repeated violation event doesn't double-count one miss.
    out.sort_by(|a, b| {
        (
            a.change_id.as_str(),
            a.gate.as_str(),
            a.invariant_hint.as_str(),
        )
            .cmp(&(
                b.change_id.as_str(),
                b.gate.as_str(),
                b.invariant_hint.as_str(),
            ))
    });
    out.dedup();
    out
}

/// Load a JSONL file of [`GatePassedChange`] rows (one per line), preserving
/// file order. Blank lines are skipped; a malformed line reports the file and
/// 1-based line number. Deterministic: no network, no clock, no env.
pub fn load_changes(path: impl AsRef<Path>) -> Result<Vec<GatePassedChange>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading gate-passed changes JSONL: {}", path.display()))?;
    parse_jsonl(&text, &path.display().to_string())
}

/// Load a JSONL file of [`AuditResult`] rows (one per line), preserving file
/// order. Same error shape as [`load_changes`]. Deterministic.
pub fn load_audits(path: impl AsRef<Path>) -> Result<Vec<AuditResult>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading audit results JSONL: {}", path.display()))?;
    parse_jsonl(&text, &path.display().to_string())
}

/// Parse JSONL text into a `Vec<T>` (the pure core, decoupled from the fs).
/// Mirrors [`crate::loader`]'s shape: blank lines skipped, 1-based line numbers.
fn parse_jsonl<T: for<'de> Deserialize<'de>>(text: &str, source: &str) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: T = serde_json::from_str(trimmed)
            .with_context(|| format!("malformed row at {}:{}", source, idx + 1))?;
        out.push(row);
    }
    Ok(out)
}

/// End-to-end calibration loop over on-disk inputs.
///
/// Reads the gate-passed population and (optionally) the audit verdicts, draws a
/// deterministic seeded sample, routes any misses through the two feedback
/// paths, and prints either a human summary or a `--json` report. Returns the
/// process exit code: `2` on any I/O / parse error, else `0`.
///
/// When `audits` is `None` the loop stops after sampling and prints the sample
/// (which changes a human/adversarial reviewer should audit next). When audits
/// are supplied, only the verdicts whose `change_id` is in the sample are routed
/// — the loop never acts on a change it did not actually sample.
pub fn execute(
    changes_path: &Path,
    audits_path: Option<&Path>,
    fraction: f64,
    seed: u64,
    json_out: bool,
) -> i32 {
    let population = match load_changes(changes_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("benchkit auditsample: {e:#}");
            return 2;
        }
    };
    let sampled = sample(&population, fraction, seed);

    // Route only the audit verdicts for changes we actually sampled.
    let feedback = match audits_path {
        None => None,
        Some(ap) => match load_audits(ap) {
            Ok(all) => {
                let in_sample: std::collections::BTreeSet<&str> =
                    sampled.iter().map(|c| c.change_id.as_str()).collect();
                let scoped: Vec<AuditResult> = all
                    .into_iter()
                    .filter(|a| in_sample.contains(a.change_id.as_str()))
                    .collect();
                Some(route_feedback(&scoped))
            }
            Err(e) => {
                eprintln!("benchkit auditsample: {e:#}");
                return 2;
            }
        },
    };

    if json_out {
        report_json(population.len(), &sampled, feedback.as_ref());
    } else {
        report_human(population.len(), &sampled, feedback.as_ref());
    }
    0
}

/// End-to-end calibration loop over the **real** violation stream.
///
/// Reads the gate-passed population from disk, draws the same deterministic
/// seeded sample as [`execute`], then — instead of loading a modeled audits
/// file — reads the *actual* recorded violations via
/// [`overwatch::store::read_violations`] and derives the misses by
/// cross-reference ([`derive_misses_from_violations`]). Only the derived
/// verdicts whose `change_id` is in the sample are routed through the two
/// feedback paths, exactly as the modeled path does. Returns the process exit
/// code: `2` on any I/O / parse error, else `0`.
///
/// This is what makes the audit real: the misses come from observed gate
/// violations (data the four gates emitted), not from a hand-authored file.
/// Nothing here mutates a gate config — the threshold proposals still land on
/// the ratify queue for a human. `cwd` locates the per-project violation store.
pub fn execute_from_violations(
    changes_path: &Path,
    cwd: &Path,
    fraction: f64,
    seed: u64,
    json_out: bool,
) -> i32 {
    let population = match load_changes(changes_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("benchkit auditsample: {e:#}");
            return 2;
        }
    };
    let sampled = sample(&population, fraction, seed);

    // The REAL audit source: cross-reference the population against the
    // observed violation stream. A gate-passed change that later shows up as a
    // violation is a miss the gates let through.
    let violations = match overwatch::store::read_violations(cwd) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("benchkit auditsample: reading violation stream: {e:#}");
            return 2;
        }
    };
    let derived = derive_misses_from_violations(&population, &violations);

    // Route only the verdicts for changes we actually sampled (same discipline
    // as the modeled path — never act on a change we did not sample).
    let in_sample: std::collections::BTreeSet<&str> =
        sampled.iter().map(|c| c.change_id.as_str()).collect();
    let scoped: Vec<AuditResult> = derived
        .into_iter()
        .filter(|a| in_sample.contains(a.change_id.as_str()))
        .collect();
    let feedback = Some(route_feedback(&scoped));

    if json_out {
        report_json(population.len(), &sampled, feedback.as_ref());
    } else {
        report_human(population.len(), &sampled, feedback.as_ref());
    }
    0
}

fn report_human(pop: usize, sampled: &[GatePassedChange], feedback: Option<&Feedback>) {
    println!(
        "benchkit auditsample: sampled {} of {} auto-gate-passed changes",
        sampled.len(),
        pop
    );
    for c in sampled {
        println!("  ~ {} (gates: {})", c.change_id, c.gates.join(","));
    }
    match feedback {
        None => {
            println!("\n  (no audit results supplied — sample only; run a stricter audit next)");
        }
        Some(fb) => {
            println!(
                "\n  (a) {} new-invariant candidate(s) for propguard/specguard:",
                fb.invariant_candidates.len()
            );
            for c in &fb.invariant_candidates {
                println!("     + {} :: {}", c.change_id, c.invariant);
            }
            println!(
                "  (b) {} threshold-adjustment proposal(s) — RATIFY QUEUE (never auto-applied):",
                fb.ratify_queue.len()
            );
            for p in &fb.ratify_queue {
                println!(
                    "     ? {} (×{} miss, evidence {})",
                    p.gate, p.miss_count, p.change_id
                );
            }
        }
    }
}

fn report_json(pop: usize, sampled: &[GatePassedChange], feedback: Option<&Feedback>) {
    let sampled_ids: Vec<&str> = sampled.iter().map(|c| c.change_id.as_str()).collect();
    println!(
        "{}",
        json!({
            "population": pop,
            "sampled": sampled_ids,
            "invariant_candidates": feedback.map(|f| &f.invariant_candidates),
            "ratify_queue": feedback.map(|f| &f.ratify_queue),
        })
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(id: &str, gates: &[&str]) -> GatePassedChange {
        GatePassedChange {
            change_id: id.to_string(),
            gates: gates.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn population(n: usize) -> Vec<GatePassedChange> {
        (0..n)
            .map(|i| change(&format!("c{i:03}"), &["propguard"]))
            .collect()
    }

    #[test]
    fn sample_size_is_rounded_fraction_clamped() {
        let pop = population(10);
        assert_eq!(sample(&pop, 0.0, 1).len(), 0);
        assert_eq!(sample(&pop, 0.25, 1).len(), 3); // 2.5 → round-half-up 3
        assert_eq!(sample(&pop, 0.5, 1).len(), 5);
        assert_eq!(sample(&pop, 1.0, 1).len(), 10);
        // Out-of-range fractions clamp, never panic.
        assert_eq!(sample(&pop, 2.0, 1).len(), 10);
        assert_eq!(sample(&pop, -1.0, 1).len(), 0);
    }

    #[test]
    fn sample_is_deterministic_for_a_seed() {
        let pop = population(50);
        let a = sample(&pop, 0.2, 42);
        let b = sample(&pop, 0.2, 42);
        assert_eq!(a, b, "same seed must yield the identical sample");
        assert_eq!(a.len(), 10);
    }

    #[test]
    fn different_seeds_generally_differ() {
        let pop = population(50);
        let a = sample(&pop, 0.2, 1);
        let b = sample(&pop, 0.2, 2);
        // Not a hard guarantee for every seed pair, but for these it holds and
        // documents that the seed actually steers selection.
        assert_ne!(a, b);
    }

    #[test]
    fn sample_is_independent_of_input_order() {
        let pop = population(20);
        let mut shuffled = pop.clone();
        shuffled.reverse();
        assert_eq!(
            sample(&pop, 0.3, 7),
            sample(&shuffled, 0.3, 7),
            "sampling must not depend on input line order"
        );
    }

    #[test]
    fn empty_population_yields_empty_sample_no_panic() {
        let empty: Vec<GatePassedChange> = Vec::new();
        assert!(sample(&empty, 0.5, 1).is_empty());
    }

    #[test]
    fn sample_output_is_sorted_by_change_id() {
        let pop = population(30);
        let s = sample(&pop, 0.5, 99);
        let mut sorted = s.clone();
        sorted.sort_by(|a, b| a.change_id.cmp(&b.change_id));
        assert_eq!(s, sorted);
    }

    fn audit(id: &str, miss: bool, gate: &str, hint: &str) -> AuditResult {
        AuditResult {
            change_id: id.to_string(),
            miss,
            gate: gate.to_string(),
            invariant_hint: hint.to_string(),
        }
    }

    #[test]
    fn no_misses_yields_empty_feedback() {
        let audits = vec![audit("c1", false, "", ""), audit("c2", false, "", "")];
        let fb = route_feedback(&audits);
        assert!(fb.invariant_candidates.is_empty());
        assert!(fb.ratify_queue.is_empty());
    }

    #[test]
    fn a_miss_emits_both_feedback_paths() {
        let audits = vec![audit("c5", true, "mutategate", "kill_ratio_holds")];
        let fb = route_feedback(&audits);
        assert_eq!(
            fb.invariant_candidates,
            vec![InvariantCandidate {
                change_id: "c5".to_string(),
                invariant: "kill_ratio_holds".to_string(),
            }]
        );
        assert_eq!(
            fb.ratify_queue,
            vec![ThresholdProposal {
                gate: "mutategate".to_string(),
                change_id: "c5".to_string(),
                miss_count: 1,
            }]
        );
    }

    #[test]
    fn threshold_proposals_aggregate_per_gate() {
        let audits = vec![
            audit("c3", true, "mutategate", "h1"),
            audit("c1", true, "mutategate", "h2"),
            audit("c9", true, "specguard", "h3"),
        ];
        let fb = route_feedback(&audits);
        // Two mutategate misses aggregate into one proposal with count 2, citing
        // the lexicographically-smallest change_id ("c1") as evidence.
        assert_eq!(
            fb.ratify_queue,
            vec![
                ThresholdProposal {
                    gate: "mutategate".to_string(),
                    change_id: "c1".to_string(),
                    miss_count: 2,
                },
                ThresholdProposal {
                    gate: "specguard".to_string(),
                    change_id: "c9".to_string(),
                    miss_count: 1,
                },
            ]
        );
        // Three misses → three invariant candidates, sorted by change_id.
        let ids: Vec<&str> = fb
            .invariant_candidates
            .iter()
            .map(|c| c.change_id.as_str())
            .collect();
        assert_eq!(ids, vec!["c1", "c3", "c9"]);
    }

    #[test]
    fn miss_without_hint_or_gate_is_partially_routed() {
        // A miss that names neither a hint nor a gate contributes nothing (it
        // cannot seed either path) — no empty-string proposals leak through.
        let audits = vec![audit("c1", true, "", "")];
        let fb = route_feedback(&audits);
        assert!(fb.invariant_candidates.is_empty());
        assert!(fb.ratify_queue.is_empty());
    }

    #[test]
    fn ratify_queue_never_auto_applies() {
        // The ratify queue is a *proposal* surface: nothing in this crate may
        // apply a threshold, mutate a gate config, or otherwise turn a proposal
        // into an effect. Idempotence alone (the old assertion) is vacuous — it
        // would stay green even if an auto-apply path were bolted on. So this
        // test proves the safety property *structurally*: it scans this module's
        // own source for any code path that would write back to a gate config or
        // mutate a threshold, and fails if one exists. If someone later adds an
        // `apply_threshold(...)` / config-write call, THIS test goes red.
        let src = include_str!("auditsample.rs");
        // Strip the test module so our own assertion strings don't self-trip.
        let non_test = src
            .split("#[cfg(test)]")
            .next()
            .expect("source has a non-test prefix");

        // Any of these tokens appearing in the non-test code would indicate an
        // apply / mutation path. Their ABSENCE is the guarantee.
        let forbidden = [
            "fn apply",
            "apply_threshold",
            "set_threshold",
            "write_config",
            "fs::write",
            "OpenOptions",
            "save_",
        ];
        for needle in forbidden {
            assert!(
                !non_test.contains(needle),
                "ratify queue must never auto-apply: found forbidden mutation token {needle:?} \
                 in auditsample non-test code — the ratify queue must stay a human-only step"
            );
        }

        // And the positive behavioural guarantee: routing only ever produces
        // data (proposals), and doing it twice is side-effect free.
        let audits = vec![audit("c1", true, "blastguard", "reversible")];
        let a = route_feedback(&audits);
        let b = route_feedback(&audits);
        assert_eq!(a.ratify_queue, b.ratify_queue);
        assert_eq!(a.ratify_queue.len(), 1);
    }

    fn violation(source: ViolationSource, sig: &str, task_key: &str) -> ViolationEvent {
        ViolationEvent {
            source,
            signature: sig.to_string(),
            task_key: task_key.to_string(),
            session_id: "s1".to_string(),
            ts: 1000,
            detail: None,
        }
    }

    #[test]
    fn real_audit_cross_reference_match_is_miss_nonmatch_ignored() {
        // The REAL audit source: a gate-passed change whose change_id matches a
        // recorded violation's task_key is a miss the gates let through; a
        // gate-passed change with NO matching violation is not a miss.
        let changes = vec![
            change("c001", &["mutategate"]),
            change("c002", &["propguard"]),
            change("c003", &["blastguard"]),
        ];
        let violations = vec![
            // Matches c001 -> a miss, attributed to mutategate.
            violation(
                ViolationSource::Mutategate,
                "mutategate:arithmetic-op-swap",
                "c001",
            ),
            // Matches c003 -> a miss, attributed to blastguard.
            violation(ViolationSource::Blastguard, "blastguard:rm-rf", "c003"),
            // task_key "c999" is NOT in the gate-passed population -> ignored.
            violation(ViolationSource::Specguard, "specguard:drift:x", "c999"),
        ];

        let derived = derive_misses_from_violations(&changes, &violations);

        // Exactly the two matching changes become misses; c002 (no violation)
        // and c999 (not gate-passed) contribute nothing.
        assert_eq!(derived.len(), 2);
        assert!(derived.iter().all(|a| a.miss));
        assert_eq!(
            derived,
            vec![
                AuditResult {
                    change_id: "c001".to_string(),
                    miss: true,
                    gate: "mutategate".to_string(),
                    invariant_hint: "mutategate:arithmetic-op-swap".to_string(),
                },
                AuditResult {
                    change_id: "c003".to_string(),
                    miss: true,
                    gate: "blastguard".to_string(),
                    invariant_hint: "blastguard:rm-rf".to_string(),
                },
            ]
        );

        // And the derived misses feed BOTH feedback paths.
        let fb = route_feedback(&derived);
        assert_eq!(fb.invariant_candidates.len(), 2);
        assert_eq!(fb.ratify_queue.len(), 2); // one per responsible gate
    }

    #[test]
    fn real_audit_no_matching_violation_yields_no_misses() {
        let changes = vec![
            change("c001", &["mutategate"]),
            change("c002", &["propguard"]),
        ];
        // Violation for a task that never passed the gates here -> not a miss.
        let violations = vec![violation(
            ViolationSource::Propguard,
            "propguard:prop-003",
            "unrelated-task",
        )];
        let derived = derive_misses_from_violations(&changes, &violations);
        assert!(derived.is_empty());
        let fb = route_feedback(&derived);
        assert!(fb.invariant_candidates.is_empty());
        assert!(fb.ratify_queue.is_empty());
    }

    #[test]
    fn real_audit_dedupes_repeated_violation_for_same_change() {
        // The same (change, gate, signature) recorded twice must not
        // double-count into two misses.
        let changes = vec![change("c001", &["mutategate"])];
        let violations = vec![
            violation(ViolationSource::Mutategate, "mutategate:op", "c001"),
            violation(ViolationSource::Mutategate, "mutategate:op", "c001"),
        ];
        let derived = derive_misses_from_violations(&changes, &violations);
        assert_eq!(derived.len(), 1);
    }

    #[test]
    fn execute_from_violations_reads_real_store_and_routes() {
        // End-to-end over the real path: seed the overwatch violation store for
        // a temp project, then confirm execute_from_violations reads it, derives
        // a miss by cross-reference, and returns 0.
        let cwd = tempfile::tempdir().unwrap();
        // A gate-passed change that we will also record a violation against.
        let changes_path = cwd.path().join("changes.jsonl");
        std::fs::write(
            &changes_path,
            "{\"change_id\":\"c001\",\"gates\":[\"mutategate\"]}\n",
        )
        .unwrap();

        // Record a REAL violation whose task_key matches the change.
        let ev = violation(ViolationSource::Mutategate, "mutategate:op-swap", "c001");
        overwatch::store::append_violation(cwd.path(), &ev).unwrap();

        // Sanity: the store now has the event (proves we read real data).
        let read_back = overwatch::store::read_violations(cwd.path()).unwrap();
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].task_key, "c001");

        // fraction 1.0 samples the whole population, so the miss is in-sample.
        assert_eq!(
            execute_from_violations(&changes_path, cwd.path(), 1.0, 0, true),
            0
        );
    }

    #[test]
    fn jsonl_loaders_skip_blanks_and_report_line_numbers() {
        let changes =
            "\n{\"change_id\":\"c1\",\"gates\":[\"propguard\"]}\n\n{\"change_id\":\"c2\"}\n";
        let got: Vec<GatePassedChange> = parse_jsonl(changes, "inline").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].change_id, "c1");
        assert!(got[1].gates.is_empty()); // gates defaults to empty

        let bad = "{\"change_id\":\"ok\"}\nnot json\n";
        let err = parse_jsonl::<GatePassedChange>(bad, "inline").unwrap_err();
        assert!(err.to_string().contains("inline:2"), "got: {err}");
    }

    #[test]
    fn execute_end_to_end_routes_only_sampled_misses() {
        let dir = tempfile::tempdir().unwrap();
        let changes = dir.path().join("changes.jsonl");
        let audits = dir.path().join("audits.jsonl");

        // 4-change population; with fraction 0.5 seed 1 we get a fixed 2-change
        // sample. We audit ALL four but only the sampled ones may feed back.
        let mut body = String::new();
        for id in ["c000", "c001", "c002", "c003"] {
            body.push_str(&format!(
                "{{\"change_id\":\"{id}\",\"gates\":[\"mutategate\"]}}\n"
            ));
        }
        std::fs::write(&changes, body).unwrap();

        // Determine the sample deterministically to assert the scoping is real.
        let pop = load_changes(&changes).unwrap();
        let sampled = sample(&pop, 0.5, 1);
        assert_eq!(sampled.len(), 2);

        // Every change is a miss in the audit file.
        let mut abody = String::new();
        for id in ["c000", "c001", "c002", "c003"] {
            abody.push_str(&format!(
                "{{\"change_id\":\"{id}\",\"miss\":true,\"gate\":\"mutategate\",\"invariant_hint\":\"h\"}}\n"
            ));
        }
        std::fs::write(&audits, abody).unwrap();

        // Route only the sampled subset by hand and confirm the count matches.
        let in_sample: std::collections::BTreeSet<&str> =
            sampled.iter().map(|c| c.change_id.as_str()).collect();
        let all_audits = load_audits(&audits).unwrap();
        let scoped: Vec<AuditResult> = all_audits
            .into_iter()
            .filter(|a| in_sample.contains(a.change_id.as_str()))
            .collect();
        let fb = route_feedback(&scoped);
        // Exactly the sampled misses (2) feed back, not all four.
        assert_eq!(fb.invariant_candidates.len(), 2);
        assert_eq!(fb.ratify_queue.len(), 1); // one gate, aggregated
        assert_eq!(fb.ratify_queue[0].miss_count, 2);

        // The public execute path returns 0 on well-formed input (both modes).
        assert_eq!(execute(&changes, Some(&audits), 0.5, 1, true), 0);
        assert_eq!(execute(&changes, None, 0.5, 1, false), 0);
    }

    #[test]
    fn execute_unreadable_input_is_exit_2() {
        let missing = Path::new("/no/such/benchkit/changes.jsonl");
        assert_eq!(execute(missing, None, 0.5, 1, false), 2);
    }
}
