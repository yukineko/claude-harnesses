//! The routing policy: turn a task + its neighbours into a worker/verifier model
//! choice. Core rule = **cheapest tier that historically clears the bar**; fall
//! back to a keyword prior on cold start. This is the deterministic stand-in for
//! fugu's trained coordinator — retrieval over outcomes instead of weight updates.

use std::collections::BTreeMap;

use crate::rag::Neighbor;
use crate::rng::Rng;

/// Cost order of the model tiers condukt understands. Index = cheapest-first.
const TIERS: &[&str] = &["haiku", "sonnet", "opus"];

/// Exploration bonus the bandit adds to a tier's sampled pass-rate, scaled by how
/// cheap the tier is (cheapest tier gets the full bonus, the priciest gets none).
/// This nudges Thompson sampling to try the cheaper tiers more often before their
/// posteriors have tightened — the cheap tiers are exactly the ones worth
/// exploring, since a pass there is a permanent cost saving. Kept small so a tier
/// with a genuinely poor record still loses; it only tips near-ties cheapward.
const EXPLORE_CHEAP_BONUS: f64 = 0.15;

/// Cheap-tier exploration bonus for `TIERS[idx]`: full bonus at the cheapest tier
/// (idx 0), linearly down to 0 at the priciest. opus never gets a bonus, so this
/// only ever biases sampling *toward* cheaper models, never toward opus.
fn cheap_bonus(idx: usize) -> f64 {
    let last = TIERS.len() - 1;
    if last == 0 {
        return 0.0;
    }
    EXPLORE_CHEAP_BONUS * (last - idx) as f64 / last as f64
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub worker_model: String,
    pub verifier_model: String,
    /// "learned" | "prior" | "gated"
    pub basis: &'static str,
    /// "high" | "low"
    pub confidence: &'static str,
    pub neighbors: usize,
    pub rationale: String,
}

const DESIGN_KW: &[&str] = &[
    "design",
    "architecture",
    "refactor",
    "migrate",
    "migration",
    "schema",
    "security",
    "auth",
    "concurren",
    "protocol",
    "algorithm",
    "crypto",
    "race",
    "performance",
];
const TRIVIAL_KW: &[&str] = &[
    "rename",
    "typo",
    "format",
    "comment",
    "doc",
    "docs",
    "readme",
    "whitespace",
    "lint",
    "bump",
    "copy",
    "label",
    "wording",
];

/// Cold-start model guess from task text + file count (mirrors the interpreter's
/// own rule: design→opus, trivial→cheap, else cheapest tier the blast radius
/// allows).
///
/// The cold start is deliberately biased **cheap** — with no history to learn
/// from, start at the floor and let the verifier's cascade escalation
/// (haiku→sonnet→opus on a failed check) buy up only the tasks that actually
/// need it. Over-charging every unseen task at sonnet/opus spends real money on
/// work that would have cleared at haiku.
///
/// Order matters: triviality is judged **before** the file-count rules.
/// Otherwise a many-file mechanical chore (a repo-wide `rename`, a formatting
/// sweep) would short-circuit to a pricier tier on volume alone — the cold start
/// over-charges exactly the cheap work, before any history exists to correct it.
/// Design/high-stakes keywords still win outright.
pub fn prior_model(title: &str, file_count: usize) -> &'static str {
    let t = title.to_lowercase();

    // Design / high-stakes work warrants the strong tier even at one file.
    if DESIGN_KW.iter().any(|k| t.contains(k)) {
        return "opus";
    }
    // A mechanical chore stays cheap across many files — the *kind* of work is
    // trivial, so volume alone shouldn't force opus. A large sweep still gets
    // sonnet (more surface = more chance of a slip); a small one gets haiku.
    if TRIVIAL_KW.iter().any(|k| t.contains(k)) {
        return if file_count > 5 { "sonnet" } else { "haiku" };
    }
    // Non-trivial, non-design: blast radius is the only complexity signal we
    // have at cold start, so scale the tier to it rather than defaulting pricey.
    // A very wide change (>10 files) still earns opus; a medium spread (6–10)
    // gets sonnet; an ordinary small change starts at the haiku floor and only
    // escalates if the verifier rejects it.
    if file_count > 10 {
        return "opus";
    }
    if file_count > 5 {
        return "sonnet";
    }
    "haiku"
}

/// Independent verifier: a **different** model from the worker so it doesn't
/// share its blind spots. High-stakes work (serial / design) escalates the
/// verifier; low-stakes work takes the cheapest independent tier.
pub fn verifier_model(worker: &str, class: &str, title: &str) -> &'static str {
    let t = title.to_lowercase();
    let high_stakes = class == "serial" || DESIGN_KW.iter().any(|k| t.contains(k));
    match worker {
        "opus" => "sonnet", // strong worker → independent (cheaper) second pair of eyes
        _ => {
            if high_stakes {
                // Serial / design work still gets a strong independent verifier.
                "opus"
            } else if worker == "haiku" {
                // Low-stakes, but the worker is already at the floor — verify one
                // tier up so the check stays independent (can't verify haiku with
                // haiku).
                "sonnet"
            } else {
                // Low-stakes sonnet worker → a cheap haiku verifier is enough of an
                // independent second pass; save the sonnet spend for the cases that
                // need it.
                "haiku"
            }
        }
    }
}

/// Drop a model one tier toward the cheapest. haiku is the floor.
///
/// `pub(crate)` (not private) so `mode::apply` can reuse the exact same clamp
/// `downgrade_for_budget` uses for its `fast`-mode worker shift, instead of
/// re-deriving the tier table.
pub(crate) fn one_tier_down(model: &str) -> &'static str {
    match model {
        "opus" => "sonnet",
        "sonnet" => "haiku",
        _ => "haiku",
    }
}

/// Raise a model one tier toward the priciest. opus is the ceiling.
/// Sibling of [`one_tier_down`], for `mode::apply`'s `high`-mode worker shift.
pub(crate) fn one_tier_up(model: &str) -> &'static str {
    match model {
        "haiku" => "sonnet",
        _ => "opus", // sonnet/opus/unknown -> opus (opus is already the ceiling)
    }
}

/// Under daily budget pressure (budgetguard reports the warn threshold reached),
/// bias the decision cheaper: shave the worker one tier and cap the verifier at
/// sonnet (suppress opus escalation). Deterministic and explicitly recorded in
/// the rationale. A no-op for `gated` tasks (human-approved, never auto-routed)
/// and when nothing can be lowered (worker already haiku, verifier not opus).
pub fn downgrade_for_budget(d: Decision) -> Decision {
    if d.basis == "gated" {
        return d;
    }
    let new_worker = one_tier_down(&d.worker_model);
    // Never let the independent verifier sit at opus while we're saving money.
    let new_verifier = if d.verifier_model == "opus" {
        "sonnet"
    } else {
        d.verifier_model.as_str()
    };
    if d.worker_model == new_worker && d.verifier_model == new_verifier {
        return d; // already at the floor — nothing to downgrade
    }
    // Record only the tiers that actually moved (the worker may already be at
    // the haiku floor while the verifier still gets capped).
    let mut changes = Vec::new();
    if d.worker_model != new_worker {
        changes.push(format!("worker {}→{new_worker}", d.worker_model));
    }
    if d.verifier_model != new_verifier {
        changes.push(format!("verifier {}→{new_verifier}", d.verifier_model));
    }
    let rationale = format!("{} | budget pressure: {}", d.rationale, changes.join(", "));
    Decision {
        worker_model: new_worker.to_string(),
        verifier_model: new_verifier.to_string(),
        basis: d.basis,
        confidence: d.confidence,
        neighbors: d.neighbors,
        rationale,
    }
}

struct ModelStat {
    count: usize,
    passes: usize,
    total_cost_usd: f64,
}

fn aggregate(neighbors: &[Neighbor]) -> BTreeMap<String, ModelStat> {
    let mut m: BTreeMap<String, ModelStat> = BTreeMap::new();
    for n in neighbors {
        let s = m.entry(n.ep.model.clone()).or_insert(ModelStat {
            count: 0,
            passes: 0,
            total_cost_usd: 0.0,
        });
        s.count += 1;
        s.total_cost_usd += n.ep.cost_usd;
        // Learn from the human label when present, else the verifier's self-pass.
        if n.ep.effective_pass() {
            s.passes += 1;
        }
    }
    m
}

pub fn decide(
    title: &str,
    files: &[String],
    class: &str,
    neighbors: &[Neighbor],
    pass_threshold: f64,
    min_samples: usize,
) -> Decision {
    if class == "gated" {
        return Decision {
            worker_model: "opus".into(),
            verifier_model: "opus".into(),
            basis: "gated",
            confidence: "high",
            neighbors: 0,
            rationale: "gated task — human approval required; not auto-routed".into(),
        };
    }

    let stats = aggregate(neighbors);
    // Among tiers that clear the pass threshold with enough samples, prefer the
    // one with the lowest cost-per-success (total_cost_usd / passes). When cost
    // data is absent (all zeros) this degenerates to the original cheapest-first
    // ordering (TIERS is haiku→sonnet→opus, so the first qualifying tier wins).
    let mut chosen: Option<(String, f64, f64)> = None; // (model, pass_rate, cost_per_pass)
    for tier in TIERS {
        if let Some(s) = stats.get(*tier) {
            if s.count >= min_samples {
                let rate = s.passes as f64 / s.count as f64;
                if rate >= pass_threshold {
                    let cpp = if s.passes > 0 {
                        s.total_cost_usd / s.passes as f64
                    } else {
                        f64::INFINITY
                    };
                    // Replace if this tier has a lower cost/pass (or first qualifying).
                    if chosen
                        .as_ref()
                        .is_none_or(|(_, _, prev_cpp)| cpp < *prev_cpp)
                    {
                        chosen = Some((tier.to_string(), rate, cpp));
                    }
                }
            }
        }
    }

    let (worker, basis, confidence, rationale) = match chosen {
        Some((m, rate, cpp)) => (
            m.clone(),
            "learned",
            "high",
            format!(
                "{} similar task(s): {} passed {:.0}% cost/pass=${:.4} → cheapest qualifying tier",
                neighbors.len(),
                m,
                rate * 100.0,
                cpp,
            ),
        ),
        None => {
            let p = prior_model(title, files.len());
            let why = if neighbors.is_empty() {
                "no similar history".to_string()
            } else {
                format!(
                    "{} neighbour(s) but none cleared {:.0}% with >={} samples",
                    neighbors.len(),
                    pass_threshold * 100.0,
                    min_samples
                )
            };
            (
                p.to_string(),
                "prior",
                "low",
                format!("{why} -> heuristic prior: {p}"),
            )
        }
    };

    let verifier = verifier_model(&worker, class, title).to_string();
    Decision {
        worker_model: worker,
        verifier_model: verifier,
        basis,
        confidence,
        neighbors: neighbors.len(),
        rationale,
    }
}

/// Thompson-sampling variant of [`decide`]. Same "cheapest tier that clears the
/// bar" goal, but instead of a hard pass-rate test it draws each tier's
/// pass-probability from a Beta(1+passes, 1+fails) posterior (Gaussian-
/// approximated) and takes the cheapest tier whose *sample* clears the bar. Wide
/// posteriors on rarely-tried cheap tiers get explored; as evidence accrues they
/// converge to exploitation. This is the online-learning the threshold rule lacks.
pub fn decide_bandit(
    title: &str,
    files: &[String],
    class: &str,
    neighbors: &[Neighbor],
    pass_threshold: f64,
    min_samples: usize,
    rng: &mut Rng,
) -> Decision {
    if class == "gated" {
        return Decision {
            worker_model: "opus".into(),
            verifier_model: "opus".into(),
            basis: "gated",
            confidence: "high",
            neighbors: 0,
            rationale: "gated task — human approval required; not auto-routed".into(),
        };
    }

    let stats = aggregate(neighbors);
    let posterior = |tier: &str| -> (f64, f64, usize) {
        let (count, passes) = stats
            .get(tier)
            .map(|s| (s.count, s.passes))
            .unwrap_or((0, 0));
        let a = 1.0 + passes as f64;
        let b = 1.0 + (count - passes) as f64;
        let mean = a / (a + b);
        let var = (a * b) / ((a + b) * (a + b) * (a + b + 1.0));
        (mean, var.sqrt(), count)
    };

    // Cost-efficiency posterior sampling: sample a pass rate from each tier's
    // Beta posterior, then adjust by the tier's cost-per-pass relative to the
    // cheapest observed tier. A tier 4× more expensive per pass must sample 4×
    // higher to win. When no cost data is recorded (all zeros), the adjustment
    // is 1.0 everywhere and this degenerates to cheapest-first sampling.
    let cpps: Vec<f64> = TIERS
        .iter()
        .filter_map(|t| stats.get(*t))
        .filter(|s| s.passes > 0 && s.total_cost_usd > 0.0)
        .map(|s| s.total_cost_usd / s.passes as f64)
        .collect();
    let min_cpp = cpps.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_cpp = cpps.iter().cloned().fold(0.0f64, f64::max);
    let min_cpp = if min_cpp.is_finite() { min_cpp } else { 0.0 };
    // Untested tiers default to the most expensive observed cpp so they don't
    // appear "free" and incorrectly beat known-cheap tiers on random samples.
    let default_cpp = if max_cpp > 0.0 { max_cpp } else { 0.0 };

    let mut chosen: Option<(String, f64, usize, f64)> = None; // (tier, mean, count, efficiency)
    for (idx, tier) in TIERS.iter().enumerate() {
        let (mean, sd, count) = posterior(tier);
        // Draw a pass-rate, then add a cheap-tier exploration bonus so the cheaper
        // tiers clear the gate (and score) a little more readily. opus gets no
        // bonus, so this only ever tips the choice cheapward.
        let sample = (rng.normal(mean, sd).clamp(0.0, 1.0) + cheap_bonus(idx)).min(1.0);
        // Only consider tiers that sampled above the threshold (exploration gate).
        if sample < pass_threshold {
            continue;
        }
        if min_cpp > 0.0 {
            // Cost data available — score by efficiency; pick the best across all
            // qualifying tiers (not just cheapest-first).
            let cpp = stats
                .get(*tier)
                .and_then(|s| {
                    if s.passes > 0 && s.total_cost_usd > 0.0 {
                        Some(s.total_cost_usd / s.passes as f64)
                    } else {
                        None
                    }
                })
                .unwrap_or(default_cpp); // untested → pessimistic max-cpp prior
            let efficiency = if cpp > 0.0 {
                sample / (cpp / min_cpp)
            } else {
                sample
            };
            if chosen
                .as_ref()
                .is_none_or(|(_, _, _, best_eff)| efficiency > *best_eff)
            {
                chosen = Some((tier.to_string(), mean, count, efficiency));
            }
        } else {
            // No cost data at all — cheapest-first (original behaviour): take the
            // first qualifying tier and stop so haiku is always preferred over sonnet
            // when both sample above the bar (preserves exploration tests).
            chosen = Some((tier.to_string(), mean, count, sample));
            break;
        }
    }

    let (worker, basis, confidence, rationale) = match chosen {
        Some((m, mean, count, eff)) => (
            m.clone(),
            "bandit",
            if count >= min_samples { "high" } else { "low" },
            format!(
                "Thompson(cost-adj): {m} cleared {:.0}% (posterior mean {:.0}%, efficiency {:.3}, {count} sample(s))",
                pass_threshold * 100.0,
                mean * 100.0,
                eff,
            ),
        ),
        None => {
            // No tier sampled the bar — exploit the best posterior mean we have,
            // else fall back to the cold-start prior.
            let best = TIERS
                .iter()
                .filter_map(|t| stats.get(*t).map(|_| (*t, posterior(t).0)))
                .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal));
            match best {
                Some((t, mean)) => (
                    t.to_string(),
                    "bandit",
                    "low",
                    format!(
                        "no tier sampled the bar → exploit best mean {:.0}% ({t})",
                        mean * 100.0
                    ),
                ),
                None => {
                    let p = prior_model(title, files.len());
                    (
                        p.to_string(),
                        "prior",
                        "low",
                        format!("no history → prior {p}"),
                    )
                }
            }
        }
    };

    let verifier = verifier_model(&worker, class, title).to_string();
    Decision {
        worker_model: worker,
        verifier_model: verifier,
        basis,
        confidence,
        neighbors: neighbors.len(),
        rationale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Episode;

    fn nb(model: &str, pass: bool) -> Neighbor {
        Neighbor {
            ep: Episode {
                ts: 0,
                title: "x".into(),
                touched_files: vec![],
                class: "parallel".into(),
                model: model.into(),
                role: "worker".into(),
                pass,
                cost_usd: 0.0,
                human_label: None,
                labeled_by: None,
                skill_fingerprint: None,
                duration_secs: 0.0,
                delegation: None,
                ..Default::default()
            },
            sim: 0.5,
        }
    }

    #[test]
    fn human_label_overrides_pass_in_aggregate() {
        // Two sonnet neighbours that the verifier passed; a human labels one bad.
        let mut bad = nb("sonnet", true);
        bad.ep.human_label = Some(false);
        let good = nb("sonnet", true);
        let stats = aggregate(&[bad, good]);
        let s = stats.get("sonnet").unwrap();
        assert_eq!(s.count, 2);
        // only the human-good (unlabeled→self-pass) one counts as a pass.
        assert_eq!(s.passes, 1);
    }

    #[test]
    fn human_good_label_rescues_a_failed_episode() {
        // verifier failed it, but a human says good → counts as a pass.
        let mut rescued = nb("haiku", false);
        rescued.ep.human_label = Some(true);
        let stats = aggregate(&[rescued]);
        assert_eq!(stats.get("haiku").unwrap().passes, 1);
    }

    #[test]
    fn verifier_is_cheap_for_low_stakes_but_stays_independent() {
        // Low-stakes sonnet worker → a cheap haiku verifier is enough.
        assert_eq!(verifier_model("sonnet", "parallel", "add a field"), "haiku");
        // Low-stakes haiku worker → can't verify haiku with haiku; step up to
        // sonnet to keep the check independent.
        assert_eq!(
            verifier_model("haiku", "parallel", "rename a field"),
            "sonnet"
        );
        // High-stakes (serial) low-tier worker → strong opus verifier, unchanged.
        assert_eq!(
            verifier_model("sonnet", "serial", "wire the endpoint"),
            "opus"
        );
        // Design keyword is high-stakes even when parallel → opus verifier.
        assert_eq!(
            verifier_model("sonnet", "parallel", "auth token check"),
            "opus"
        );
        // Opus worker → independent (cheaper) sonnet second pair of eyes.
        assert_eq!(
            verifier_model("opus", "serial", "redesign schema"),
            "sonnet"
        );
    }

    #[test]
    fn cheap_bonus_is_largest_for_cheapest_tier_and_zero_for_opus() {
        // haiku (idx 0) gets the full bonus, opus (last idx) none, so the bandit
        // bias can only ever tip the choice toward cheaper models.
        assert_eq!(cheap_bonus(0), EXPLORE_CHEAP_BONUS);
        assert!(cheap_bonus(1) > 0.0 && cheap_bonus(1) < EXPLORE_CHEAP_BONUS);
        assert_eq!(cheap_bonus(TIERS.len() - 1), 0.0);
    }

    #[test]
    fn gated_is_not_auto_routed() {
        let d = decide("deploy to prod", &[], "gated", &[], 0.7, 2);
        assert_eq!(d.basis, "gated");
    }

    #[test]
    fn cold_start_uses_keyword_prior() {
        let d = decide("redesign auth architecture", &[], "serial", &[], 0.7, 2);
        assert_eq!(d.worker_model, "opus");
        assert_eq!(d.basis, "prior");
        // worker is already opus → verify independently with a cheaper sonnet.
        assert_eq!(d.verifier_model, "sonnet");

        let d2 = decide("rename a variable", &[], "parallel", &[], 0.7, 2);
        assert_eq!(d2.worker_model, "haiku");
    }

    #[test]
    fn multi_file_trivial_drops_below_opus() {
        // A many-file mechanical chore must NOT be billed at opus on volume
        // alone (the bug): a wide trivial sweep gets sonnet, a small one haiku.
        assert_eq!(prior_model("rename a symbol across the repo", 20), "sonnet");
        assert_eq!(prior_model("reformat the whole tree", 30), "sonnet");
        assert_eq!(prior_model("fix a typo", 1), "haiku");
    }

    #[test]
    fn budget_downgrade_shaves_worker_and_caps_verifier() {
        // opus worker / sonnet verifier under pressure → sonnet / sonnet.
        let d = decide("redesign auth architecture", &[], "serial", &[], 0.7, 2);
        assert_eq!(d.worker_model, "opus");
        assert_eq!(d.verifier_model, "sonnet");
        let dg = downgrade_for_budget(d);
        assert_eq!(dg.worker_model, "sonnet");
        assert_eq!(dg.verifier_model, "sonnet");
        assert!(dg.rationale.contains("budget pressure"));
    }

    #[test]
    fn budget_downgrade_suppresses_opus_verifier() {
        // sonnet worker + high-stakes (serial) → opus verifier; pressure caps it
        // at sonnet and shaves the worker to haiku. ("endpoint" hits no DESIGN_KW;
        // a 6–10 file spread lands the cold-start prior on sonnet, not haiku.)
        let files: Vec<String> = (0..6).map(|i| format!("f{i}.rs")).collect();
        let d = decide("implement the endpoint", &files, "serial", &[], 0.7, 2);
        assert_eq!(d.worker_model, "sonnet");
        assert_eq!(d.verifier_model, "opus");
        let dg = downgrade_for_budget(d);
        assert_eq!(dg.worker_model, "haiku");
        assert_eq!(dg.verifier_model, "sonnet");
        assert!(dg.rationale.contains("verifier opus→sonnet"));
    }

    #[test]
    fn budget_downgrade_is_noop_at_floor_and_for_gated() {
        // haiku worker + sonnet verifier: already the floor → unchanged.
        let floor = decide("rename a variable", &[], "parallel", &[], 0.7, 2);
        assert_eq!(floor.worker_model, "haiku");
        assert_eq!(floor.verifier_model, "sonnet");
        let dg = downgrade_for_budget(floor);
        assert_eq!(dg.worker_model, "haiku");
        assert_eq!(dg.verifier_model, "sonnet");
        assert!(!dg.rationale.contains("budget pressure"));

        // gated stays human-gated, never downgraded.
        let gated = downgrade_for_budget(decide("deploy to prod", &[], "gated", &[], 0.7, 2));
        assert_eq!(gated.basis, "gated");
        assert_eq!(gated.worker_model, "opus");
    }

    #[test]
    fn cold_start_scales_tier_to_blast_radius() {
        // Design keywords win outright, even across many files…
        assert_eq!(prior_model("refactor the auth module", 20), "opus");
        // …a very wide (>10 files) non-trivial, non-design change still earns opus…
        assert_eq!(prior_model("implement the new endpoints", 12), "opus");
        // …a medium spread (6–10 files) gets sonnet…
        assert_eq!(prior_model("implement the new endpoints", 8), "sonnet");
        // …and an ordinary small change starts at the haiku floor (cheap cold
        // start; the verifier's cascade buys up only what actually needs it).
        assert_eq!(prior_model("add a field to the response", 1), "haiku");
    }

    #[test]
    fn learned_picks_cheapest_that_clears() {
        // haiku passes 3/3 on similar tasks → choose haiku even if sonnet also passes.
        let neighbors = vec![
            nb("haiku", true),
            nb("haiku", true),
            nb("haiku", true),
            nb("sonnet", true),
        ];
        let d = decide("implement endpoint", &[], "parallel", &neighbors, 0.7, 2);
        assert_eq!(d.worker_model, "haiku");
        assert_eq!(d.basis, "learned");
    }

    #[test]
    fn learned_escalates_when_cheap_fails() {
        // haiku fails 0/2, sonnet passes 2/2 → choose sonnet.
        let neighbors = vec![
            nb("haiku", false),
            nb("haiku", false),
            nb("sonnet", true),
            nb("sonnet", true),
        ];
        let d = decide("implement endpoint", &[], "parallel", &neighbors, 0.7, 2);
        assert_eq!(d.worker_model, "sonnet");
        assert_eq!(d.basis, "learned");
    }

    #[test]
    fn too_few_samples_falls_back_to_prior() {
        // only one haiku pass — below min_samples=2 → prior, not learned.
        let neighbors = vec![nb("haiku", true)];
        let d = decide(
            "tweak something ordinary",
            &[],
            "parallel",
            &neighbors,
            0.7,
            2,
        );
        assert_eq!(d.basis, "prior");
    }

    // --- bandit (Thompson sampling) -------------------------------------------

    fn tally_picks(neighbors: &[Neighbor], iters: u64) -> BTreeMap<String, usize> {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for i in 0..iters {
            let mut rng = Rng::new(i + 1);
            let d = decide_bandit(
                "implement an endpoint",
                &[],
                "parallel",
                neighbors,
                0.7,
                2,
                &mut rng,
            );
            *m.entry(d.worker_model).or_insert(0) += 1;
        }
        m
    }

    #[test]
    fn bandit_exploits_strong_history() {
        // haiku passes 5/5 → it should dominate.
        let neighbors = vec![
            nb("haiku", true),
            nb("haiku", true),
            nb("haiku", true),
            nb("haiku", true),
            nb("haiku", true),
        ];
        let t = tally_picks(&neighbors, 300);
        assert!(
            t.get("haiku").copied().unwrap_or(0) > 230,
            "haiku picks: {t:?}"
        );
    }

    #[test]
    fn bandit_explores_untested_cheaper_tier() {
        // sonnet has a track record; haiku is untested. The bandit must still try
        // the cheaper haiku sometimes (exploration), while favouring sonnet.
        let neighbors = vec![
            nb("sonnet", true),
            nb("sonnet", true),
            nb("sonnet", true),
            nb("sonnet", true),
        ];
        let t = tally_picks(&neighbors, 300);
        assert!(
            t.get("haiku").copied().unwrap_or(0) > 20,
            "expected some haiku exploration: {t:?}"
        );
        assert!(
            t.get("sonnet").copied().unwrap_or(0) > t.get("opus").copied().unwrap_or(0),
            "{t:?}"
        );
    }

    #[test]
    fn bandit_avoids_failing_cheap_tier() {
        // haiku fails 0/4, sonnet passes 4/4 → haiku should rarely be chosen.
        let neighbors = vec![
            nb("haiku", false),
            nb("haiku", false),
            nb("haiku", false),
            nb("haiku", false),
            nb("sonnet", true),
            nb("sonnet", true),
            nb("sonnet", true),
            nb("sonnet", true),
        ];
        let t = tally_picks(&neighbors, 300);
        assert!(
            t.get("haiku").copied().unwrap_or(0) < 50,
            "haiku over-chosen: {t:?}"
        );
        assert!(t.get("sonnet").copied().unwrap_or(0) > 150, "{t:?}");
    }

    #[test]
    fn bandit_keeps_gated_human_gated() {
        let mut rng = Rng::new(1);
        let d = decide_bandit("deploy to prod", &[], "gated", &[], 0.7, 2, &mut rng);
        assert_eq!(d.basis, "gated");
    }

    // Helper: build a Neighbor with explicit cost_usd.
    fn nb_cost(model: &str, pass: bool, cost_usd: f64) -> Neighbor {
        let mut n = nb(model, pass);
        n.ep.cost_usd = cost_usd;
        n
    }

    #[test]
    fn cost_per_pass_prefers_cheaper_tier_over_equally_accurate_pricier_one() {
        // haiku: 3/3 pass @ $0.10 each → cost/pass = $0.10
        // sonnet: 3/3 pass @ $1.00 each → cost/pass = $1.00
        // Both clear 70% threshold. haiku should win despite same pass rate.
        let neighbors = vec![
            nb_cost("haiku", true, 0.10),
            nb_cost("haiku", true, 0.10),
            nb_cost("haiku", true, 0.10),
            nb_cost("sonnet", true, 1.00),
            nb_cost("sonnet", true, 1.00),
            nb_cost("sonnet", true, 1.00),
        ];
        let d = decide("test task", &[], "parallel", &neighbors, 0.7, 3);
        assert_eq!(
            d.worker_model, "haiku",
            "haiku costs less per pass — should be chosen: {d:?}"
        );
        assert!(d.rationale.contains("cost/pass="), "{}", d.rationale);
    }

    #[test]
    fn bandit_cost_adj_favours_cheaper_tier_over_expensive_with_equal_accuracy() {
        // haiku: 5/5 pass @ $0.10 each → cpp = $0.10
        // sonnet: 5/5 pass @ $1.00 each → cpp = $1.00, 10× more expensive
        // With cost data, haiku's efficiency score is 10× higher than sonnet's at
        // equal sample values — haiku should dominate overwhelmingly.
        let neighbors: Vec<Neighbor> = (0..5)
            .map(|_| nb_cost("haiku", true, 0.10))
            .chain((0..5).map(|_| nb_cost("sonnet", true, 1.00)))
            .collect();
        let t = tally_picks(&neighbors, 300);
        assert!(
            t.get("haiku").copied().unwrap_or(0) > 250,
            "haiku should dominate with 10× cost advantage: {t:?}"
        );
    }

    #[test]
    fn cost_per_pass_skips_tier_with_no_cost_data_gracefully() {
        // All cost_usd = 0.0 → cost/pass = 0 for all tiers that pass.
        // Should degrade to original cheapest-first (haiku wins).
        let neighbors = vec![
            nb("haiku", true),
            nb("haiku", true),
            nb("haiku", true),
            nb("sonnet", true),
            nb("sonnet", true),
            nb("sonnet", true),
        ];
        let d = decide("test task", &[], "parallel", &neighbors, 0.7, 3);
        assert_eq!(d.worker_model, "haiku", "zero-cost fallback: {d:?}");
    }
}
