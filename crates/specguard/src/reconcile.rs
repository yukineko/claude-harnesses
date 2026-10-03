//! `specguard reconcile-findings` — close specguard STRUCTURAL review findings
//! (`specguard:<undocumented|dangling-reference|untested>:<key>`, recorded by
//! `specguard audit`) by RE-RUNNING the same deterministic structural detection
//! (backlog 89544915, ruling R4). Never by commit message.
//!
//! * detection ran, the key's map entry is still present, and the detection no
//!   longer reports `(kind, key)` → closed with the non-human verdict
//!   `resolved`;
//! * detection still reports it → open (a determined answer);
//! * detection cannot run (spec map absent / unreadable / unparseable, config
//!   unloadable) → UNDETERMINED: open, reason reported, exit 3. An absent map is
//!   NOT "nothing reported" — `SpecMap::load` reads absence as an empty map,
//!   which is exactly the empty-set-reads-as-clean trap, so absence is checked
//!   here first;
//! * the key's entry is no longer in the map at all → UNDETERMINED: the finding
//!   is not re-observable, and "not found" is not "fixed".
//!
//! Shard-audit kinds (`spec-drift`, `spec-doc-stale`, `audit-indeterminate`)
//! are agent judgment, not re-detectable, and are never selected.
use crate::auditmap::{self, StructuralKind};
use crate::specmap::SpecMap;
use harness_core::verdict::Determination;
use overwatch::observed_close::{self, Observation, ReconcileReport};
use overwatch::review_finding::ReviewFinding;
use std::collections::BTreeSet;
use std::path::Path;

const REVIEWER: &str = "specguard-reconcile";
const OBSERVED_SOURCE: &str = "specguard structural re-detection (spec map)";

const STRUCTURAL_KINDS: [StructuralKind; 3] = [
    StructuralKind::Undocumented,
    StructuralKind::DanglingReference,
    StructuralKind::Untested,
];

/// `specguard:<structural-kind>:<key>` → `(kind, key)`. Keys may contain `:`
/// (`crate::a::B`), kinds never do.
fn parse_id(id: &str) -> Option<(StructuralKind, &str)> {
    let rest = id.strip_prefix("specguard:")?;
    let (kind, key) = rest.split_once(':')?;
    let kind = STRUCTURAL_KINDS.into_iter().find(|k| k.as_str() == kind)?;
    (!key.is_empty()).then_some((kind, key))
}

/// Load the map for re-detection, tri-state (absent is undetermined here).
fn load_map(map_path: &Path) -> Determination<SpecMap> {
    match harness_core::boundary::read_to_string(map_path) {
        Determination::Known(None) => Determination::undetermined(format!(
            "spec map {} is absent, so the structural detection cannot run",
            map_path.display()
        )),
        Determination::Known(Some(_)) => match SpecMap::load(map_path) {
            Ok(m) => Determination::known(m),
            Err(e) => Determination::undetermined(format!(
                "spec map {} could not be loaded ({e:#}), so the structural detection \
                 cannot run",
                map_path.display()
            )),
        },
        Determination::Undetermined(why) => Determination::undetermined(format!(
            "spec map {} could not be read ({}), so the structural detection cannot run",
            map_path.display(),
            why.as_str()
        )),
    }
}

/// Run one reconcile pass. `detection` is `Undetermined` when the config could
/// not even be loaded; otherwise it is the map path to re-detect from.
pub fn reconcile(store_root: &Path, detection: Determination<&Path>) -> ReconcileReport {
    let open = match observed_close::open_findings(store_root, |id| parse_id(id).is_some()) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return ReconcileReport::store_undetermined(why.as_str())
        }
    };
    if open.is_empty() {
        return ReconcileReport::default();
    }
    let map = match detection {
        Determination::Known(path) => load_map(path),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    };
    // (kind, key) pairs the detection reports NOW, plus the keys it looked at.
    let observed = map.map(|m| {
        let reported: BTreeSet<(&'static str, String)> =
            auditmap::scan_map_filtered(&m, store_root, "")
                .into_iter()
                .map(|f| (f.kind.as_str(), f.key))
                .collect();
        let keys: BTreeSet<String> = m.entries.values().map(|e| e.key.clone()).collect();
        (reported, keys)
    });
    let judge = |f: &ReviewFinding| -> Determination<Observation> {
        let Some((kind, key)) = parse_id(&f.finding_id) else {
            return Determination::undetermined(format!(
                "finding id {:?} is not a structural specguard id",
                f.finding_id
            ));
        };
        let (reported, keys) = match &observed {
            Determination::Known(o) => o,
            Determination::Undetermined(why) => {
                return Determination::undetermined(why.as_str().to_string())
            }
        };
        if !keys.contains(key) {
            return Determination::undetermined(format!(
                "map entry {key:?} is no longer in the spec map, so {} cannot be re-observed \
                 (not found is not fixed)",
                kind.as_str()
            ));
        }
        if reported.contains(&(kind.as_str(), key.to_string())) {
            Determination::known(Observation::StillPresent)
        } else {
            Determination::known(Observation::Resolved {
                evidence: format!(
                    "structural re-detection over the spec map no longer reports {} for \
                     entry {key}",
                    kind.as_str()
                ),
            })
        }
    };
    observed_close::close_observed(
        store_root,
        open,
        judge,
        REVIEWER,
        OBSERVED_SOURCE,
        overwatch::store::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_id_accepts_only_structural_kinds_and_keeps_colons_in_key() {
        assert!(matches!(
            parse_id("specguard:untested:crate::a::B"),
            Some((StructuralKind::Untested, "crate::a::B"))
        ));
        assert!(parse_id("specguard:dangling-reference:x").is_some());
        assert!(parse_id("specguard:spec-drift:src").is_none());
        assert!(parse_id("specguard:spec-doc-stale:src").is_none());
        assert!(parse_id("specguard:audit-indeterminate:src").is_none());
        assert!(parse_id("specguard:untested:").is_none());
        assert!(parse_id("gate-exec:r:t").is_none());
    }
}
