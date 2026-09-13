//! Queue-level collision detection: **file-scope overlap** (backlog e73a1fd5)
//! and **near-duplicate titles** (backlog f7b018f8).
//!
//! Both answer the same operational question — "is some other item in this
//! queue the same work, or work that would collide with it?" — which the store
//! could not answer before: [`crate::store`]'s duplicate guard is an EXACT
//! [`crate::task::hashkey`] match (title+project, normalised), so two items
//! phrased differently are two unrelated tasks as far as it is concerned, and
//! nothing anywhere compared file scopes.
//!
//! # Neither judgment blocks
//!
//! Both are ADVISORY and always surfaced: the caller prints the peer's task id
//! and never refuses, skips, or exits non-zero because of them. A human decides
//! what the overlap means. (Blocking is [`crate::store`]'s exact-hashkey guard's
//! job, and it keeps it.)
//!
//! # What the near-duplicate scorer can and cannot see (read this before
//! trusting an empty result)
//!
//! The scorer is [`harness_core::lessons::text_similarity`]: lexical Jaccard
//! over tokens cut on non-alphanumeric characters, keeping tokens of at least 3
//! **bytes**. That tokenizer is built for whitespace-segmented text. Japanese
//! is not whitespace-segmented and kana/kanji are `char::is_alphanumeric`, so a
//! whole Japanese clause collapses into ONE token that can only match another
//! byte-identical clause. **This repo's backlog titles are mostly Japanese, so
//! the scorer is largely inert on them** — see
//! `tests::measured_similarity_of_the_known_duplicate_pair`, which pins the
//! measured score of a pair that is a KNOWN real duplicate and is NOT caught at
//! the [`NEAR_DUPLICATE_THRESHOLD`] in use. The tokenizer defect is filed
//! upstream as backlog 7b6bcfe6; it is not fixed here and the threshold is not
//! bent to paper over it.
//!
//! Consequence, and the reason [`near_duplicate_report`] never renders an empty
//! peer list as silence: **an empty peer set here does NOT mean "checked, no
//! duplicates"** — on Japanese text it mostly means the scorer could not see.

use crate::task::Task;
use serde::Serialize;

/// Jaccard score at or above which two queued titles are surfaced as possible
/// near-duplicates.
///
/// **Provisional, and deliberately not tuned here.** It is `0.6` only because
/// `crates/overwatch/src/lease.rs`'s `POSSIBLE_DUPLICATE_THRESHOLD` — the one
/// other consumer of the same scorer — already uses `0.6`, so the two agree
/// rather than each inventing a number. There is no measurement behind the
/// value in either place. The measured score of the one known-duplicate pair we
/// have (see `tests::measured_similarity_of_the_known_duplicate_pair`) is far
/// BELOW this, and the threshold is still left alone: that gap is a tokenizer
/// defect (backlog 7b6bcfe6), and lowering the threshold far enough to close it
/// would drag in unrelated pairs instead of fixing anything.
pub const NEAR_DUPLICATE_THRESHOLD: f64 = 0.6;

/// Three answers, not two, to "do these two tasks touch the same files?".
///
/// The two-valued version of this question is the fail-open CLAUDE.md §3 names:
/// a `files_conflict`-style `any()` over an empty declaration is `false`, which
/// reads downstream as "checked, no overlap" when the truth is "nobody said".
/// `crates/condukt/src/schedule.rs`'s `Class::Parallel` arm documents the same
/// hazard and resolves it by serializing the undeclared task.
///
/// Deliberately has no `Default`, no `From<bool>` and no `Into<bool>`: the only
/// bool this type will produce is [`ScopeOverlap::must_serialize`], which
/// resolves the undeclared case to the RESTRICTED side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum ScopeOverlap {
    /// At least one side declared no `touched_files` at all. **Not** an
    /// observation about the files: the question was never answerable.
    Undeclared,
    /// Both sides declared a scope, and no entry of one names anything the
    /// other names. This one IS an observation.
    Disjoint,
    /// Both sides declared a scope and the scopes intersect.
    Overlapping,
}

impl ScopeOverlap {
    /// Whether the two tasks must not be run concurrently, collapsing the three
    /// values onto the restricted side: `Undeclared` answers `true` together
    /// with `Overlapping`, because an unknown blast radius is not a small one.
    ///
    /// This is the ONLY bool this type offers, and it exists so that the
    /// permissive collapse (`Undeclared` -> `false`) is not spellable at all.
    #[must_use]
    pub fn must_serialize(self) -> bool {
        matches!(self, ScopeOverlap::Undeclared | ScopeOverlap::Overlapping)
    }

    /// A short human-readable label for CLI output.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ScopeOverlap::Undeclared => "scope undeclared (unknown blast radius)",
            ScopeOverlap::Disjoint => "scopes disjoint",
            ScopeOverlap::Overlapping => "scopes overlap",
        }
    }
}

/// Judge two declared file scopes.
///
/// An empty slice on EITHER side yields [`ScopeOverlap::Undeclared`] — it is
/// never silently answered as `Disjoint`.
pub fn scope_overlap(a: &[String], b: &[String]) -> ScopeOverlap {
    if a.is_empty() || b.is_empty() {
        return ScopeOverlap::Undeclared;
    }
    if a.iter().any(|x| b.iter().any(|y| entries_conflict(x, y))) {
        ScopeOverlap::Overlapping
    } else {
        ScopeOverlap::Disjoint
    }
}

/// Normalise one `touched_files` entry for comparison: trim, drop a leading
/// `./`, drop a trailing `/`.
fn normalize_entry(e: &str) -> &str {
    let e = e.trim();
    let e = e.strip_prefix("./").unwrap_or(e);
    e.strip_suffix('/').unwrap_or(e)
}

/// The literal path prefix of an entry: everything before the first glob
/// metacharacter. `src/*.rs` -> `src`, `src/task.rs` -> `src/task.rs`.
fn literal_prefix(e: &str) -> &str {
    match e.find(['*', '?', '[']) {
        Some(i) => normalize_entry(&e[..i]),
        None => e,
    }
}

/// Whether `parent` names `child` or a directory containing it.
fn path_contains(parent: &str, child: &str) -> bool {
    parent == child
        || child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether two declared entries can name the same file.
///
/// Handles exact spelling, directory containment (`crates/backlog` contains
/// `crates/backlog/src/task.rs`), and the literal prefix of a glob
/// (`crates/backlog/src/*.rs` vs `crates/backlog/src/task.rs`). It does NOT
/// evaluate glob bodies — `src/*.rs` and `src/*.toml` are reported as
/// conflicting because both reduce to `src` — which is the conservative
/// direction: an over-reported overlap costs a human a glance, an under-reported
/// one is the collision this module exists to surface. An entry that normalises
/// to the empty string is likewise treated as conflicting, since it names
/// nothing determinate.
fn entries_conflict(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_entry(a), normalize_entry(b));
    if a.is_empty() || b.is_empty() {
        return true;
    }
    if a == b {
        return true;
    }
    let (pa, pb) = (literal_prefix(a), literal_prefix(b));
    if pa.is_empty() || pb.is_empty() {
        return true;
    }
    path_contains(pa, pb) || path_contains(pb, pa)
}

/// Tasks partitioned by file-scope overlap.
///
/// `undeclared` is a SEPARATE list rather than a pile of singleton groups
/// precisely so a caller cannot read "this task is in a group of one" as "this
/// task collides with nothing": it collides with an unknown set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlapGrouping {
    /// Ids of tasks that declared a scope, grouped so that two ids share a
    /// group iff their scopes overlap transitively. Groups and the ids inside
    /// them are sorted for a stable render.
    pub groups: Vec<Vec<String>>,
    /// Ids of tasks that declared NO scope. Ungroupable — not "disjoint".
    pub undeclared: Vec<String>,
}

/// Group `tasks` by declared file-scope overlap (backlog e73a1fd5).
///
/// Union-find over [`scope_overlap`] `== Overlapping`; tasks with no declared
/// scope never join a group and are reported in
/// [`OverlapGrouping::undeclared`].
pub fn group_by_overlap(tasks: &[Task]) -> OverlapGrouping {
    let declared: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.touched_files.is_empty())
        .collect();
    let undeclared: Vec<String> = tasks
        .iter()
        .filter(|t| t.touched_files.is_empty())
        .map(|t| t.id.clone())
        .collect();

    // Union-find over the declared tasks. `parent[i]` is an index into
    // `declared`; the sets it builds are the transitive closure of
    // `ScopeOverlap::Overlapping`.
    let mut parent: Vec<usize> = (0..declared.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (i, ti) in declared.iter().enumerate() {
        for (j, tj) in declared.iter().enumerate().skip(i + 1) {
            if scope_overlap(&ti.touched_files, &tj.touched_files) == ScopeOverlap::Overlapping {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }

    let mut by_root: std::collections::BTreeMap<usize, Vec<String>> =
        std::collections::BTreeMap::new();
    for (i, t) in declared.iter().enumerate() {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(t.id.clone());
    }
    let mut groups: Vec<Vec<String>> = by_root
        .into_values()
        .map(|mut g| {
            g.sort();
            g
        })
        .collect();
    groups.sort();
    OverlapGrouping { groups, undeclared }
}

/// A queued peer whose title scored at or above the threshold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NearDuplicate {
    pub id: String,
    pub title: String,
    pub similarity: f64,
    /// How this peer's declared file scope relates to the anchor's, rendered
    /// through [`ScopeOverlap::label`] so "undeclared" never reads as
    /// "disjoint".
    pub scope: String,
    /// The same judgment collapsed for a machine consumer, via
    /// [`ScopeOverlap::must_serialize`] — i.e. resolved to the RESTRICTED side,
    /// so a driver reading only this field still serializes an undeclared pair
    /// instead of running it in parallel.
    pub must_serialize: bool,
}

/// Whether a task is still in the queue (so a near-duplicate of it is live
/// work). `done`/`cancelled` items are history and are not surfaced; `claimed`
/// IS surfaced, because "somebody else is already on this" is the single most
/// useful thing this check can say.
pub fn is_queued(t: &Task) -> bool {
    matches!(t.status.as_str(), "pending" | "failed" | "claimed")
}

/// Peers of `anchor` in the same project's queue whose titles score
/// `>= threshold` under [`harness_core::lessons::text_similarity`], strongest
/// first (ties broken by id for a stable render).
///
/// Read this module's docs before treating an empty result as "no duplicates":
/// on Japanese titles the scorer is largely inert.
pub fn near_duplicates(anchor: &Task, tasks: &[Task], threshold: f64) -> Vec<NearDuplicate> {
    let mut found: Vec<NearDuplicate> = tasks
        .iter()
        .filter(|t| t.id != anchor.id && t.project == anchor.project && is_queued(t))
        .filter_map(|t| {
            let similarity = harness_core::lessons::text_similarity(&anchor.title, &t.title);
            (similarity >= threshold).then(|| {
                let overlap = scope_overlap(&anchor.touched_files, &t.touched_files);
                NearDuplicate {
                    id: t.id.clone(),
                    title: t.title.clone(),
                    similarity,
                    scope: overlap.label().to_string(),
                    must_serialize: overlap.must_serialize(),
                }
            })
        })
        .collect();
    found.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    found
}

/// The human-facing render of a near-duplicate scan. **Always non-empty** for a
/// queued anchor: an empty peer list is rendered as an explicit "found nothing,
/// and here is why that is not a clean bill of health" line rather than as
/// silence, because silence is read as "checked, no duplicates" and on Japanese
/// titles that reading is false (module docs; backlog 7b6bcfe6).
pub fn near_duplicate_report(anchor: &Task, peers: &[NearDuplicate], threshold: f64) -> String {
    if peers.is_empty() {
        return format!(
            "near-duplicate scan for {}: no queued peer scored >= {threshold} — this is NOT \
             \"checked, no duplicates\": the scorer is lexical Jaccard over tokens cut on \
             non-alphanumeric characters, so it cannot see near-duplicates in Japanese titles \
             (backlog 7b6bcfe6), which most of this queue's titles are.",
            anchor.id
        );
    }
    let mut out = format!(
        "near-duplicate scan for {}: {} queued peer(s) scored >= {threshold} (advisory — nothing \
         is blocked; a human decides):",
        anchor.id,
        peers.len()
    );
    for p in peers {
        out.push_str(&format!(
            "\n  {}  similarity {:.3}  [{}]  {}",
            p.id, p.similarity, p.scope, p.title
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, title: &str, touched: &[&str]) -> Task {
        Task {
            id: id.to_string(),
            title: title.to_string(),
            project: "/repo".to_string(),
            project_unresolved: false,
            tags: Vec::new(),
            touched_files: touched.iter().map(|s| s.to_string()).collect(),
            status: "pending".to_string(),
            notes: String::new(),
            created_at: 0,
            updated_at: 0,
            defer_until: None,
            weight: 0.0,
            issue_number: None,
            issue_url: None,
            issue_closed_at: None,
        }
    }

    /// The exact title of backlog 91e503c8, read verbatim out of
    /// `.backlog/tasks.toml` at HEAD 1235d60d.
    const TITLE_91E503C8: &str = "donegate の required check が workspace 全体を見るため、他セッションの赤が全セッションを止める (共有 skip ファイルへの構造的圧力 = fail-open vector)";
    /// The exact title of backlog 15eb3f94, same source.
    const TITLE_15EB3F94: &str =
        "donegate/reviewgate の required check を自分の worktree スコープに絞る (workspace 全体スキャンをやめる)";

    // --- (B) three-valued scope overlap -------------------------------------

    /// The whole point of the type: "nobody declared a scope" and "declared a
    /// scope that does not overlap" must be DIFFERENT values, visible to the
    /// caller. Dies the moment `scope_overlap` maps an empty declaration onto
    /// `Disjoint` (the bool collapse).
    #[test]
    fn undeclared_is_a_distinct_answer_from_disjoint() {
        let declared = vec!["crates/backlog/src/task.rs".to_string()];
        let other = vec!["crates/condukt/src/schedule.rs".to_string()];

        assert_eq!(
            scope_overlap(&[], &other),
            ScopeOverlap::Undeclared,
            "an empty declaration on the left must not be answered as an observation"
        );
        assert_eq!(
            scope_overlap(&declared, &[]),
            ScopeOverlap::Undeclared,
            "an empty declaration on the right must not be answered as an observation"
        );
        assert_eq!(scope_overlap(&[], &[]), ScopeOverlap::Undeclared);
        // Anti-vacuity: the other two answers are still reachable, so a
        // function that returned Undeclared unconditionally cannot pass.
        assert_eq!(scope_overlap(&declared, &other), ScopeOverlap::Disjoint);
        assert_eq!(
            scope_overlap(&declared, &declared),
            ScopeOverlap::Overlapping
        );
        assert_ne!(ScopeOverlap::Undeclared, ScopeOverlap::Disjoint);
    }

    /// The one permitted bool collapse resolves the undetermined answer to the
    /// RESTRICTED side (CLAUDE.md §3), matching condukt's "when in doubt,
    /// serialize".
    #[test]
    fn undeclared_collapses_to_the_restricted_side() {
        assert!(ScopeOverlap::Undeclared.must_serialize());
        assert!(ScopeOverlap::Overlapping.must_serialize());
        assert!(!ScopeOverlap::Disjoint.must_serialize());
    }

    /// Entry comparison is not plain string equality: a directory containing a
    /// file, and a glob's literal prefix, both overlap.
    #[test]
    fn overlap_sees_containment_and_glob_prefix() {
        let dir = vec!["crates/backlog/src".to_string()];
        let file = vec!["crates/backlog/src/task.rs".to_string()];
        let glob = vec!["crates/backlog/src/*.rs".to_string()];
        let elsewhere = vec!["crates/backlog/README.md".to_string()];
        assert_eq!(scope_overlap(&dir, &file), ScopeOverlap::Overlapping);
        assert_eq!(scope_overlap(&glob, &file), ScopeOverlap::Overlapping);
        assert_eq!(scope_overlap(&file, &elsewhere), ScopeOverlap::Disjoint);
        // A near-miss sibling directory must NOT be read as containment
        // (`crates/backlog2` does not live inside `crates/backlog`).
        assert_eq!(
            scope_overlap(
                &["crates/backlog".to_string()],
                &["crates/backlog2/src/x.rs".to_string()]
            ),
            ScopeOverlap::Disjoint
        );
    }

    /// Grouping keeps the undeclared tasks OUT of the groups: a singleton group
    /// would say "collides with nobody", which is exactly the claim that cannot
    /// be made about an undeclared scope.
    #[test]
    fn grouping_never_puts_an_undeclared_task_in_a_group() {
        let tasks = vec![
            task("aaaa", "a", &["crates/backlog/src/task.rs"]),
            task("bbbb", "b", &["crates/backlog/src"]),
            task("cccc", "c", &["crates/condukt/src/schedule.rs"]),
            task("dddd", "d", &[]),
        ];
        let g = group_by_overlap(&tasks);
        assert_eq!(
            g.undeclared,
            vec!["dddd".to_string()],
            "the scope-less task must be reported as undeclared, not grouped"
        );
        assert!(
            !g.groups.iter().any(|grp| grp.contains(&"dddd".to_string())),
            "undeclared task leaked into a group: {:?}",
            g.groups
        );
        assert_eq!(
            g.groups,
            vec![
                vec!["aaaa".to_string(), "bbbb".to_string()],
                vec!["cccc".to_string()]
            ],
            "overlapping scopes must group transitively; disjoint ones must not"
        );
    }

    // --- (D) the measurement ------------------------------------------------

    /// **An observation, not a target.** `text_similarity` is measured here on
    /// the two REAL titles of backlog 91e503c8 and 15eb3f94 — a pair a single
    /// session claimed as two separate items on 2026-09-13 and which is a
    /// confirmed duplicate of one piece of work.
    ///
    /// The number asserted below is whatever the scorer actually returns. It is
    /// recorded so that the gap between it and [`NEAR_DUPLICATE_THRESHOLD`] is
    /// a FACT in the repo rather than a claim: this known duplicate pair is NOT
    /// caught at 0.6, because the tokenizer cuts on non-alphanumeric characters
    /// and Japanese text is not whitespace-segmented, so a whole clause becomes
    /// one token (upstream defect: backlog 7b6bcfe6). The threshold is
    /// deliberately left at 0.6 — bending it to make this pair match would hide
    /// the tokenizer defect behind a number, not fix it.
    #[test]
    fn measured_similarity_of_the_known_duplicate_pair() {
        let measured = harness_core::lessons::text_similarity(TITLE_91E503C8, TITLE_15EB3F94);
        // Observed 2026-09-13 in this worktree, `cargo test -p backlog`:
        //   left: 0.2631578947368421   (= 5 shared tokens / 19 in the union)
        // i.e. 0.263, against a threshold of 0.6. The pair is NOT caught.
        assert_eq!(
            measured, 0.2631578947368421,
            "this is an OBSERVATION of what text_similarity returns, not a target"
        );
        assert!(
            measured < NEAR_DUPLICATE_THRESHOLD,
            "recorded fact: the known duplicate pair 91e503c8/15eb3f94 scores {measured}, \
             BELOW the {NEAR_DUPLICATE_THRESHOLD} threshold, so this feature does not catch \
             it (tokenizer defect, backlog 7b6bcfe6). If this assertion ever fails, the \
             scorer changed and the module docs must be re-measured."
        );
    }

    // --- (C) near-duplicate surfacing ---------------------------------------

    #[test]
    fn near_duplicates_surfaces_a_scoring_peer_with_its_id() {
        let anchor = task("aaaa", "add touched_files field to backlog task", &[]);
        let peer = task("bbbb", "add touched_files field to backlog task queue", &[]);
        let unrelated = task("cccc", "rewrite the mutation canary shell script", &[]);
        let tasks = vec![anchor.clone(), peer.clone(), unrelated];
        let found = near_duplicates(&anchor, &tasks, NEAR_DUPLICATE_THRESHOLD);
        assert_eq!(found.len(), 1, "expected exactly the one peer: {found:?}");
        assert_eq!(found[0].id, "bbbb");
        assert!(found[0].similarity >= NEAR_DUPLICATE_THRESHOLD);
        // The anchor never reports itself.
        assert!(!found.iter().any(|p| p.id == "aaaa"));
    }

    /// Anti-vacuity for the filters: a done task, and a task in another
    /// project, are not surfaced even with an identical title.
    #[test]
    fn near_duplicates_ignores_finished_work_and_other_projects() {
        let anchor = task("aaaa", "add touched_files field to backlog task", &[]);
        let mut done = task("bbbb", "add touched_files field to backlog task", &[]);
        done.status = "done".to_string();
        let mut foreign = task("cccc", "add touched_files field to backlog task", &[]);
        foreign.project = "/other".to_string();
        let mut claimed = task("dddd", "add touched_files field to backlog task", &[]);
        claimed.status = "claimed".to_string();
        let tasks = vec![anchor.clone(), done, foreign, claimed];
        let found = near_duplicates(&anchor, &tasks, NEAR_DUPLICATE_THRESHOLD);
        assert_eq!(
            found.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["dddd"],
            "only the live, same-project peer is surfaced: {found:?}"
        );
    }

    /// Each surfaced peer carries the three-valued scope judgment, so a peer
    /// that declared nothing is not rendered as "scopes disjoint".
    #[test]
    fn near_duplicate_carries_the_three_valued_scope() {
        let anchor = task(
            "aaaa",
            "add touched_files field to backlog task",
            &["crates/backlog/src/task.rs"],
        );
        let undeclared = task("bbbb", "add touched_files field to backlog task queue", &[]);
        let tasks = vec![anchor.clone(), undeclared];
        let found = near_duplicates(&anchor, &tasks, NEAR_DUPLICATE_THRESHOLD);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].scope, ScopeOverlap::Undeclared.label());
        assert!(
            found[0].must_serialize,
            "the machine-readable collapse must resolve an undeclared scope to the \
             restricted side"
        );
    }

    /// The report NAMES the peer id (criterion C: always visible to a human),
    /// and never blocks — it is a string, it has no verdict.
    #[test]
    fn report_names_every_peer_id() {
        let anchor = task("aaaa", "add touched_files field to backlog task", &[]);
        let peer = task("bbbb", "add touched_files field to backlog task queue", &[]);
        let tasks = vec![anchor.clone(), peer];
        let found = near_duplicates(&anchor, &tasks, NEAR_DUPLICATE_THRESHOLD);
        let report = near_duplicate_report(&anchor, &found, NEAR_DUPLICATE_THRESHOLD);
        assert!(report.contains("bbbb"), "peer id must be visible: {report}");
        assert!(
            report.contains("near-duplicate"),
            "report must say what it is: {report}"
        );
    }

    /// An empty peer set must NOT render as silence or as a clean bill of
    /// health: it has to say that the scorer is lexical and inert on Japanese
    /// titles, which is what most of this queue is.
    #[test]
    fn empty_peer_set_is_not_reported_as_checked_clean() {
        let anchor = task("aaaa", TITLE_91E503C8, &[]);
        let report = near_duplicate_report(&anchor, &[], NEAR_DUPLICATE_THRESHOLD);
        assert!(
            !report.is_empty(),
            "an empty peer set must still say something"
        );
        let lower = report.to_lowercase();
        assert!(
            lower.contains("not") && lower.contains("japanese"),
            "the empty result must be qualified, not presented as clean: {report}"
        );
        assert!(
            report.contains("7b6bcfe6"),
            "the report must point at the filed tokenizer defect: {report}"
        );
    }
}
