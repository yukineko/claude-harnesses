use crate::config::Config;
use crate::hypothesis::{Assumption, Criterion, Evidence, Hypothesis, Risk, Status};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// ── TOML file shape ───────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct HypothesesFile {
    #[serde(default)]
    hypotheses: Vec<Hypothesis>,
}

// ── Timestamp (for validate/reject updated_at) ────────────────────────────────

fn now_iso() -> String {
    use chrono::Utc;
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ── Store ─────────────────────────────────────────────────────────────────────

pub struct Store {
    hypotheses: Vec<Hypothesis>,
    cfg: Config,
}

impl Store {
    pub fn load(cfg: &Config) -> Result<Self> {
        let path = cfg.hypotheses_path();
        let hypotheses = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            let file: HypothesesFile = toml::from_str(&text)?;
            file.hypotheses
        } else {
            vec![]
        };
        Ok(Self {
            hypotheses,
            cfg: Config {
                enabled: cfg.enabled,
                store_dir: cfg.store_dir.clone(),
                inject_limit: cfg.inject_limit,
            },
        })
    }

    fn save(&self) -> Result<()> {
        let store_dir = &self.cfg.store_dir;
        std::fs::create_dir_all(store_dir)?;

        let file = HypothesesFile {
            hypotheses: self.hypotheses.clone(),
        };
        let content = toml::to_string_pretty(&file)?;

        // Atomic write: write to temp file then rename
        let tmp_name = format!(".hypotheses.tmp.{}.toml", {
            let mut h = DefaultHasher::new();
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .hash(&mut h);
            h.finish()
        });
        let tmp_path = store_dir.join(&tmp_name);
        std::fs::write(&tmp_path, content)?;
        std::fs::rename(&tmp_path, self.cfg.hypotheses_path())?;
        Ok(())
    }

    /// Convenience wrapper for [`Store::add_with_criteria`] without criteria.
    /// Used by tests and kept as a stable, ergonomic entry point.
    #[allow(dead_code)]
    pub fn add(&mut self, text: String, goal: Option<String>) -> Result<String> {
        self.add_with_criteria(text, goal, None, None)
    }

    /// Add a hypothesis with optional pre-registered success/kill criteria.
    ///
    /// Fixing the bar at `add` time (before the experiment ships) is what makes
    /// the hypothesis falsifiable: `validate` later checks a measurement against
    /// these criteria instead of accepting any after-the-fact evidence.
    pub fn add_with_criteria(
        &mut self,
        text: String,
        goal: Option<String>,
        success: Option<Criterion>,
        kill: Option<Criterion>,
    ) -> Result<String> {
        let mut h = Hypothesis::new(text, goal);
        h.success_criterion = success;
        h.kill_criterion = kill;
        let id = h.id.clone();
        self.hypotheses.push(h);
        self.save()?;
        Ok(id)
    }

    /// Convenience wrapper for [`Store::validate_with_measurements`] with no
    /// measurements. Used by tests and the no-criteria validation path.
    #[allow(dead_code)]
    pub fn validate(
        &mut self,
        id: &str,
        evidence: Vec<String>,
        run_id: Option<String>,
    ) -> Result<()> {
        self.validate_with_measurements(id, evidence, vec![], run_id)
    }

    /// Validate a hypothesis, checking any pre-registered success criterion
    /// against the supplied `measurements` (`(metric, value)` pairs).
    ///
    /// Gates, in order:
    /// 1. **Measured-evidence gate** (build != validation): refuse if neither
    ///    evidence nor a measurement was supplied.
    /// 2. **Pre-registered success gate** (anti goalpost-shift): if a success
    ///    criterion was registered at `add` time, a measurement for that metric
    ///    must be supplied *and* clear the registered bar. If the measurement
    ///    instead hits the kill criterion, the error points at `reject`.
    pub fn validate_with_measurements(
        &mut self,
        id: &str,
        evidence: Vec<String>,
        measurements: Vec<(String, f64)>,
        run_id: Option<String>,
    ) -> Result<()> {
        // (1) A hypothesis is "validated" by measured learning, not by code
        // shipping. A measurement counts as evidence; require at least one of
        // the two so a build alone can't flip the status.
        if evidence.iter().all(|e| e.trim().is_empty()) && measurements.is_empty() {
            anyhow::bail!(
                "validate requires measured evidence: pass --evidence \"<observed outcome>\" \
                 or --measurement \"<metric>=<value>\" (shipping code is not validation)"
            );
        }

        let idx = self
            .hypotheses
            .iter()
            .position(|h| h.id == id)
            .ok_or_else(|| anyhow::anyhow!("hypothesis not found: {id}"))?;

        // (2) Pre-registered success gate. Clone the criteria so the immutable
        // checks don't conflict with the mutable update below.
        let success = self.hypotheses[idx].success_criterion.clone();
        let kill = self.hypotheses[idx].kill_criterion.clone();
        if let Some(crit) = &success {
            let measured = measurements
                .iter()
                .find(|(m, _)| m == &crit.metric)
                .map(|(_, v)| *v);
            match measured {
                None => anyhow::bail!(
                    "hypothesis {id} has a pre-registered success criterion ({crit}); \
                     pass the measured value with --measurement \"{}=<value>\" so validation \
                     checks the registered bar (no post-hoc goalpost-shifting)",
                    crit.metric
                ),
                Some(v) if !crit.satisfied_by(v) => {
                    let hint = match &kill {
                        Some(k) if k.metric == crit.metric && k.satisfied_by(v) => format!(
                            " — it hits the kill criterion ({k}); reject it with \
                             `hypothesis reject {id} --reason \"...\"`"
                        ),
                        _ => format!(
                            " — record it with `hypothesis reject {id} --reason \"...\"` \
                             if the bet is disproven"
                        ),
                    };
                    anyhow::bail!(
                        "measured {}={} does not clear the pre-registered success criterion \
                         ({crit}); this is not a validation{hint}",
                        crit.metric,
                        v
                    );
                }
                Some(_) => {} // measurement clears the registered bar → proceed
            }
        }

        let measured_evidence: Vec<String> = measurements
            .iter()
            .map(|(m, v)| match &success {
                Some(c) if &c.metric == m => format!("{m}={v} (success criterion {c} met)"),
                _ => format!("{m}={v}"),
            })
            .collect();

        let h = &mut self.hypotheses[idx];
        h.status = Status::Validated;
        h.evidence
            .extend(evidence.into_iter().filter(|e| !e.trim().is_empty()));
        h.evidence.extend(measured_evidence);
        h.condukt_run = run_id;
        h.updated_at = now_iso();
        self.save()
    }

    /// Move a hypothesis into the `awaiting-measurement` state: a linked
    /// deliverable has shipped but no measurement has been taken yet. This is
    /// deliberately distinct from validation (build != validation) — a human
    /// still has to run validate/reject with evidence after measuring.
    pub fn mark_awaiting_measurement(&mut self, id: &str, run_id: Option<String>) -> Result<()> {
        let h = self
            .hypotheses
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or_else(|| anyhow::anyhow!("hypothesis not found: {id}"))?;
        h.status = Status::AwaitingMeasurement;
        h.condukt_run = run_id;
        let now = now_iso();
        // Overwritten on every (re-)ship: the shipped-vs-measured metric cares
        // about the most recent shipment, not the first.
        h.shipped_at = Some(now.clone());
        h.updated_at = now;
        self.save()
    }

    /// Shipped-vs-measured PDO health metrics over the current store contents
    /// (see [`crate::hypothesis::compute_stats`]).
    pub fn stats(&self) -> crate::hypothesis::Stats {
        crate::hypothesis::compute_stats(&self.hypotheses)
    }

    pub fn reject(
        &mut self,
        id: &str,
        reason: Option<String>,
        run_id: Option<String>,
    ) -> Result<()> {
        // Rejection is also a measured learning decision; require a reason.
        let reason = match reason {
            Some(r) if !r.trim().is_empty() => r,
            _ => anyhow::bail!("reject requires a reason: pass --reason \"<what disproved it>\""),
        };
        let h = self
            .hypotheses
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or_else(|| anyhow::anyhow!("hypothesis not found: {id}"))?;
        h.status = Status::Rejected;
        h.evidence.push(reason);
        h.condukt_run = run_id;
        h.updated_at = now_iso();
        self.save()
    }

    /// Attach an assumption to a hypothesis. Recording assumptions lets flow
    /// run a RAT (riskiest-assumption test) — de-risking the leap of faith with
    /// a minimal experiment before committing to a full build.
    pub fn add_assumption(
        &mut self,
        id: &str,
        text: String,
        risk: Risk,
        evidence: Evidence,
    ) -> Result<()> {
        if text.trim().is_empty() {
            anyhow::bail!("assumption text must not be empty");
        }
        let h = self
            .hypotheses
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or_else(|| anyhow::anyhow!("hypothesis not found: {id}"))?;
        h.assumptions.push(Assumption {
            text,
            risk,
            evidence,
            tested: false,
        });
        h.updated_at = now_iso();
        self.save()
    }

    /// Mark the assumption at `index` as tested (e.g. after a RAT de-risked it),
    /// so it no longer registers as an untested leap of faith.
    pub fn mark_assumption_tested(&mut self, id: &str, index: usize) -> Result<()> {
        let h = self
            .hypotheses
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or_else(|| anyhow::anyhow!("hypothesis not found: {id}"))?;
        let n = h.assumptions.len();
        let a = h
            .assumptions
            .get_mut(index)
            .ok_or_else(|| anyhow::anyhow!("assumption index {index} out of range (have {n})"))?;
        a.tested = true;
        h.updated_at = now_iso();
        self.save()
    }

    /// List hypotheses (optionally filtered by status) in **discovery order**:
    /// confidence descending, then created_at ascending as the deterministic
    /// tie-break. This is what makes confidence load-bearing — the open list is
    /// no longer insertion-ordered, so the highest-confidence bet surfaces first
    /// for validation. `f64::total_cmp` gives a total order (NaN-safe); created_at
    /// is an ISO-8601 string so a lexicographic compare is chronological.
    pub fn list(&self, status: Option<&str>) -> Vec<&Hypothesis> {
        let mut out: Vec<&Hypothesis> = match status {
            None => self.hypotheses.iter().collect(),
            Some(s) => self
                .hypotheses
                .iter()
                .filter(|h| h.status.to_string() == s)
                .collect(),
        };
        out.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        out
    }

    /// Update a hypothesis's discovery confidence and persist. Errors if no
    /// hypothesis has the given id. Changing confidence re-orders [`Store::list`]
    /// (and therefore flow's open-hypothesis pick) deterministically. Wired to the
    /// `confidence <id> <value>` CLI subcommand and `add --confidence`.
    pub fn set_confidence(&mut self, id: &str, value: f64) -> Result<()> {
        let h = self
            .hypotheses
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or_else(|| anyhow::anyhow!("hypothesis not found: {id}"))?;
        h.confidence = value;
        h.updated_at = now_iso();
        self.save()
    }

    /// All hypotheses as a backing slice (for callers that filter themselves).
    pub fn all(&self) -> &[Hypothesis] {
        &self.hypotheses
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_cfg(dir: &TempDir) -> Config {
        Config {
            enabled: true,
            store_dir: dir.path().to_path_buf(),
            inject_limit: 2000,
        }
    }

    #[test]
    fn test_add_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        // add a hypothesis
        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("my hypothesis text".to_string(), None).unwrap();
        assert!(!id.is_empty());

        // reload from disk
        let st2 = Store::load(&cfg).unwrap();
        let hypotheses = st2.list(None);
        assert_eq!(hypotheses.len(), 1);
        assert_eq!(hypotheses[0].id, id);
        assert_eq!(hypotheses[0].text, "my hypothesis text");
        assert!(hypotheses[0].status.is_open());
    }

    // --- confidence ordering (discovery layer made load-bearing) ---

    #[test]
    fn list_sorts_by_confidence_descending() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);
        let mut st = Store::load(&cfg).unwrap();
        // Insertion order is low, high, mid — confidence must override it.
        let lo = st.add("low".to_string(), None).unwrap();
        let hi = st.add("high".to_string(), None).unwrap();
        let mid = st.add("mid".to_string(), None).unwrap();
        st.set_confidence(&lo, 0.1).unwrap();
        st.set_confidence(&hi, 0.9).unwrap();
        st.set_confidence(&mid, 0.5).unwrap();
        let order: Vec<&str> = st.list(None).iter().map(|h| h.text.as_str()).collect();
        assert_eq!(order, vec!["high", "mid", "low"]);
    }

    #[test]
    fn list_confidence_tiebreak_by_created_at() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);
        let mut st = Store::load(&cfg).unwrap();
        let older = st.add("older".to_string(), None).unwrap();
        let newer = st.add("newer".to_string(), None).unwrap();
        // Equal confidence; force distinct created_at so the tie-break is
        // deterministic regardless of how fast the two adds happened.
        st.set_confidence(&older, 0.5).unwrap();
        st.set_confidence(&newer, 0.5).unwrap();
        for h in st.hypotheses.iter_mut() {
            if h.id == older {
                h.created_at = "2026-01-01T00:00:00Z".to_string();
            } else if h.id == newer {
                h.created_at = "2026-02-01T00:00:00Z".to_string();
            }
        }
        let order: Vec<&str> = st.list(None).iter().map(|h| h.text.as_str()).collect();
        assert_eq!(order, vec!["older", "newer"]);
    }

    #[test]
    fn set_confidence_changes_list_order() {
        // The load-bearing assertion: editing confidence reorders the queue
        // and the new order survives a reload from disk.
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);
        let mut st = Store::load(&cfg).unwrap();
        let a = st.add("A".to_string(), None).unwrap();
        let b = st.add("B".to_string(), None).unwrap();
        st.set_confidence(&a, 0.3).unwrap();
        st.set_confidence(&b, 0.6).unwrap();
        assert_eq!(st.list(None)[0].text, "B");

        st.set_confidence(&a, 0.9).unwrap();
        assert_eq!(st.list(None)[0].text, "A");
        // persisted, not just in-memory
        let st2 = Store::load(&cfg).unwrap();
        assert_eq!(st2.list(None)[0].text, "A");
    }

    #[test]
    fn set_confidence_unknown_id_errors() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);
        let mut st = Store::load(&cfg).unwrap();
        assert!(st.set_confidence("nonexistent", 0.9).is_err());
    }

    #[test]
    fn test_validate() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("validate this".to_string(), None).unwrap();

        st.validate(
            &id,
            vec!["evidence A".to_string(), "evidence B".to_string()],
            None,
        )
        .unwrap();

        // reload to verify persistence
        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert!(h.status.is_validated());
        assert!(h.evidence.contains(&"evidence A".to_string()));
        assert!(h.evidence.contains(&"evidence B".to_string()));
    }

    #[test]
    fn test_reject() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("reject this".to_string(), None).unwrap();

        st.reject(&id, Some("not supported by data".to_string()), None)
            .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert!(h.status.is_rejected());
        assert!(h.evidence.contains(&"not supported by data".to_string()));
    }

    #[test]
    fn test_mark_awaiting_measurement_persists() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st
            .add("shipped but not measured".to_string(), None)
            .unwrap();
        st.mark_awaiting_measurement(&id, Some("run-await1".to_string()))
            .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert!(h.status.is_awaiting_measurement());
        assert_eq!(h.condukt_run, Some("run-await1".to_string()));
        assert!(
            h.shipped_at.is_some(),
            "mark_awaiting_measurement must stamp shipped_at"
        );
    }

    #[test]
    fn test_stats_reflects_shipped_and_measured() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        st.add("still just an idea".to_string(), None).unwrap();
        let shipped_id = st
            .add("shipped, not measured yet".to_string(), None)
            .unwrap();
        st.mark_awaiting_measurement(&shipped_id, None).unwrap();

        let stats = st.stats();
        assert_eq!(stats.shipped, 1);
        assert_eq!(stats.awaiting, 1);
        assert_eq!(stats.validated, 0);
        assert_eq!(stats.rejected, 0);
        assert_eq!(
            stats.avg_measurement_delay_days, None,
            "nothing has been measured yet"
        );
    }

    #[test]
    fn test_awaiting_measurement_can_still_validate() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("measure after ship".to_string(), None).unwrap();
        st.mark_awaiting_measurement(&id, Some("run-1".to_string()))
            .unwrap();
        assert!(st.list(None)[0].status.is_awaiting_measurement());

        // A human measures and validates with evidence → validated.
        st.validate(
            &id,
            vec!["conversion rose 12%".to_string()],
            Some("run-1".to_string()),
        )
        .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert!(h.status.is_validated());
        assert!(h.evidence.contains(&"conversion rose 12%".to_string()));
    }

    #[test]
    fn test_unknown_id_error() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();

        let err = st
            .validate("deadbeef", vec!["measured".to_string()], None)
            .unwrap_err();
        assert!(err.to_string().contains("hypothesis not found"));

        let err2 = st
            .reject("deadbeef", Some("disproven".to_string()), None)
            .unwrap_err();
        assert!(err2.to_string().contains("hypothesis not found"));
    }

    #[test]
    fn test_load_empty_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        // File does not exist — should return empty store, not error
        let st = Store::load(&cfg).unwrap();
        assert_eq!(st.list(None).len(), 0);
    }

    #[test]
    fn test_validate_with_run_id() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("validate with run".to_string(), None).unwrap();
        st.validate(
            &id,
            vec!["measured".to_string()],
            Some("run-abc123".to_string()),
        )
        .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert!(h.status.is_validated());
        assert_eq!(h.condukt_run, Some("run-abc123".to_string()));
    }

    #[test]
    fn test_reject_with_run_id() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("reject with run".to_string(), None).unwrap();
        st.reject(
            &id,
            Some("disproven".to_string()),
            Some("run-xyz789".to_string()),
        )
        .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert!(h.status.is_rejected());
        assert_eq!(h.condukt_run, Some("run-xyz789".to_string()));
    }

    #[test]
    fn test_validate_requires_evidence() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("needs measuring".to_string(), None).unwrap();

        // Empty / whitespace-only evidence is refused (shipping != validation).
        let err = st
            .validate(&id, vec![], Some("run-1".to_string()))
            .unwrap_err();
        assert!(err.to_string().contains("requires measured evidence"));
        let err2 = st.validate(&id, vec!["   ".to_string()], None).unwrap_err();
        assert!(err2.to_string().contains("requires measured evidence"));

        // Status unchanged after a refused validate.
        assert!(st.list(None)[0].status.is_open());

        // reject likewise requires a reason.
        let err3 = st.reject(&id, None, None).unwrap_err();
        assert!(err3.to_string().contains("requires a reason"));
        assert!(st.list(None)[0].status.is_open());
    }

    #[test]
    fn test_list_status_filter() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let open_id = st.add("stays open".to_string(), None).unwrap();
        let val_id = st.add("gets validated".to_string(), None).unwrap();
        let rej_id = st.add("gets rejected".to_string(), None).unwrap();
        st.validate(&val_id, vec!["measured".to_string()], None)
            .unwrap();
        st.reject(&rej_id, Some("disproven".to_string()), None)
            .unwrap();

        // --status open must exclude validated/rejected (regression: filter was a no-op)
        let open = st.list(Some("open"));
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, open_id);

        assert_eq!(st.list(Some("validated")).len(), 1);
        assert_eq!(st.list(Some("validated"))[0].id, val_id);
        assert_eq!(st.list(Some("rejected")).len(), 1);
        assert_eq!(st.list(Some("rejected"))[0].id, rej_id);

        // None returns everything
        assert_eq!(st.list(None).len(), 3);
    }

    #[test]
    fn test_criteria_persist() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st
            .add_with_criteria(
                "faster onboarding lifts activation".to_string(),
                None,
                Some(Criterion::parse("activation >= 0.4").unwrap()),
                Some(Criterion::parse("activation <= 0.2").unwrap()),
            )
            .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = st2.list(None).into_iter().find(|h| h.id == id).unwrap();
        assert_eq!(
            h.success_criterion.as_ref().map(|c| c.to_string()),
            Some("activation >= 0.4".to_string())
        );
        assert_eq!(
            h.kill_criterion.as_ref().map(|c| c.to_string()),
            Some("activation <= 0.2".to_string())
        );
    }

    #[test]
    fn test_validate_requires_measurement_when_criterion_registered() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st
            .add_with_criteria(
                "bet".to_string(),
                None,
                Some(Criterion::parse("activation >= 0.4").unwrap()),
                None,
            )
            .unwrap();

        // Evidence alone can't validate a hypothesis that pre-registered a bar —
        // a measurement of the registered metric is required.
        let err = st
            .validate(&id, vec!["looks good".to_string()], None)
            .unwrap_err();
        assert!(err.to_string().contains("pre-registered success criterion"));
        assert!(st.list(None)[0].status.is_open());
    }

    #[test]
    fn test_validate_passes_when_measurement_clears_bar() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st
            .add_with_criteria(
                "bet".to_string(),
                None,
                Some(Criterion::parse("activation >= 0.4").unwrap()),
                None,
            )
            .unwrap();

        st.validate_with_measurements(&id, vec![], vec![("activation".to_string(), 0.45)], None)
            .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert!(h.status.is_validated());
        // The measured value is persisted as evidence.
        assert!(h.evidence.iter().any(|e| e.contains("activation=0.45")));
    }

    #[test]
    fn test_validate_refused_when_measurement_misses_bar() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st
            .add_with_criteria(
                "bet".to_string(),
                None,
                Some(Criterion::parse("activation >= 0.4").unwrap()),
                Some(Criterion::parse("activation <= 0.2").unwrap()),
            )
            .unwrap();

        // Misses success bar but not the kill bar → refused, hints at reject.
        let err = st
            .validate_with_measurements(&id, vec![], vec![("activation".to_string(), 0.3)], None)
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("does not clear the pre-registered success criterion"));
        assert!(st.list(None)[0].status.is_open());

        // Hits the kill bar → refused, error points specifically at the kill criterion.
        let err2 = st
            .validate_with_measurements(&id, vec![], vec![("activation".to_string(), 0.1)], None)
            .unwrap_err();
        assert!(err2.to_string().contains("kill criterion"));
        assert!(st.list(None)[0].status.is_open());
    }

    #[test]
    fn test_validate_without_criterion_unchanged() {
        // Hypotheses with no registered criterion keep the old behavior:
        // non-empty evidence is enough.
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("plain bet".to_string(), None).unwrap();
        st.validate(&id, vec!["measured outcome".to_string()], None)
            .unwrap();
        assert!(st.list(None)[0].status.is_validated());
    }

    #[test]
    fn test_assumptions_persist_and_rat_selection() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("users will pay for X".to_string(), None).unwrap();
        st.add_assumption(
            &id,
            "users have this problem".to_string(),
            Risk::High,
            Evidence::None,
        )
        .unwrap();
        st.add_assumption(
            &id,
            "we can build it".to_string(),
            Risk::Medium,
            Evidence::Weak,
        )
        .unwrap();
        st.add_assumption(
            &id,
            "pricing model works".to_string(),
            Risk::High,
            Evidence::Weak,
        )
        .unwrap();

        // Reload → assumptions persisted; RAT = highest leap score untested.
        let st2 = Store::load(&cfg).unwrap();
        let h = st2.list(None).into_iter().find(|h| h.id == id).unwrap();
        assert_eq!(h.assumptions.len(), 3);
        let rat = h.riskiest_assumption().expect("a leap of faith");
        assert_eq!(rat.text, "users have this problem"); // high + none = score 4

        // Marking the top RAT tested promotes the next leap of faith.
        let mut st3 = Store::load(&cfg).unwrap();
        st3.mark_assumption_tested(&id, 0).unwrap();
        let st4 = Store::load(&cfg).unwrap();
        let h4 = st4.list(None).into_iter().find(|h| h.id == id).unwrap();
        assert!(h4.assumptions[0].tested);
        assert_eq!(
            h4.riskiest_assumption().map(|a| a.text.as_str()),
            Some("pricing model works") // high + weak = score 3
        );
    }

    #[test]
    fn test_add_assumption_errors() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("a bet".to_string(), None).unwrap();

        assert!(st
            .add_assumption("deadbeef", "x".to_string(), Risk::Low, Evidence::Weak)
            .unwrap_err()
            .to_string()
            .contains("hypothesis not found"));
        assert!(st
            .add_assumption(&id, "  ".to_string(), Risk::Low, Evidence::Weak)
            .unwrap_err()
            .to_string()
            .contains("must not be empty"));
        assert!(st
            .mark_assumption_tested(&id, 5)
            .unwrap_err()
            .to_string()
            .contains("out of range"));
    }

    #[test]
    fn test_linked_goal_preserved() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        st.add("with goal".to_string(), Some("goal-abc".to_string()))
            .unwrap();

        let st2 = Store::load(&cfg).unwrap();
        let h = &st2.list(None)[0];
        assert_eq!(h.linked_goal, Some("goal-abc".to_string()));
    }
}
