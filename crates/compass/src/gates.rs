//! gates — compass's [`RigorGates`] impl for the **deterministic floor only**:
//! **C1 (existence)** and **C2 (freshness/drift)** (DESIGN §12).
//!
//! ARCHITECTURE: a Rust binary cannot call an LLM or `AskUserQuestion`, so the
//! LLM gates C3 (observable DoD) / C4 (goal-vs-recent-work consistency) /
//! C5 (gap computability) are evaluated **by the SKILL later** — NOT here. This
//! `evaluate` returns only C1/C2 open questions; it returns empty when both pass
//! (charter sharp & fresh per the deterministic floor).

use std::path::Path;

use harness_core::interrogate::{Bundle, OpenQuestion, RigorGates};

use crate::charter::Charter;
use crate::config::Config;
use crate::freshness;

/// Deterministic-floor gates for compass (C1 + C2). Borrows the resolved config
/// and the charter context needed to run the freshness signals.
// Driven by the `/compass` skill via harness-core's `interrogate::evaluate` in a
// later task (DESIGN §12); exercised by tests now.
#[allow(dead_code)]
pub struct CompassGates<'a> {
    pub cfg: &'a Config,
    pub repo_root: &'a Path,
    pub charter_path: &'a Path,
    pub charter: &'a Charter,
}

impl<'a> RigorGates for CompassGates<'a> {
    fn evaluate(&self, bundle: &Bundle) -> Vec<OpenQuestion> {
        let mut open: Vec<OpenQuestion> = Vec::new();

        // ── C1 existence ──────────────────────────────────────────────────
        // charter file missing OR north_star empty OR definition_of_done empty.
        let missing_file = !self.charter_path.exists();
        let empty_north_star = self.charter.north_star.trim().is_empty();
        let empty_dod = self.charter.definition_of_done.is_empty();

        if missing_file || empty_north_star || empty_dod {
            let gap = if missing_file {
                "charter.md absent — no stated goal to ground against".to_string()
            } else {
                let mut parts = Vec::new();
                if empty_north_star {
                    parts.push("north_star empty");
                }
                if empty_dod {
                    parts.push("definition_of_done empty");
                }
                parts.join("; ")
            };
            open.push(OpenQuestion {
                gate: "C1".to_string(),
                reference: "charter".to_string(),
                gap,
                // C1 sources: the charter fragments already in the bundle.
                sources: charter_fragments(bundle),
                // No defensible default when the goal doesn't exist yet.
                default: None,
            });
            // C1 fails => the charter isn't a usable basis; don't also run C2.
            return open;
        }

        // ── C2 freshness/drift ────────────────────────────────────────────
        let fresh = freshness::check(self.repo_root, self.charter_path, self.charter, self.cfg);
        if fresh.stale {
            open.push(OpenQuestion {
                gate: "C2".to_string(),
                reference: "charter".to_string(),
                gap: fresh.reasons.join("; "),
                // The "what's actually happening now" sources motivate the drift.
                sources: recent_activity_fragments(bundle),
                // Tiebreak hint: present the current north_star as the
                // "still valid?" recommended default for the SKILL to confirm.
                default: Some(self.charter.north_star.clone()),
            });
        }

        open
    }
}

/// Fragments sourced from the charter itself (High authority) — used as the
/// `sources` on the C1 question.
#[allow(dead_code)]
fn charter_fragments(bundle: &Bundle) -> Vec<harness_core::interrogate::Fragment> {
    bundle
        .fragments
        .iter()
        .filter(|f| f.source_path == ".compass/charter.md")
        .cloned()
        .collect()
}

/// Fragments describing recent activity (git / deepwiki) — the
/// "what the project is doing now" evidence behind a drift question.
#[allow(dead_code)]
fn recent_activity_fragments(bundle: &Bundle) -> Vec<harness_core::interrogate::Fragment> {
    bundle
        .fragments
        .iter()
        .filter(|f| f.source_path != ".compass/charter.md")
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::interrogate::evaluate;

    fn gates<'a>(
        cfg: &'a Config,
        root: &'a Path,
        path: &'a Path,
        charter: &'a Charter,
    ) -> CompassGates<'a> {
        CompassGates {
            cfg,
            repo_root: root,
            charter_path: path,
            charter,
        }
    }

    #[test]
    fn c1_fires_on_missing_charter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = Config::default();
        let charter = Charter::default();
        let path = Charter::project_path(dir.path()); // does not exist
        let g = gates(&cfg, dir.path(), &path, &charter);

        let open = evaluate(&g, &Bundle::default());
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].gate, "C1");
        assert!(open[0].default.is_none());
    }

    #[test]
    fn c1_fires_on_empty_north_star() {
        let dir = tempfile::tempdir().expect("tempdir");
        // File exists but north_star empty.
        let charter = Charter {
            north_star: String::new(),
            definition_of_done: vec!["something".to_string()],
            ..Charter::default()
        };
        let path = dir.path().join("charter.md");
        charter.save(&path).unwrap();
        let cfg = Config::default();
        let g = gates(&cfg, dir.path(), &path, &charter);

        let open = evaluate(&g, &Bundle::default());
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].gate, "C1");
        assert!(open[0].gap.contains("north_star"));
    }

    #[test]
    fn c1_passes_on_populated_fresh_charter() {
        // Hermetic git repo so C2's commit/elapsed signals don't trip.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        git_init_with_charter(root);

        let charter = Charter {
            north_star: "Ship compass.".to_string(),
            definition_of_done: vec!["crate builds".to_string()],
            ..Charter::default()
        };
        // No DoD path-like tokens => DoD-ref signal won't trip.
        let path = root.join(".compass/charter.md");
        let cfg = Config::default();
        let g = gates(&cfg, root, &path, &charter);

        let open = evaluate(&g, &Bundle::default());
        assert!(
            open.is_empty(),
            "populated, fresh charter should pass C1+C2: {open:?}"
        );
    }

    #[test]
    fn c2_fires_on_missing_dod_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        git_init_with_charter(root);

        let charter = Charter {
            north_star: "Ship compass.".to_string(),
            definition_of_done: vec!["src/missing_thing.rs must exist".to_string()],
            ..Charter::default()
        };
        let path = root.join(".compass/charter.md");
        let cfg = Config::default(); // check_dod_refs = true
        let g = gates(&cfg, root, &path, &charter);

        let open = evaluate(&g, &Bundle::default());
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].gate, "C2");
        assert!(open[0].gap.contains("missing_thing.rs"));
        // C2 carries the current north_star as the "still valid?" default.
        assert_eq!(open[0].default.as_deref(), Some("Ship compass."));
    }

    /// Init a git repo and commit a charter so C2's commit-divergence /
    /// elapsed-days signals see a just-committed, in-range charter.
    fn git_init_with_charter(root: &Path) {
        use std::process::Command;
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .expect("git");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.t"]);
        run(&["config", "user.name", "t"]);
        std::fs::create_dir_all(root.join(".compass")).unwrap();
        std::fs::write(root.join(".compass/charter.md"), "## north_star\nx\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "add charter"]);
    }
}
