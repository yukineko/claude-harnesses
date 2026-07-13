//! Deterministic review-ORDER pass — a purely-structural companion to
//! [`crate::review_worthiness`] (which scores how much attention a change
//! WARRANTS) and [`crate::review_brief`] (which digests WHY): this module
//! answers "in what ORDER should a human read the hunks of this diff?".
//!
//! git presents a diff in flat VCS (alphabetical file) order. That is the
//! wrong reading order for a human reviewing a large, AI-generated change:
//! it is far easier to review top-to-bottom when hunks are (a) CLUSTERED so
//! logically-connected hunks sit together, and (b) ORDERED within a cluster
//! so a definition comes before the code that uses it (deps before
//! dependents). This module implements that pass over STRUCTURED inputs
//! (parsed hunks + a caller-supplied define/reference map) so it is fully
//! hermetic/unit-testable without git, blastguard, or harness-core in the
//! loop — the CLI boundary in `main.rs` is what actually gathers those
//! structured inputs from a live diff + source tree (mirroring
//! `review_worthiness`'s pure-core / `--from-git` CLI-boundary split).
//!
//! # Determinism
//!
//! Every function here is PURE: no I/O, no wall-clock, no randomness, no
//! hash-order iteration (`BTreeMap`/`BTreeSet`/sorted `Vec`s only, mirroring
//! `overwatch::review_queue::dedup_findings`'s union-find idiom). The same
//! inputs always produce byte-identical output. Fail-soft throughout:
//! malformed diff lines are skipped, never panicking; a cycle in the
//! dependency graph is broken deterministically rather than looping forever.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

/// One hunk of a unified diff: the file it touches, its new-side line range,
/// and its added/removed body lines (file headers `+++`/`---` are not
/// included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub file: String,
    /// 1-indexed new-side start line (per the `@@ -o,l +n,l @@` header).
    pub new_start: usize,
    /// Number of new-side lines this hunk spans (a missing `,len` in the
    /// header means length `1`, per unified-diff spec).
    pub new_lines: usize,
    /// Body lines added by this hunk (the `+`-prefixed lines, prefix
    /// stripped).
    pub added: Vec<String>,
    /// Body lines removed by this hunk (the `-`-prefixed lines, prefix
    /// stripped).
    pub removed: Vec<String>,
}

/// Parse `(start, len)` out of one unified-diff range field (without its
/// leading `+`/`-` sign), e.g. `"3,7"` -> `(3, 7)`, `"3"` -> `(3, 1)` (a
/// missing `,len` means length `1`). Returns `None` on anything unparseable
/// so the caller can skip the malformed header fail-soft.
fn parse_range(s: &str) -> Option<(usize, usize)> {
    if let Some((start, len)) = s.split_once(',') {
        Some((start.trim().parse().ok()?, len.trim().parse().ok()?))
    } else {
        Some((s.trim().parse().ok()?, 1))
    }
}

/// Parse one `@@ -old_start,old_len +new_start,new_len @@ ...` hunk header
/// line, returning `(new_start, new_lines)`. Returns `None` on any
/// unparseable/malformed header so the caller can skip it fail-soft (never
/// panics).
fn parse_at_header(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@")?.trim_start();
    let mut parts = rest.splitn(3, ' ');
    let old_part = parts.next()?;
    let new_part = parts.next()?;
    let _old = parse_range(old_part.strip_prefix('-')?)?;
    let new = parse_range(new_part.strip_prefix('+')?)?;
    Some(new)
}

/// PURE parser of unified-diff text into a flat, ordered [`Vec<Hunk>`].
/// Tracks the current file from `+++ b/<path>` lines (the `b/` prefix is
/// stripped; a deleted-file `+++ /dev/null` clears the current file to an
/// empty string rather than erroring). Each `@@ -o,l +n,l @@` header starts
/// a fresh [`Hunk`] against the current file; subsequent `+`/`-` body lines
/// (excluding the `+++`/`---` file headers themselves) are collected into
/// `added`/`removed` until the next header or file boundary. Any line that
/// doesn't fit one of these shapes (context lines, `diff --git`/`index ...`
/// metadata, `\ No newline at end of file`, garbage) is silently skipped —
/// never panics, never errors, on any input including empty text.
pub fn parse_diff(diff_text: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_file = String::new();

    for line in diff_text.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.trim();
            current_file = if path == "/dev/null" {
                String::new()
            } else if let Some(stripped) = path.strip_prefix("b/") {
                stripped.to_string()
            } else {
                path.to_string()
            };
            continue;
        }
        if line.starts_with("--- ") {
            // Old-side file header: never sets the CURRENT (new-side) file.
            continue;
        }
        if line.starts_with("@@") {
            if let Some((new_start, new_lines)) = parse_at_header(line) {
                hunks.push(Hunk {
                    file: current_file.clone(),
                    new_start,
                    new_lines,
                    added: Vec::new(),
                    removed: Vec::new(),
                });
            }
            // A malformed `@@` header is skipped fail-soft: no hunk starts,
            // and any following `+`/`-` lines simply attach to whatever hunk
            // (if any) was already open.
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            if let Some(h) = hunks.last_mut() {
                h.added.push(rest.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            if let Some(h) = hunks.last_mut() {
                h.removed.push(rest.to_string());
            }
            continue;
        }
        // Context lines (leading space), `diff --git`/`index ...` metadata,
        // `\ No newline at end of file`, and any other shape: skipped.
    }

    hunks
}

/// Return the index of the [`Hunk`] in `hunks` whose `file` matches and
/// whose new-side half-open range `[new_start, new_start + new_lines)`
/// contains `line`; `None` if no hunk owns that line. Deterministic: returns
/// the first match in slice order (ranges for a single file do not overlap
/// in a well-formed diff, so this is unambiguous in practice).
pub fn hunk_containing(hunks: &[Hunk], file: &str, line: usize) -> Option<usize> {
    hunks
        .iter()
        .position(|h| h.file == file && line >= h.new_start && line < h.new_start + h.new_lines)
}

/// One directed dependency edge between two hunks: `from` (the hunk that
/// DEFINES a symbol) must be reviewed before `to` (the hunk that REFERENCES
/// it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReviewEdge {
    pub from: usize,
    pub to: usize,
}

/// PURE edge builder. `defines[i] = (hunk_index, symbol names that hunk
/// DEFINES)`. `refs[sym] = the (file, line) sites that reference `sym``. For
/// each defined symbol of a definer hunk, for each reference site, resolve
/// the referencing hunk via [`hunk_containing`]; if found and it is a
/// DIFFERENT hunk from the definer, emit an edge `from = definer, to =
/// referencer`. A self-reference (a hunk referencing a symbol it itself
/// defines) is skipped. Deduplicated and returned sorted by `(from, to)` for
/// determinism.
pub fn build_edges(
    hunks: &[Hunk],
    defines: &[(usize, Vec<String>)],
    refs: &BTreeMap<String, Vec<(String, usize)>>,
) -> Vec<ReviewEdge> {
    let mut set: BTreeSet<(usize, usize)> = BTreeSet::new();

    for (definer_idx, symbols) in defines {
        for sym in symbols {
            let Some(sites) = refs.get(sym) else {
                continue;
            };
            for (file, line) in sites {
                let Some(referencer_idx) = hunk_containing(hunks, file, *line) else {
                    continue;
                };
                if referencer_idx == *definer_idx {
                    continue; // self-reference: skipped
                }
                set.insert((*definer_idx, referencer_idx));
            }
        }
    }

    set.into_iter()
        .map(|(from, to)| ReviewEdge { from, to })
        .collect()
}

/// One hunk placed into the final review order: its original index, the
/// cluster it was grouped into, its global read `position`, an echo of its
/// file/new-side range, and the symbols it defines (if any).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderedHunk {
    pub hunk_index: usize,
    pub cluster: usize,
    pub position: usize,
    pub file: String,
    pub new_start: usize,
    pub new_lines: usize,
    pub defines: Vec<String>,
}

/// Deterministic union-find `find` with path compression, mirroring
/// `overwatch::review_queue::dedup_findings`'s idiom.
fn uf_find(parent: &mut [usize], x: usize) -> usize {
    if parent[x] != x {
        parent[x] = uf_find(parent, parent[x]);
    }
    parent[x]
}

/// Deterministic union: always attach the higher root to the lower one, so
/// the resulting root for any component is stable regardless of union order.
fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        if ra < rb {
            parent[rb] = ra;
        } else {
            parent[ra] = rb;
        }
    }
}

/// A hunk's stable sort key: `(file, new_start, hunk_index)`. Used both to
/// pick each cluster's representative and, within a cluster, to break ties
/// among in-degree-zero nodes (and to break cycles) deterministically.
fn hunk_key(hunks: &[Hunk], idx: usize) -> (String, usize, usize) {
    (hunks[idx].file.clone(), hunks[idx].new_start, idx)
}

/// Kahn's-algorithm topological order over one cluster's `members`,
/// restricted to the directed `edges` whose endpoints are both in the
/// cluster. Among in-degree-zero candidates, always picks the smallest
/// `hunk_key` (a stable min-selection, never hash order). If every remaining
/// node has nonzero in-degree (a cycle), breaks it deterministically by
/// taking the smallest-key remaining node anyway and continuing — never
/// panics, never loops forever (`remaining` strictly shrinks every
/// iteration).
fn topo_order_cluster(hunks: &[Hunk], members: &[usize], edges: &[(usize, usize)]) -> Vec<usize> {
    let member_set: BTreeSet<usize> = members.iter().copied().collect();

    let mut indegree: BTreeMap<usize, usize> = members.iter().map(|&m| (m, 0)).collect();
    let mut adj: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(from, to) in edges {
        if member_set.contains(&from) && member_set.contains(&to) {
            adj.entry(from).or_default().push(to);
            *indegree.entry(to).or_insert(0) += 1;
        }
    }

    let mut remaining: BTreeSet<usize> = member_set;
    let mut order = Vec::with_capacity(members.len());

    while !remaining.is_empty() {
        let zero_indegree: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|n| *indegree.get(n).unwrap_or(&0) == 0)
            .collect();
        let pick = if !zero_indegree.is_empty() {
            zero_indegree
                .into_iter()
                .min_by_key(|&n| hunk_key(hunks, n))
                .expect("non-empty checked above")
        } else {
            // Cycle: no in-degree-zero candidate remains. Break it
            // deterministically by taking the smallest-key remaining node.
            *remaining
                .iter()
                .min_by_key(|&&n| hunk_key(hunks, n))
                .expect("remaining is non-empty (loop guard)")
        };
        remaining.remove(&pick);
        order.push(pick);
        if let Some(succs) = adj.get(&pick) {
            for &s in succs {
                if remaining.contains(&s) {
                    if let Some(d) = indegree.get_mut(&s) {
                        *d = d.saturating_sub(1);
                    }
                }
            }
        }
    }

    order
}

/// PURE, DETERMINISTIC orderer: turns `hunks` + their `edges` (dependency
/// graph) + `defines` (echoed through into the output) into a flat
/// human review order. Steps:
///
/// 1. **Clusters** — weakly-connected components over the undirected edge
///    graph (union-find over hunk indices). A hunk with no edges is its own
///    singleton cluster. Clusters are numbered/emitted in ascending order of
///    their representative — the member with the smallest `(file,
///    new_start, hunk_index)` key — so cluster ids and emission order are
///    stable across calls.
/// 2. **Within each cluster**, a Kahn topological order over the DIRECTED
///    edges restricted to that cluster puts definitions before referencers,
///    breaking ties (and cycles) via the same smallest-key rule
///    (see [`topo_order_cluster`]).
/// 3. Hunks are emitted cluster-by-cluster (clusters in representative-key
///    order), each cluster in its topo order, assigning a global `position`
///    `0..n` and the cluster id, carrying that hunk's `defines`.
///
/// Determinism contract: the same `(hunks, edges, defines)` always yields
/// byte-identical output.
pub fn order_hunks(
    hunks: &[Hunk],
    edges: &[ReviewEdge],
    defines: &[(usize, Vec<String>)],
) -> Vec<OrderedHunk> {
    let n = hunks.len();
    if n == 0 {
        return Vec::new();
    }

    let mut defines_by_idx: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (idx, syms) in defines {
        defines_by_idx
            .entry(*idx)
            .or_default()
            .extend(syms.iter().cloned());
    }

    let mut parent: Vec<usize> = (0..n).collect();
    for e in edges {
        if e.from < n && e.to < n {
            uf_union(&mut parent, e.from, e.to);
        }
    }

    // Group members by root (a BTreeMap keyed on the root index avoids any
    // hash-order dependence; within each group, indices are pushed in
    // ascending index order).
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = uf_find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }

    let mut edges_by_root: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
    for e in edges {
        if e.from < n && e.to < n {
            let root = uf_find(&mut parent, e.from);
            edges_by_root.entry(root).or_default().push((e.from, e.to));
        }
    }

    // Sort clusters by their representative's (file, new_start, hunk_index)
    // key so cluster ids / emission order are stable.
    let mut cluster_reps: Vec<((String, usize, usize), usize)> = groups
        .iter()
        .map(|(root, members)| {
            let rep_key = members
                .iter()
                .map(|&i| hunk_key(hunks, i))
                .min()
                .expect("group is non-empty");
            (rep_key, *root)
        })
        .collect();
    cluster_reps.sort();

    let mut out = Vec::with_capacity(n);
    let mut position = 0usize;
    for (cluster_id, (_, root)) in cluster_reps.into_iter().enumerate() {
        let members = groups.get(&root).cloned().unwrap_or_default();
        let cluster_edges = edges_by_root.get(&root).cloned().unwrap_or_default();
        let topo = topo_order_cluster(hunks, &members, &cluster_edges);
        for idx in topo {
            out.push(OrderedHunk {
                hunk_index: idx,
                cluster: cluster_id,
                position,
                file: hunks[idx].file.clone(),
                new_start: hunks[idx].new_start,
                new_lines: hunks[idx].new_lines,
                defines: defines_by_idx.get(&idx).cloned().unwrap_or_default(),
            });
            position += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(file: &str, new_start: usize, new_lines: usize) -> Hunk {
        Hunk {
            file: file.to_string(),
            new_start,
            new_lines,
            added: Vec::new(),
            removed: Vec::new(),
        }
    }

    // --- order_hunks: dependency order must BEAT file order ---

    #[test]
    fn order_hunks_dependency_overrides_file_order() {
        // The definer lives in `z_defs.rs` (sorts LAST alphabetically); the
        // referencer lives in `a_uses.rs` (sorts FIRST). A flat VCS/file-order
        // pass — the very thing this feature replaces — would place the
        // referencer (a_uses.rs) before the definer (z_defs.rs). The
        // dependency edge definer->referencer MUST override that so the
        // definer is read first. This is the test that actually discriminates
        // dependency ordering from file ordering (the other order_hunks tests
        // happen to have the definer sort first by file, so they pass under
        // either behaviour).
        let hunks = vec![
            hunk("a_uses.rs", 1, 3), // idx 0 = referencer (file sorts FIRST)
            hunk("z_defs.rs", 1, 3), // idx 1 = definer    (file sorts LAST)
        ];
        let edges = vec![ReviewEdge { from: 1, to: 0 }]; // definer z -> referencer a
        let defines = vec![(1usize, vec!["helper".to_string()])];

        let out = order_hunks(&hunks, &edges, &defines);
        let pos_definer = out.iter().find(|o| o.hunk_index == 1).unwrap().position;
        let pos_referencer = out.iter().find(|o| o.hunk_index == 0).unwrap().position;
        assert!(
            pos_definer < pos_referencer,
            "definer (z_defs.rs, sorts LAST by file) must precede referencer \
             (a_uses.rs, sorts FIRST) because of the dependency edge, not file order"
        );
        // They are connected by the edge, so they share one cluster.
        let c_definer = out.iter().find(|o| o.hunk_index == 1).unwrap().cluster;
        let c_referencer = out.iter().find(|o| o.hunk_index == 0).unwrap().cluster;
        assert_eq!(c_definer, c_referencer);
    }

    // --- parse_diff ---

    #[test]
    fn parse_diff_single_file_multi_hunk() {
        let diff = "\
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -1,2 +1,3 @@
 fn a() {}
+fn b() {}
 fn c() {}
@@ -10,1 +11,2 @@
-fn old() {}
+fn new_one() {}
+fn new_two() {}
";
        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].file, "src/foo.rs");
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[0].new_lines, 3);
        assert_eq!(hunks[0].added, vec!["fn b() {}".to_string()]);
        assert_eq!(hunks[1].file, "src/foo.rs");
        assert_eq!(hunks[1].new_start, 11);
        assert_eq!(hunks[1].new_lines, 2);
        assert_eq!(
            hunks[1].added,
            vec!["fn new_one() {}".to_string(), "fn new_two() {}".to_string()]
        );
        assert_eq!(hunks[1].removed, vec!["fn old() {}".to_string()]);
    }

    #[test]
    fn parse_diff_multi_file() {
        let diff = "\
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,1 +1,2 @@
 fn a() {}
+fn a2() {}
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,1 +1,2 @@
 fn b() {}
+fn b2() {}
";
        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].file, "src/a.rs");
        assert_eq!(hunks[1].file, "src/b.rs");
    }

    #[test]
    fn parse_diff_at_header_without_explicit_lengths_defaults_to_one() {
        let diff = "\
--- a/x.rs
+++ b/x.rs
@@ -5 +5 @@
-old line
+new line
";
        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].new_start, 5);
        assert_eq!(hunks[0].new_lines, 1);
    }

    #[test]
    fn parse_diff_empty_input_is_empty() {
        assert!(parse_diff("").is_empty());
    }

    #[test]
    fn parse_diff_dev_null_new_side_clears_current_file() {
        let diff = "\
--- a/deleted.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn gone() {}
-fn also_gone() {}
";
        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file, "");
        assert_eq!(hunks[0].new_lines, 0);
    }

    #[test]
    fn parse_diff_garbage_lines_are_skipped_fail_soft() {
        let diff = "\
this is not a diff at all
diff --git a/x.rs b/x.rs
index abc123..def456 100644
--- a/x.rs
+++ b/x.rs
@@ garbled header not parseable @@
@@ -1,1 +1,2 @@
 fn a() {}
+fn b() {}
\\ No newline at end of file
";
        let hunks = parse_diff(diff);
        assert_eq!(
            hunks.len(),
            1,
            "only the well-formed @@ header starts a hunk"
        );
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[0].new_lines, 2);
        assert_eq!(hunks[0].added, vec!["fn b() {}".to_string()]);
    }

    // --- hunk_containing ---

    #[test]
    fn hunk_containing_boundary() {
        let hunks = vec![hunk("a.rs", 10, 5)]; // covers lines [10, 15)
        assert_eq!(hunk_containing(&hunks, "a.rs", 10), Some(0));
        assert_eq!(hunk_containing(&hunks, "a.rs", 14), Some(0));
        assert_eq!(
            hunk_containing(&hunks, "a.rs", 15),
            None,
            "exclusive upper bound"
        );
        assert_eq!(hunk_containing(&hunks, "a.rs", 9), None, "below range");
        assert_eq!(hunk_containing(&hunks, "b.rs", 12), None, "wrong file");
    }

    // --- build_edges ---

    #[test]
    fn build_edges_one_edge_for_cross_hunk_reference() {
        let hunks = vec![hunk("a.rs", 1, 3), hunk("b.rs", 1, 3)];
        let defines = vec![(0usize, vec!["helper".to_string()])];
        let mut refs: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        refs.insert("helper".to_string(), vec![("b.rs".to_string(), 2)]);
        let edges = build_edges(&hunks, &defines, &refs);
        assert_eq!(edges, vec![ReviewEdge { from: 0, to: 1 }]);
    }

    #[test]
    fn build_edges_self_reference_is_skipped() {
        let hunks = vec![hunk("a.rs", 1, 3)];
        let defines = vec![(0usize, vec!["helper".to_string()])];
        let mut refs: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        refs.insert("helper".to_string(), vec![("a.rs".to_string(), 2)]);
        let edges = build_edges(&hunks, &defines, &refs);
        assert!(
            edges.is_empty(),
            "a hunk referencing its own def is not an edge"
        );
    }

    #[test]
    fn build_edges_unowned_reference_line_yields_no_edge() {
        let hunks = vec![hunk("a.rs", 1, 3), hunk("b.rs", 1, 3)];
        let defines = vec![(0usize, vec!["helper".to_string()])];
        let mut refs: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        // Line 99 in b.rs isn't covered by any hunk.
        refs.insert("helper".to_string(), vec![("b.rs".to_string(), 99)]);
        let edges = build_edges(&hunks, &defines, &refs);
        assert!(edges.is_empty());
    }

    #[test]
    fn build_edges_deduplicates_and_sorts() {
        let hunks = vec![hunk("a.rs", 1, 3), hunk("b.rs", 1, 5)];
        let defines = vec![(0usize, vec!["helper".to_string(), "other".to_string()])];
        let mut refs: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        refs.insert(
            "helper".to_string(),
            vec![("b.rs".to_string(), 2), ("b.rs".to_string(), 3)],
        );
        refs.insert("other".to_string(), vec![("b.rs".to_string(), 4)]);
        let edges = build_edges(&hunks, &defines, &refs);
        assert_eq!(edges, vec![ReviewEdge { from: 0, to: 1 }]);
    }

    // --- order_hunks ---

    #[test]
    fn order_hunks_definer_before_referencer() {
        let hunks = vec![hunk("a.rs", 1, 3), hunk("b.rs", 1, 3)];
        let edges = vec![ReviewEdge { from: 0, to: 1 }];
        let defines = vec![(0usize, vec!["helper".to_string()])];
        let out = order_hunks(&hunks, &edges, &defines);
        assert_eq!(out.len(), 2);
        let definer_pos = out.iter().find(|o| o.hunk_index == 0).unwrap().position;
        let referencer_pos = out.iter().find(|o| o.hunk_index == 1).unwrap().position;
        assert!(definer_pos < referencer_pos);
        // Both land in the same cluster.
        assert_eq!(out[0].cluster, out[1].cluster);
    }

    #[test]
    fn order_hunks_stable_cluster_order_for_unrelated_files() {
        // No edges: two singleton clusters, ordered by (file, new_start).
        let hunks = vec![hunk("z.rs", 1, 2), hunk("a.rs", 1, 2)];
        let out = order_hunks(&hunks, &[], &[]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].file, "a.rs", "a.rs sorts before z.rs");
        assert_eq!(out[0].cluster, 0);
        assert_eq!(out[1].file, "z.rs");
        assert_eq!(out[1].cluster, 1);
    }

    #[test]
    fn order_hunks_cycle_never_panics_and_is_deterministic() {
        let hunks = vec![hunk("a.rs", 1, 3), hunk("b.rs", 1, 3), hunk("c.rs", 1, 3)];
        // A -> B -> C -> A: a mutual-reference cycle.
        let edges = vec![
            ReviewEdge { from: 0, to: 1 },
            ReviewEdge { from: 1, to: 2 },
            ReviewEdge { from: 2, to: 0 },
        ];
        let one = order_hunks(&hunks, &edges, &[]);
        let two = order_hunks(&hunks, &edges, &[]);
        assert_eq!(one, two, "must be deterministic across calls");
        assert_eq!(one.len(), 3, "every hunk still appears exactly once");
        let mut indices: Vec<usize> = one.iter().map(|o| o.hunk_index).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn order_hunks_singletons_order_by_file_then_new_start() {
        let hunks = vec![hunk("a.rs", 20, 2), hunk("a.rs", 5, 2), hunk("b.rs", 1, 2)];
        let out = order_hunks(&hunks, &[], &[]);
        let files_starts: Vec<(String, usize)> =
            out.iter().map(|o| (o.file.clone(), o.new_start)).collect();
        assert_eq!(
            files_starts,
            vec![
                ("a.rs".to_string(), 5),
                ("a.rs".to_string(), 20),
                ("b.rs".to_string(), 1),
            ]
        );
    }

    #[test]
    fn order_hunks_is_deterministic_same_inputs_twice() {
        let hunks = vec![hunk("b.rs", 1, 3), hunk("a.rs", 1, 3), hunk("a.rs", 10, 3)];
        let edges = vec![ReviewEdge { from: 1, to: 2 }];
        let defines = vec![(1usize, vec!["x".to_string()])];
        let one = order_hunks(&hunks, &edges, &defines);
        let two = order_hunks(&hunks, &edges, &defines);
        assert_eq!(one, two);
        assert_eq!(
            serde_json::to_string(&one).unwrap(),
            serde_json::to_string(&two).unwrap()
        );
    }
}
