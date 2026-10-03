//! `specguard reconcile-findings` — close specguard STRUCTURAL review findings
//! (`specguard:<undocumented|dangling-reference|untested>:<key>[:<episode>]`,
//! recorded by `specguard audit`) by RE-RUNNING the same deterministic
//! structural detection (backlog 89544915, ruling R4). Never by commit message.
//!
//! * detection ran, the key's map entry is still present, and the detection no
//!   longer reports `(kind, key)` → closed with the non-human verdict
//!   `resolved`;
//! * detection ran and the key's map entry was REMOVED (the map is present and
//!   parses, but has no entry for the key) → closed `resolved`, evidence naming
//!   the key and saying it was removed (user ruling 2026-10-03: a structural
//!   gap about an entry that no longer exists can no longer hold);
//! * detection still reports it → open (a determined answer);
//! * detection cannot run (spec map absent / unreadable / unparseable, config
//!   unloadable) → UNDETERMINED: open, reason reported, exit 3. An absent map is
//!   NOT "every entry removed" — `SpecMap::load` reads absence as an empty map,
//!   which is exactly the empty-set-reads-as-clean trap, so absence is checked
//!   here first and never reaches the removed-entry rule.
//!
//! ## Episodes
//!
//! The review queue joins dispositions on the EXACT `finding_id`, so a gap that
//! was closed `resolved` and later came back would, under an episode-less id,
//! be re-derived as the same id and stay hidden behind the old disposition.
//! Like record-audit (`record-audit:<dim>:<first-breach epoch>`), a structural
//! id therefore carries an episode: `specguard:<kind>:<key>:<epoch>`, where
//! `<epoch>` is the unix second the episode was first recorded. The episode is
//! persisted as the id itself in the review-findings store (see
//! [`next_episode`]); legacy episode-less ids recorded before this change are
//! still recognised, reused while open, and closed here.
//!
//! Shard-audit kinds (`spec-drift`, `spec-doc-stale`, `audit-indeterminate`)
//! are agent judgment, not re-detectable, and are never selected.
use crate::auditmap::{self, StructuralKind};
use crate::specmap::SpecMap;
use harness_core::verdict::Determination;
use overwatch::disposition::DispositionVerdict;
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

/// `specguard:<structural-kind>:<rest>` → `(kind, rest)`, where `rest` is
/// `<key>` (legacy) or `<key>:<epoch>` (episode). Keys may contain `:`
/// (`crate::a::B`), kinds never do — so `rest` alone cannot always tell the
/// two shapes apart; [`resolve_key`] disambiguates with the finding's `file`
/// and the map.
fn parse_id(id: &str) -> Option<(StructuralKind, &str)> {
    let rest = id.strip_prefix("specguard:")?;
    let (kind, key) = rest.split_once(':')?;
    let kind = STRUCTURAL_KINDS.into_iter().find(|k| k.as_str() == kind)?;
    (!key.is_empty()).then_some((kind, key))
}

/// The pre-episode id shape, still present on live stores.
pub(crate) fn legacy_id(kind: StructuralKind, key: &str) -> String {
    format!("specguard:{}:{key}", kind.as_str())
}

/// The episode id shape `specguard:<kind>:<key>:<epoch>`.
pub(crate) fn episode_id(kind: StructuralKind, key: &str, epoch: i64) -> String {
    format!("{}:{epoch}", legacy_id(kind, key))
}

/// Does `id` belong to `(kind, key)`? `Some(None)` = the legacy id,
/// `Some(Some(epoch))` = an episode id, `None` = not this finding.
pub(crate) fn episode_of(id: &str, kind: StructuralKind, key: &str) -> Option<Option<i64>> {
    let legacy = legacy_id(kind, key);
    if id == legacy {
        return Some(None);
    }
    let epoch = id.strip_prefix(&legacy)?.strip_prefix(':')?;
    if epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    epoch.parse::<i64>().ok().map(Some)
}

/// What `specguard audit` should do for a structural finding it detects NOW.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Episode {
    /// The finding's current episode is still open, or was closed by a HUMAN
    /// verdict that a re-audit must not override: record nothing.
    Open(String),
    /// Record a new finding under this id.
    Start(String),
}

/// Decide the id for a structural finding detected now, from the recorded
/// rows and their dispositions (the episode is persisted as the id itself).
///
/// * a row for `(kind, key)` with no disposition → `Open` (same id, no
///   re-record);
/// * no row at all → `Start` a new episode;
/// * every row closed, and the LATEST episode was closed `resolved` (an
///   automated observation that the gap went away) → the gap RECURRED: `Start`
///   a new episode, so the recurrence is visible rather than hidden behind the
///   old disposition;
/// * every row closed and the latest by a human verdict (confirmed /
///   dismissed / false-positive) → `Open`: the human verdict on this gap
///   stands (the pre-episode behaviour for human closures).
///
/// `rows` is `(finding_id, file)`; a row whose `file` names a different key is
/// not this finding even if its id shape matches (keys may contain `:`). The
/// new epoch is `max(now, latest + 1)` so a recurrence within the same second
/// still gets a fresh id.
pub(crate) fn next_episode<'a>(
    kind: StructuralKind,
    key: &str,
    rows: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    verdict_of: impl Fn(&str) -> Option<DispositionVerdict>,
    now: i64,
) -> Episode {
    // (epoch, id) of every row belonging to this finding; legacy sorts first.
    let mut mine: Vec<(i64, &str)> = rows
        .into_iter()
        .filter(|(_, file)| file.is_none_or(|f| f == key))
        .filter_map(|(id, _)| episode_of(id, kind, key).map(|e| (e.unwrap_or(i64::MIN), id)))
        .collect();
    mine.sort();
    mine.dedup();
    if let Some((_, id)) = mine.iter().find(|(_, id)| verdict_of(id).is_none()) {
        return Episode::Open((*id).to_string());
    }
    let Some(&(latest_epoch, latest_id)) = mine.last() else {
        return Episode::Start(episode_id(kind, key, now));
    };
    match verdict_of(latest_id) {
        Some(DispositionVerdict::Resolved) => Episode::Start(episode_id(
            kind,
            key,
            now.max(latest_epoch.saturating_add(1)),
        )),
        _ => Episode::Open(latest_id.to_string()),
    }
}

/// The map key a recorded structural finding is about.
///
/// The producer stores the key in `file`; when present it must match the id
/// (legacy or episode shape) or the row is inconsistent → `Err` (undetermined).
/// A row without `file` (hand-made / foreign) is resolved against the map: the
/// whole `rest` (legacy reading) and `rest` minus a `:<digits>` suffix (episode
/// reading) are the candidates. Exactly one in the map → that key; neither in
/// the map → the entry is removed under either reading (`Ok(rest)`); both →
/// ambiguous → `Err`.
fn resolve_key<'a>(
    f: &'a ReviewFinding,
    kind: StructuralKind,
    rest: &'a str,
    keys: &BTreeSet<String>,
) -> Result<&'a str, String> {
    if let Some(file) = f.file.as_deref() {
        return match episode_of(&f.finding_id, kind, file) {
            Some(_) => Ok(file),
            None => Err(format!(
                "finding id {:?} does not match its recorded key {file:?}",
                f.finding_id
            )),
        };
    }
    let episodic = rest
        .rsplit_once(':')
        .filter(|(k, e)| !k.is_empty() && !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit()))
        .map(|(k, _)| k);
    match (keys.contains(rest), episodic.filter(|k| keys.contains(*k))) {
        (true, Some(k)) => Err(format!(
            "finding id {:?} has no recorded key and is ambiguous between map entries \
             {rest:?} and {k:?}",
            f.finding_id
        )),
        (true, None) => Ok(rest),
        (false, Some(k)) => Ok(k),
        (false, None) => Ok(rest),
    }
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
        let Some((kind, rest)) = parse_id(&f.finding_id) else {
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
        let key = match resolve_key(f, kind, rest, keys) {
            Ok(k) => k,
            Err(why) => return Determination::undetermined(why),
        };
        // Reached only with a map that is present and parsed (absence and
        // parse failure are `Undetermined` above), so "no entry" here is an
        // observation that the entry was removed, not an empty-set default.
        if !keys.contains(key) {
            return Determination::known(Observation::Resolved {
                evidence: format!(
                    "map entry {key:?} was removed from the spec map, so {} can no longer \
                     hold for it",
                    kind.as_str()
                ),
            });
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

    const U: StructuralKind = StructuralKind::Untested;

    #[test]
    fn episode_of_recognises_legacy_and_episode_ids_only_for_their_key() {
        assert_eq!(episode_of("specguard:untested:a::B", U, "a::B"), Some(None));
        assert_eq!(
            episode_of("specguard:untested:a::B:17", U, "a::B"),
            Some(Some(17))
        );
        assert_eq!(episode_of("specguard:untested:a::Bc", U, "a::B"), None);
        assert_eq!(episode_of("specguard:untested:a::B:x1", U, "a::B"), None);
        assert_eq!(episode_of("specguard:untested:a::B:", U, "a::B"), None);
        assert_eq!(
            episode_of("specguard:undocumented:a::B:17", U, "a::B"),
            None
        );
    }

    fn no_disposition(_: &str) -> Option<DispositionVerdict> {
        None
    }

    #[test]
    fn next_episode_starts_one_when_nothing_is_recorded() {
        let rows: [(&str, Option<&str>); 0] = [];
        assert_eq!(
            next_episode(U, "k", rows, no_disposition, 100),
            Episode::Start("specguard:untested:k:100".into())
        );
    }

    #[test]
    fn next_episode_keeps_an_open_episode_or_legacy_id() {
        let rows = [("specguard:untested:k:50", Some("k"))];
        assert_eq!(
            next_episode(U, "k", rows, no_disposition, 100),
            Episode::Open("specguard:untested:k:50".into())
        );
        let rows = [("specguard:untested:k", Some("k"))];
        assert_eq!(
            next_episode(U, "k", rows, no_disposition, 100),
            Episode::Open("specguard:untested:k".into())
        );
    }

    #[test]
    fn next_episode_starts_a_new_one_after_a_resolved_closure_even_same_second() {
        let resolved = |_: &str| Some(DispositionVerdict::Resolved);
        let rows = [("specguard:untested:k:100", Some("k"))];
        assert_eq!(
            next_episode(U, "k", rows, resolved, 100),
            Episode::Start("specguard:untested:k:101".into())
        );
        // A resolved LEGACY id recurs as an episode at `now`.
        let rows = [("specguard:untested:k", Some("k"))];
        assert_eq!(
            next_episode(U, "k", rows, resolved, 100),
            Episode::Start("specguard:untested:k:100".into())
        );
    }

    #[test]
    fn next_episode_respects_a_human_closure() {
        let dismissed = |_: &str| Some(DispositionVerdict::Dismissed);
        let rows = [("specguard:untested:k:40", Some("k"))];
        assert_eq!(
            next_episode(U, "k", rows, dismissed, 100),
            Episode::Open("specguard:untested:k:40".into())
        );
    }

    #[test]
    fn next_episode_ignores_rows_recorded_for_another_key() {
        // `specguard:untested:k:5` recorded for key "k:5" (legacy) is not
        // episode 5 of key "k".
        let rows = [("specguard:untested:k:5", Some("k:5"))];
        assert_eq!(
            next_episode(U, "k", rows, no_disposition, 100),
            Episode::Start("specguard:untested:k:100".into())
        );
    }
}
