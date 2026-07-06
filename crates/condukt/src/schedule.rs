//! The deterministic core: validate a decomposition and compute a schedule.
//!
//! This is the work the LLM should NOT do by eyeballing. Given tasks with
//! `touched_files`, `deps`, and a `class`, we:
//!   1. force `serial`/`gated` tasks (and anything touching a configured shared
//!      glob) off the parallel track,
//!   2. layer the remaining tasks by dependency depth, and
//!   3. within each layer, group tasks with no pairwise file conflict into
//!      parallel batches (greedy graph coloring).
//!
//! All functions are pure and deterministic (stable ordering by id), so the
//! same decomposition always yields the same schedule.

use crate::model::{Batch, Class, Decomposition, Schedule, Task};
use blastguard::classify::classify;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{HashMap, HashSet};

const GLOB_META: [char; 4] = ['*', '?', '[', '{'];

fn is_glob(p: &str) -> bool {
    p.contains(GLOB_META)
}

/// The literal portion of a pattern before its first glob metacharacter.
fn pattern_prefix(p: &str) -> &str {
    match p.find(GLOB_META) {
        Some(i) => &p[..i],
        None => p,
    }
}

/// Do two individual path/glob entries conflict (could touch the same file)?
///
/// Conservative: when uncertain we say "yes", because a false conflict only
/// serializes work (safe) whereas a missed conflict races two workers on one
/// file (unsafe).
fn entries_conflict(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // glob-vs-literal: does either pattern match the other as a path?
    if let Ok(g) = Glob::new(a) {
        if g.compile_matcher().is_match(b) {
            return true;
        }
    }
    if let Ok(g) = Glob::new(b) {
        if g.compile_matcher().is_match(a) {
            return true;
        }
    }
    // glob-vs-glob: if at least one side is a glob and their literal prefixes
    // nest, the wildcard regions can overlap.
    if is_glob(a) || is_glob(b) {
        let (pa, pb) = (pattern_prefix(a), pattern_prefix(b));
        if !pa.is_empty() && !pb.is_empty() && (pa.starts_with(pb) || pb.starts_with(pa)) {
            return true;
        }
    }
    false
}

/// Do two tasks' file sets conflict?
pub fn files_conflict(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.iter().any(|y| entries_conflict(x, y)))
}

fn build_globset(globs: &[String]) -> Option<GlobSet> {
    if globs.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = Glob::new(g) {
            builder.add(glob);
        }
    }
    builder.build().ok()
}

/// Longest dependency chain ending at each task (0 = no deps). Cycles are
/// guarded so this terminates even on malformed input (validate() reports them).
fn compute_depths(dec: &Decomposition) -> HashMap<String, usize> {
    let map: HashMap<&str, &Task> = dec.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut depth: HashMap<String, usize> = HashMap::new();

    fn dfs(
        id: &str,
        map: &HashMap<&str, &Task>,
        depth: &mut HashMap<String, usize>,
        stack: &mut HashSet<String>,
    ) -> usize {
        if let Some(d) = depth.get(id) {
            return *d;
        }
        if !stack.insert(id.to_string()) {
            return 0; // cycle: break, validate() will flag it
        }
        let d = match map.get(id) {
            Some(t) if !t.deps.is_empty() => {
                1 + t
                    .deps
                    .iter()
                    .map(|dep| dfs(dep, map, depth, stack))
                    .max()
                    .unwrap_or(0)
            }
            _ => 0,
        };
        stack.remove(id);
        depth.insert(id.to_string(), d);
        d
    }

    let mut stack = HashSet::new();
    let ids: Vec<String> = dec.tasks.iter().map(|t| t.id.clone()).collect();
    for id in ids {
        dfs(&id, &map, &mut depth, &mut stack);
    }
    depth
}

/// Greedy coloring: pack tasks into the fewest groups such that no two tasks in
/// a group have conflicting file sets. Deterministic (sorted by id).
fn color_by_conflict(layer: &[&Task]) -> Vec<Vec<String>> {
    let mut tasks: Vec<&Task> = layer.to_vec();
    tasks.sort_by(|a, b| a.id.cmp(&b.id));

    let mut groups: Vec<Vec<&Task>> = Vec::new();
    'next: for t in tasks {
        for group in groups.iter_mut() {
            if group
                .iter()
                .all(|o| !files_conflict(&t.touched_files, &o.touched_files))
            {
                group.push(t);
                continue 'next;
            }
        }
        groups.push(vec![t]);
    }

    groups
        .into_iter()
        .map(|g| {
            let mut ids: Vec<String> = g.into_iter().map(|t| t.id.clone()).collect();
            ids.sort();
            ids
        })
        .collect()
}

/// Detect a dependency cycle, returning the offending path if any.
fn find_cycle(dec: &Decomposition) -> Option<Vec<String>> {
    let map: HashMap<&str, &Task> = dec.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut color: HashMap<String, u8> = HashMap::new(); // 0 white, 1 gray, 2 black
    let mut path: Vec<String> = Vec::new();

    fn dfs(
        id: &str,
        map: &HashMap<&str, &Task>,
        color: &mut HashMap<String, u8>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        color.insert(id.to_string(), 1);
        path.push(id.to_string());
        if let Some(t) = map.get(id) {
            for d in &t.deps {
                match color.get(d).copied().unwrap_or(0) {
                    1 => {
                        let mut cyc = path.clone();
                        cyc.push(d.clone());
                        return Some(cyc);
                    }
                    0 => {
                        if let Some(c) = dfs(d, map, color, path) {
                            return Some(c);
                        }
                    }
                    _ => {}
                }
            }
        }
        path.pop();
        color.insert(id.to_string(), 2);
        None
    }

    for t in &dec.tasks {
        if color.get(&t.id).copied().unwrap_or(0) == 0 {
            if let Some(c) = dfs(&t.id, &map, &mut color, &mut path) {
                return Some(c);
            }
        }
    }
    None
}

/// Validate a decomposition. Returns human-readable errors (empty = valid).
pub fn validate(dec: &Decomposition) -> Vec<String> {
    let mut errs = Vec::new();
    if dec.tasks.is_empty() {
        errs.push("decomposition has no tasks".into());
    }
    let mut ids = HashSet::new();
    for t in &dec.tasks {
        if t.id.trim().is_empty() {
            errs.push("a task has an empty id".into());
        } else if !ids.insert(t.id.as_str()) {
            errs.push(format!("duplicate task id: {}", t.id));
        }
    }
    for t in &dec.tasks {
        for d in &t.deps {
            if !ids.contains(d.as_str()) {
                errs.push(format!("task '{}' depends on unknown id '{}'", t.id, d));
            }
            if d == &t.id {
                errs.push(format!("task '{}' depends on itself", t.id));
            }
        }
    }
    if let Some(cycle) = find_cycle(dec) {
        errs.push(format!("dependency cycle: {}", cycle.join(" -> ")));
    }
    errs
}

/// The haystack the risk classifier inspects for a task: its title,
/// done-criteria, and touched files joined together, so a deploy/push signal in
/// any of them trips the deterministic force-gate.
fn task_action_text(t: &Task) -> String {
    let mut s = t.title.clone();
    if let Some(dc) = &t.done_criteria {
        s.push(' ');
        s.push_str(dc);
    }
    for f in &t.touched_files {
        s.push(' ');
        s.push_str(f);
    }
    s
}

/// Compute the deterministic schedule. `shared_globs` come from config: any
/// parallel task touching one is demoted to serial. A high-risk irreversible
/// action (deploy/push/release, per [`blastguard::classify`]) is force-gated
/// regardless of its declared `class`.
pub fn schedule(dec: &Decomposition, shared_globs: &[String]) -> Schedule {
    let mut sched = Schedule::default();
    let shared = build_globset(shared_globs);

    let mut gated: Vec<String> = Vec::new();
    let mut experiment: Vec<String> = Vec::new();
    let mut forced_serial: HashSet<String> = HashSet::new();

    for t in &dec.tasks {
        // Deterministic force-gate (closes the LLM under-tag hole): a high-risk
        // irreversible action — deploy / push / release — is quarantined under
        // `gated` regardless of the LLM-declared `class`. blastguard's graded
        // classifier is the single source of the risk/reversibility axes, so a
        // deploy mislabelled `parallel` can never slip past the only outward gate.
        if classify(&task_action_text(t)).requires_gate() {
            gated.push(t.id.clone());
            if !matches!(t.class, Class::Gated) {
                sched.warnings.push(format!(
                    "task '{}' force-gated: high-risk irreversible action (declared class {:?})",
                    t.id, t.class
                ));
            }
            continue;
        }
        match t.class {
            Class::Gated => gated.push(t.id.clone()),
            Class::Experiment => {
                experiment.push(t.id.clone());
                sched.warnings.push(format!(
                    "task '{}' is an experiment -> not auto-merged",
                    t.id
                ));
            }
            Class::Serial => {
                forced_serial.insert(t.id.clone());
            }
            Class::Parallel => {
                if let Some(gs) = &shared {
                    if t.touched_files.iter().any(|f| gs.is_match(f)) {
                        forced_serial.insert(t.id.clone());
                        sched
                            .warnings
                            .push(format!("task '{}' touches a shared path -> serial", t.id));
                    }
                }
            }
        }
    }

    gated.sort();
    sched.gated = gated.clone();
    experiment.sort();
    sched.experiment = experiment.clone();
    let excluded: HashSet<&str> = sched
        .gated
        .iter()
        .chain(sched.experiment.iter())
        .map(|s| s.as_str())
        .collect();

    let depth = compute_depths(dec);

    // Parallel-eligible tasks grouped by dependency depth.
    let mut by_depth: HashMap<usize, Vec<&Task>> = HashMap::new();
    for t in &dec.tasks {
        if excluded.contains(t.id.as_str()) || forced_serial.contains(&t.id) {
            continue;
        }
        let d = *depth.get(t.id.as_str()).unwrap_or(&0);
        by_depth.entry(d).or_default().push(t);
    }
    let mut depths: Vec<usize> = by_depth.keys().copied().collect();
    depths.sort_unstable();
    for d in depths {
        for ids in color_by_conflict(&by_depth[&d]) {
            sched.batches.push(Batch { parallel: ids });
        }
    }

    // Serial tasks in dependency order (stable by depth then id).
    let mut serial_ids: Vec<String> = forced_serial.into_iter().collect();
    serial_ids.sort_by(|a, b| {
        let da = depth.get(a.as_str()).unwrap_or(&0);
        let db = depth.get(b.as_str()).unwrap_or(&0);
        da.cmp(db).then_with(|| a.cmp(b))
    });
    sched.serial = serial_ids;

    sched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Class, Decomposition, Task};

    fn task(id: &str, files: &[&str], deps: &[&str], class: Class) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            touched_files: files.iter().map(|s| s.to_string()).collect(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            class,
            suggested_model: None,
            done_criteria: None,
            size: None,
            target_symbols: Vec::new(),
            reproduction_tests: None,
            confidence: None,
            kind: None,
        }
    }

    fn dec(tasks: Vec<Task>) -> Decomposition {
        Decomposition {
            goal: "g".into(),
            tasks,
        }
    }

    #[test]
    fn disjoint_files_run_in_one_parallel_batch() {
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("b", &["src/b.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[]);
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["a", "b"]);
        assert!(s.serial.is_empty());
    }

    #[test]
    fn shared_file_forces_two_batches() {
        // Both touch src/a.rs -> cannot share a batch.
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[]);
        assert_eq!(s.batches.len(), 2);
    }

    #[test]
    fn deps_create_ordered_layers() {
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("b", &["src/b.rs"], &["a"], Class::Parallel),
        ]);
        let s = schedule(&d, &[]);
        assert_eq!(s.batches.len(), 2);
        assert_eq!(s.batches[0].parallel, vec!["a"]);
        assert_eq!(s.batches[1].parallel, vec!["b"]);
    }

    #[test]
    fn deploy_task_tagged_parallel_is_force_gated() {
        // An upstream LLM mis-tags an outward-facing deploy as `parallel`. The
        // deterministic classifier must still quarantine it under `gated` — the
        // only outward gate — instead of letting it into a parallel batch.
        let d = dec(vec![
            task("safe", &["src/a.rs"], &[], Class::Parallel),
            task("deploy-prod", &["release.sh"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[]);

        assert!(
            s.gated.contains(&"deploy-prod".to_string()),
            "a deploy task must be force-gated even when tagged parallel; gated={:?}",
            s.gated,
        );
        let batched: Vec<&String> = s.batches.iter().flat_map(|b| b.parallel.iter()).collect();
        assert!(
            !batched.iter().any(|id| *id == "deploy-prod"),
            "force-gated deploy task must NOT appear in a parallel batch; batched={batched:?}",
        );
        // A force-gate is surfaced as a warning (class was parallel, not gated).
        assert!(
            s.warnings.iter().any(|w| w.contains("deploy-prod")),
            "force-gating must be announced in warnings; warnings={:?}",
            s.warnings,
        );
        // The genuinely-benign parallel task is unaffected.
        assert!(
            batched.iter().any(|id| *id == "safe"),
            "benign task must still batch; batched={batched:?}",
        );
    }

    #[test]
    fn class_serial_and_gated_are_separated() {
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("s", &["models.py"], &[], Class::Serial),
            task("g", &["deploy.sh"], &[], Class::Gated),
        ]);
        let s = schedule(&d, &[]);
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["a"]);
        assert_eq!(s.serial, vec!["s"]);
        assert_eq!(s.gated, vec!["g"]);
    }

    #[test]
    fn shared_glob_demotes_to_serial() {
        let d = dec(vec![
            task("a", &["src/models.py"], &[], Class::Parallel),
            task("b", &["src/b.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &["**/models.py".into()]);
        assert_eq!(s.serial, vec!["a"]);
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["b"]);
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn glob_touched_files_detected_as_conflict() {
        // "src/*" overlaps "src/a.rs".
        let d = dec(vec![
            task("a", &["src/*"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[]);
        assert_eq!(s.batches.len(), 2);
    }

    #[test]
    fn validate_catches_dup_unknown_dep_and_cycle() {
        let dup = dec(vec![
            task("a", &[], &[], Class::Parallel),
            task("a", &[], &[], Class::Parallel),
        ]);
        assert!(validate(&dup).iter().any(|e| e.contains("duplicate")));

        let unknown = dec(vec![task("a", &[], &["zzz"], Class::Parallel)]);
        assert!(validate(&unknown).iter().any(|e| e.contains("unknown")));

        let cyc = dec(vec![
            task("a", &[], &["b"], Class::Parallel),
            task("b", &[], &["a"], Class::Parallel),
        ]);
        assert!(validate(&cyc).iter().any(|e| e.contains("cycle")));
    }

    #[test]
    fn empty_decomposition_is_invalid() {
        assert!(!validate(&dec(vec![])).is_empty());
    }

    #[test]
    fn experiment_is_excluded_from_merge_path() {
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("x", &["src/x.rs"], &[], Class::Experiment),
        ]);
        let s = schedule(&d, &[]);
        // Experiment routed onto its own track, never the auto-merge path.
        assert_eq!(s.experiment, vec!["x"]);
        assert!(!s.batches.iter().any(|b| b.parallel.contains(&"x".into())));
        assert!(!s.serial.contains(&"x".into()));
        assert!(!s.gated.contains(&"x".into()));
        // The parallel sibling is unaffected.
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["a"]);
        // A warning marks it as not auto-merged.
        assert!(s
            .warnings
            .iter()
            .any(|w| w.contains("experiment") && w.contains("not auto-merged")));
    }

    #[test]
    fn experiment_decomposition_validates() {
        let d = dec(vec![task("x", &["src/x.rs"], &[], Class::Experiment)]);
        assert!(validate(&d).is_empty());
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::model::{Class, Decomposition, Task};
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn pt(id: &str, files: Vec<String>, deps: Vec<String>, class: Class) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            touched_files: files,
            deps,
            class,
            ..Default::default()
        }
    }

    fn pd(tasks: Vec<Task>) -> Decomposition {
        Decomposition {
            goal: "g".into(),
            tasks,
        }
    }

    fn batch_index(sched: &Schedule, id: &str) -> Option<usize> {
        sched
            .batches
            .iter()
            .position(|b| b.parallel.contains(&id.to_string()))
    }

    proptest! {
        /// Every input task id appears in exactly one output list (batches, serial, or gated).
        #[test]
        fn all_tasks_accounted_for(n in 1usize..8) {
            let tasks: Vec<Task> = (0..n).map(|i| pt(&format!("t{i}"), vec![], vec![], Class::Parallel)).collect();
            let in_ids: HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
            let sched = schedule(&pd(tasks), &[]);
            let mut out_ids: Vec<String> = sched.batches.iter()
                .flat_map(|b| b.parallel.iter().cloned())
                .chain(sched.serial.iter().cloned())
                .chain(sched.gated.iter().cloned())
                .collect();
            out_ids.sort();
            let mut in_sorted: Vec<String> = in_ids.into_iter().collect();
            in_sorted.sort();
            prop_assert_eq!(out_ids, in_sorted);
        }

        /// All parallel tasks with unique files land in batches (not serial/gated).
        #[test]
        fn parallel_unique_files_in_batches(n in 1usize..8) {
            let tasks: Vec<Task> = (0..n)
                .map(|i| pt(&format!("t{i}"), vec![format!("src/f{i}.rs")], vec![], Class::Parallel))
                .collect();
            let sched = schedule(&pd(tasks.clone()), &[]);
            for t in &tasks {
                let in_batch = sched.batches.iter().any(|b| b.parallel.contains(&t.id));
                prop_assert!(in_batch, "task {} should be in a batch", t.id);
            }
        }

        /// Gated tasks always end up in sched.gated and nowhere else.
        #[test]
        fn gated_tasks_always_in_gated(n in 1usize..6) {
            let tasks: Vec<Task> = (0..n).map(|i| pt(&format!("t{i}"), vec![], vec![], Class::Gated)).collect();
            let sched = schedule(&pd(tasks.clone()), &[]);
            for t in &tasks {
                prop_assert!(sched.gated.contains(&t.id));
                prop_assert!(!sched.serial.contains(&t.id));
                prop_assert!(!sched.batches.iter().any(|b| b.parallel.contains(&t.id)));
            }
        }

        /// Serial tasks never appear in batches or gated.
        #[test]
        fn serial_tasks_never_in_batches_or_gated(n in 1usize..6) {
            let tasks: Vec<Task> = (0..n).map(|i| pt(&format!("t{i}"), vec![], vec![], Class::Serial)).collect();
            let sched = schedule(&pd(tasks.clone()), &[]);
            for t in &tasks {
                prop_assert!(!sched.gated.contains(&t.id));
                prop_assert!(!sched.batches.iter().any(|b| b.parallel.contains(&t.id)));
            }
        }

        /// No file appears in two parallel tasks within the same batch.
        #[test]
        fn no_file_overlap_in_same_batch(n in 2usize..8) {
            let tasks: Vec<Task> = (0..n)
                .map(|i| pt(&format!("t{i}"), vec![format!("src/f{i}.rs")], vec![], Class::Parallel))
                .collect();
            let dec = pd(tasks.clone());
            let sched = schedule(&dec, &[]);
            for batch in &sched.batches {
                let mut seen: HashSet<String> = HashSet::new();
                for tid in &batch.parallel {
                    let t = dec.tasks.iter().find(|t| &t.id == tid).unwrap();
                    for f in &t.touched_files {
                        prop_assert!(seen.insert(f.clone()), "file {f} appears twice in same batch");
                    }
                }
            }
        }

        /// All IDs in schedule output reference valid input task ids.
        #[test]
        fn all_output_ids_valid(n in 1usize..8, class_seed in 0u8..3) {
            let classes = [Class::Parallel, Class::Serial, Class::Gated];
            let c = classes[class_seed as usize % 3];
            let tasks: Vec<Task> = (0..n).map(|i| pt(&format!("t{i}"), vec![], vec![], c)).collect();
            let valid: HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
            let sched = schedule(&pd(tasks), &[]);
            for id in sched.batches.iter().flat_map(|b| &b.parallel)
                .chain(sched.serial.iter()).chain(sched.gated.iter()) {
                prop_assert!(valid.contains(id), "unknown id {id} in output");
            }
        }

        /// Dep ordering: if t1 depends on t0, t0's batch index < t1's batch index.
        #[test]
        fn dep_batch_ordering(fa in "[a-z]{3,5}", fb in "[a-z]{3,5}") {
            if fa == fb { return Ok(()); }
            let tasks = vec![
                pt("t0", vec![format!("src/{fa}.rs")], vec![], Class::Parallel),
                pt("t1", vec![format!("src/{fb}.rs")], vec!["t0".into()], Class::Parallel),
            ];
            let sched = schedule(&pd(tasks), &[]);
            if let (Some(b0), Some(b1)) = (batch_index(&sched, "t0"), batch_index(&sched, "t1")) {
                prop_assert!(b0 < b1, "t0 batch {b0} >= t1 batch {b1}");
            }
        }

        /// Shared glob forces a parallel task into serial.
        #[test]
        fn shared_glob_demotes_to_serial(n in 2usize..5) {
            let tasks: Vec<Task> = (0..n)
                .map(|i| pt(&format!("t{i}"), vec!["src/shared.rs".into()], vec![], Class::Parallel))
                .collect();
            let sched = schedule(&pd(tasks.clone()), &["src/shared.rs".to_string()]);
            for t in &tasks {
                let in_serial = sched.serial.contains(&t.id);
                let in_batch_solo = sched.batches.iter()
                    .any(|b| b.parallel.contains(&t.id) && b.parallel.len() == 1);
                prop_assert!(in_serial || in_batch_solo,
                    "task {} should be serial or solo-batch when touching shared file", t.id);
            }
        }

        /// No duplicates in output lists.
        #[test]
        fn no_duplicate_ids_in_output(n in 1usize..8) {
            let tasks: Vec<Task> = (0..n).map(|i| pt(&format!("t{i}"), vec![], vec![], Class::Parallel)).collect();
            let sched = schedule(&pd(tasks), &[]);
            let all: Vec<String> = sched.batches.iter()
                .flat_map(|b| b.parallel.iter().cloned())
                .chain(sched.serial.iter().cloned())
                .chain(sched.gated.iter().cloned())
                .collect();
            let unique: HashSet<&String> = all.iter().collect();
            prop_assert_eq!(all.len(), unique.len(), "duplicate ids in schedule output");
        }

        /// Mixed classes: total output count = total input count.
        #[test]
        fn mixed_classes_total_preserved(np in 1usize..4, ns in 1usize..3, ng in 1usize..3) {
            let mut tasks: Vec<Task> = vec![];
            for i in 0..np { tasks.push(pt(&format!("p{i}"), vec![], vec![], Class::Parallel)); }
            for i in 0..ns { tasks.push(pt(&format!("s{i}"), vec![], vec![], Class::Serial)); }
            for i in 0..ng { tasks.push(pt(&format!("g{i}"), vec![], vec![], Class::Gated)); }
            let total = tasks.len();
            let sched = schedule(&pd(tasks), &[]);
            let out_count = sched.batches.iter().map(|b| b.parallel.len()).sum::<usize>()
                + sched.serial.len() + sched.gated.len();
            prop_assert_eq!(out_count, total);
        }

        /// Warnings are emitted when a task is demoted by shared glob.
        #[test]
        fn warnings_on_shared_glob_demotion(n in 2usize..5) {
            let tasks: Vec<Task> = (0..n)
                .map(|i| pt(&format!("t{i}"), vec!["shared.rs".into()], vec![], Class::Parallel))
                .collect();
            let sched = schedule(&pd(tasks), &["shared.rs".to_string()]);
            prop_assert!(!sched.warnings.is_empty(), "expected demotion warnings");
        }

        /// Single parallel task → exactly one batch of size one.
        #[test]
        fn single_parallel_task_one_batch(id in "[a-z]{2,5}") {
            let sched = schedule(&pd(vec![pt(&id, vec![], vec![], Class::Parallel)]), &[]);
            prop_assert_eq!(sched.batches.len(), 1);
            prop_assert_eq!(sched.batches[0].parallel.len(), 1);
            prop_assert!(sched.serial.is_empty());
            prop_assert!(sched.gated.is_empty());
        }
    }

    /// Empty decomposition → empty schedule (deterministic sanity check).
    #[test]
    fn empty_decomp_empty_schedule() {
        let sched = schedule(&pd(vec![]), &[]);
        assert!(sched.batches.is_empty());
        assert!(sched.serial.is_empty());
        assert!(sched.gated.is_empty());
    }
}
