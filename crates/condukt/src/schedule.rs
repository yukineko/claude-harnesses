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
use blastguard::classify::{classify_change, Risk};
use blastguard::diffrisk::SensitiveConfig;
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

/// Does `p` look like it violates the repo-relative `touched_files`
/// convention documented on [`normalize_entry`] — i.e. an absolute path
/// (Unix `/foo`, or a Windows drive letter like `C:\foo` / `C:/foo`) or a
/// path containing a `..` traversal component?
///
/// This is a **sanity check, not a canonicalizer**: it does not resolve or
/// rewrite the entry, it only flags spellings that would silently defeat
/// [`entries_conflict`]'s string-based comparison (e.g. `/etc/passwd` vs
/// `../../etc/passwd` naming the same file but comparing unequal).
fn violates_repo_relative_convention(p: &str) -> bool {
    if p.starts_with('/') || p.starts_with('\\') {
        return true;
    }
    // Windows drive letter: "C:\" or "C:/" (case-insensitive).
    let bytes = p.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
    {
        return true;
    }
    p.split(['/', '\\']).any(|comp| comp == "..")
}

/// Normalize a path/glob entry before comparison: strip a leading `./`,
/// collapse repeated `/`, and casefold (lowercase). Two different spellings of
/// the same path (e.g. `./src/a.rs` vs `src/a.rs`, `src//a.rs` vs `src/a.rs`,
/// or `Src/a.rs` vs `src/a.rs` on a case-insensitive filesystem like macOS's
/// default APFS/HFS+) must compare equal, or a same-file write race goes
/// undetected — a false negative, unlike a false positive which only
/// over-serializes (safe). Casefolding is conservative: it can only merge two
/// spellings into one same-file conflict (never split them), matching the
/// "when uncertain, treat as a conflict" contract of [`entries_conflict`].
/// Does not attempt to resolve `..` or relative-vs-absolute forms;
/// touched_files is a bare repo-relative convention, not a filesystem path to
/// fully canonicalize. (See [`violates_repo_relative_convention`] for a
/// non-canonicalizing sanity check that flags absolute paths / `..` components
/// as a warning instead.)
fn normalize_entry(p: &str) -> String {
    let mut s = p;
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    let mut out = String::with_capacity(s.len());
    let mut last_was_slash = false;
    for c in s.chars() {
        if c == '/' {
            if last_was_slash {
                continue;
            }
            last_was_slash = true;
        } else {
            last_was_slash = false;
        }
        out.push(c);
    }
    // Casefold so case-only spelling differences (e.g. `Src/a.rs` vs
    // `src/a.rs`) fold to the SAME file on case-insensitive filesystems. Glob
    // metacharacters (`* ? [ {`) are not letters, so lowercasing never alters
    // the glob semantics of a pattern entry.
    out.to_lowercase()
}

/// Split a path entry into its non-empty components, tolerating either `/` or
/// `\` separators (a convention-violating entry may use Windows separators).
fn path_components(p: &str) -> Vec<&str> {
    p.split(['/', '\\']).filter(|c| !c.is_empty()).collect()
}

/// Is `short` a component-wise suffix of `long`? (e.g. `[src, a.rs]` is a
/// suffix of `[repo, src, a.rs]`.) Empty `short` never matches.
fn is_path_suffix(long: &[&str], short: &[&str]) -> bool {
    !short.is_empty() && short.len() <= long.len() && long[long.len() - short.len()..] == *short
}

/// Do two individual path/glob entries conflict (could touch the same file)?
///
/// Conservative: when uncertain we say "yes", because a false conflict only
/// serializes work (safe) whereas a missed conflict races two workers on one
/// file (unsafe).
fn entries_conflict(a: &str, b: &str) -> bool {
    let a = &normalize_entry(a);
    let b = &normalize_entry(b);
    if a == b {
        return true;
    }
    // Abs-vs-relative spelling of the SAME file: when one entry violates the
    // repo-relative convention (absolute path / drive-letter / `..`), plain
    // string comparison can't be trusted — e.g. `/repo/src/a.rs` and
    // `src/a.rs` name the identical file yet compare unequal, silently
    // defeating overlap-demotion and racing two workers on one file. Detect
    // it precisely (not by dragging every disjoint peer to serial) via a
    // component-wise path-suffix match, gated on a convention violation so
    // ordinary relative-vs-relative comparisons are untouched.
    if violates_repo_relative_convention(a) || violates_repo_relative_convention(b) {
        let (ca, cb) = (path_components(a), path_components(b));
        if is_path_suffix(&ca, &cb) || is_path_suffix(&cb, &ca) {
            return true;
        }
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
        if !pa.is_empty() && !pb.is_empty() {
            if pa.starts_with(pb) || pb.starts_with(pa) {
                return true;
            }
        } else if is_glob(a) && is_glob(b) {
            // Leading-wildcard glob-vs-glob: at least one glob starts with a
            // wildcard so its literal prefix is empty (e.g. `*.rs` or
            // `**/foo.rs`). The nested-prefix test above short-circuits to
            // "no conflict" here, but two globs cannot be cheaply proven
            // disjoint — globset only matches glob-vs-literal, not
            // glob-vs-glob — and a leading wildcard can reach into any
            // directory (`**/foo.rs` overlaps `bar/**/foo.rs` at
            // `bar/foo.rs`). Conservatively treat two globs where one has an
            // empty literal prefix as a CONFLICT: a false conflict only
            // serializes work (safe), a missed one races two workers onto one
            // file (the bug). Genuinely-disjoint globs with two non-empty,
            // non-nesting prefixes (e.g. `src/foo/*.rs` vs `src/bar/*.rs`) are
            // handled by the branch above and are NOT dragged in here.
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
/// any of them trips the deterministic force-gate. Shared with
/// [`crate::gate_exec::run_gate_check`] so the gate-exec decision classifies the
/// exact same action text this force-gate does (no duplicated join logic).
pub(crate) fn task_action_text(t: &Task) -> String {
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

/// Resolve one task's risk classification into `(force_gate, undetermined_why)`.
///
/// Extracted from [`schedule`] as a pure function so the FAIL-CLOSED arm is
/// directly observable in a test: `schedule` builds its own
/// `SensitiveConfig::default()`, which always compiles, so the undetermined
/// branch is not reachable by feeding `schedule` a decomposition — it is only
/// reachable through a misconfigured glob list. Testing the resolution here is
/// the honest way to pin the invariant rather than leaving it unexercised.
///
/// * `Known(a)`  → gate iff `a.requires_gate() || a.risk >= Medium` (unchanged).
/// * `Undetermined` → **always gate**, carrying the reason so the caller can
///   emit a warning that names the misconfiguration. "Could not measure this
///   task's risk" must never resolve to the permissive `false`.
fn resolve_force_gate(
    d: harness_core::verdict::Determination<blastguard::classify::RiskAssessment>,
) -> (bool, Option<String>) {
    match d.require() {
        harness_core::verdict::Required::Determined(a) => {
            (a.requires_gate() || a.risk >= Risk::Medium, None)
        }
        harness_core::verdict::Required::Blocked(verdict) => (
            true,
            Some(
                verdict
                    .reason()
                    .map(|r| r.as_str().to_string())
                    .unwrap_or_else(|| "risk classification undetermined".to_string()),
            ),
        ),
    }
}

/// Compute the deterministic schedule. `shared_globs` come from config: any
/// parallel task touching one is demoted to serial. A high-risk irreversible
/// action (deploy/push/release, per [`blastguard::classify`]) is force-gated
/// regardless of its declared `class`. Additionally, a task whose
/// `touched_files` hit a configured sensitive-path glob (auth/payment/PII,
/// see [`blastguard::diffrisk::SensitiveConfig`]) is force-gated too, via
/// [`blastguard::classify::classify_change`]. BOUNDED SCOPE: pre-execution
/// there is no diff yet, so only the sensitive-path signal (which needs only
/// `paths`) can fire here — the public-symbol-diff signal needs a real diff
/// and is out of scope for this (schedule-time) call site.
///
/// NOTE on the gate predicate: [`blastguard::classify::RiskAssessment::requires_gate`]
/// is `High && !reversible`, but [`blastguard::diffrisk::classify_diff`]
/// deliberately always reports `reversible: true` for path/diff-derived
/// signals (a human can still revert the commit — see its doc comment), so a
/// sensitive-path hit alone tops out at `Medium`/reversible and can never
/// trip `requires_gate()` on its own. We therefore force-gate on
/// `requires_gate() || risk >= Risk::Medium`, which is exactly the "review
/// required" OR that `classify_diff`'s doc comment anticipates callers add.
///
/// A classification that could NOT be determined (an unparseable configured
/// sensitive-path glob) force-gates as well, via [`resolve_force_gate`], and
/// emits a warning naming the misconfiguration — "could not measure the risk"
/// is never read as "low risk".
///
/// `max_parallel` (`Config::max_parallel`) caps the width of every parallel
/// batch. A batch IS the fan-out width — the skill spawns every id in one batch
/// simultaneously — so this is where "at most N concurrent workers per session"
/// stops being advice and becomes a property of the schedule.
///
/// The cap arrives as a PARAMETER, not read from the environment here: this
/// module's contract is that every function is pure and deterministic (the same
/// decomposition always yields the same `Schedule`). `Config::load` is where the
/// environment is consulted, and it clamps the value there. This function still
/// re-clamps through the pure [`harness_core::parallel::clamp`] rather than
/// trusting its caller — a passed 0 must not produce a scheduler that can never
/// place work, and no caller may exceed the session ceiling.
///
/// Splitting is a PARTITION, never a truncation: an over-wide colour class is
/// cut into consecutive chunks that all still run, in dependency-safe order.
/// The tasks in one colour class are pairwise file-disjoint, so any subset of
/// it is equally safe to run together — cutting it costs wall-clock, not
/// correctness.
pub fn schedule(dec: &Decomposition, shared_globs: &[String], max_parallel: usize) -> Schedule {
    let mut sched = Schedule::default();
    let shared = build_globset(shared_globs);
    let sensitive = SensitiveConfig::default();

    let mut gated: Vec<String> = Vec::new();
    let mut experiment: Vec<String> = Vec::new();
    let mut forced_serial: HashSet<String> = HashSet::new();

    // Sanity-check the repo-relative `touched_files` convention documented on
    // `normalize_entry`. This is deliberately a WARN, not a reject: schedule()
    // is the single-pass deterministic core of condukt, and erroring out here
    // would halt the entire run on a convention violation rather than merely
    // degrading the conflict heuristic it protects (an offending path just
    // doesn't dedupe against equivalent relative spellings, so the affected
    // task falls back on the "when uncertain, treat as no conflict for that
    // one entry" side rather than losing collision detection for the whole
    // batch). Surfacing it as a warning makes the silent-heuristic-defeat
    // failure mode observable without giving false positives (e.g. from a
    // caller that legitimately builds `touched_files` from an absolute-rooted
    // decomposition) the power to stall scheduling. Sorted by id for
    // deterministic warning order.
    let mut convention_ids: Vec<&Task> = dec
        .tasks
        .iter()
        .filter(|t| {
            t.touched_files
                .iter()
                .any(|f| violates_repo_relative_convention(f))
        })
        .collect();
    convention_ids.sort_by(|a, b| a.id.cmp(&b.id));
    for t in convention_ids {
        let mut offenders: Vec<&String> = t
            .touched_files
            .iter()
            .filter(|f| violates_repo_relative_convention(f))
            .collect();
        offenders.sort();
        sched.warnings.push(format!(
            "task '{}' has touched_files entries that are not repo-relative \
             (absolute path or '..' component): {:?} — conflict detection may \
             silently miss same-file overlaps for these entries",
            t.id, offenders
        ));
    }

    for t in &dec.tasks {
        // Deterministic force-gate (closes the LLM under-tag hole): a high-risk
        // irreversible action — deploy / push / release, OR a change touching a
        // sensitive path (auth/payment/PII) — is quarantined under `gated`
        // regardless of the LLM-declared `class`. blastguard's graded
        // classifier is the single source of the risk/reversibility axes, so a
        // deploy mislabelled `parallel` can never slip past the only outward
        // gate, and neither can a sensitive-path change.
        //
        // FAIL-CLOSED on an undetermined classification: `classify_change`
        // returns a `Determination` because its sensitive-path signal can fail
        // to compile (a bad configured glob). "We could not measure this task's
        // risk" is NOT "this task is low risk" — it force-gates, exactly like a
        // measured Medium+, and says so in a warning so the misconfiguration is
        // visible rather than silently widening the gate.
        let (force_gate, undetermined_why) = resolve_force_gate(classify_change(
            &task_action_text(t),
            &t.touched_files,
            "",
            &sensitive,
        ));
        if force_gate {
            gated.push(t.id.clone());
            if let Some(why) = undetermined_why {
                sched.warnings.push(format!(
                    "task '{}' force-gated: risk could not be determined ({why}) — \
                     resolved to the restricted side",
                    t.id
                ));
            } else if !matches!(t.class, Class::Gated) {
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
                if t.touched_files.is_empty() {
                    // Undeclared file set = unknown blast radius. `files_conflict`
                    // is an any()==false over an empty set, so overlap-demotion
                    // can never fire for this task and it would ride a parallel
                    // worktree, colliding invisibly with peers. "When in doubt,
                    // serialize" (conservative): route it onto the serial track.
                    forced_serial.insert(t.id.clone());
                    sched.warnings.push(format!(
                        "task '{}' declares no touched_files (unknown blast radius) -> serial",
                        t.id
                    ));
                } else if t.touched_files.iter().any(|f| {
                    // Shared-glob overlap must be BIDIRECTIONAL. `gs.is_match(f)`
                    // only fires when the touched_files entry `f` is a literal
                    // path caught by a shared glob; it treats a glob-VALUED `f`
                    // (e.g. `src/*.rs`) as an opaque literal string and misses
                    // the case where `f` itself is the glob that overlaps a
                    // (more literal) shared path (e.g. shared `src/models.rs`).
                    // Evaluate `entries_conflict` in both directions against
                    // each configured shared glob so a glob-valued entry that
                    // overlaps a shared path still demotes to serial.
                    shared.as_ref().is_some_and(|gs| gs.is_match(f))
                        || shared_globs.iter().any(|sg| entries_conflict(f, sg))
                }) {
                    forced_serial.insert(t.id.clone());
                    sched
                        .warnings
                        .push(format!("task '{}' touches a shared path -> serial", t.id));
                }
            }
        }
    }

    gated.sort();
    sched.gated = gated.clone();
    experiment.sort();
    sched.experiment = experiment.clone();
    let excluded: HashSet<String> = sched
        .gated
        .iter()
        .chain(sched.experiment.iter())
        .cloned()
        .collect();

    // File-overlap is authoritative over the LLM-declared `class`. Two tasks
    // that touch a conflicting file must not both ride the parallel-worktree
    // track: greedy coloring already keeps them out of the same *batch*, but in
    // per-task-worktree mode each parallel task gets its own branch cut from the
    // same base commit and Phase-7 merges every branch — so two conflicting
    // branches 3-way merge-conflict at the shared file (the merge is refused,
    // stalling the run and forcing manual re-work). Demote every parallel-
    // eligible task that `files_conflict` with another parallel-eligible task to
    // serial, so the deterministic overlap fact — not the advisory label —
    // decides parallel-vs-serial. Genuinely-disjoint tasks are untouched and
    // still batch in parallel. Order-independent (a task is demoted iff it
    // overlaps at least one peer), so the result set is deterministic; warnings
    // are emitted in sorted id order for stable output.
    let parallel_candidates: Vec<&Task> = dec
        .tasks
        .iter()
        .filter(|t| !excluded.contains(t.id.as_str()) && !forced_serial.contains(t.id.as_str()))
        .collect();
    let mut overlap_demoted: Vec<String> = parallel_candidates
        .iter()
        .filter(|t| {
            parallel_candidates
                .iter()
                .any(|o| o.id != t.id && files_conflict(&t.touched_files, &o.touched_files))
        })
        .map(|t| t.id.clone())
        .collect();
    overlap_demoted.sort();
    for id in overlap_demoted {
        if forced_serial.insert(id.clone()) {
            sched.warnings.push(format!(
                "task '{id}' file-overlaps another parallel task -> serial \
                 (overlap is authoritative over class)"
            ));
        }
    }

    // Force-serialize any (non-gated/non-experiment) task whose touched_files
    // violate the repo-relative convention (absolute path, `..` traversal, or
    // drive-letter prefix). Such an entry is not reliably string-comparable
    // against equivalent relative spellings, so overlap-demotion may miss a
    // same-file collision with a peer — the warning above (kept) surfaces it,
    // and here we act on it: route the offending task off the parallel-
    // worktree track ("unknown/unnormalizable path -> serialize", conservative).
    // Runs AFTER overlap-demotion on purpose: a convention-violating task is
    // still a parallel_candidate while `entries_conflict`'s suffix match demotes
    // it AND its clean same-file peer together (closing the abs-vs-relative
    // blind spot for BOTH). This pass then also covers a lone violator that has
    // no matching peer. Composes with the empty-touched_files serialization
    // (a task may hit either or both; both route to serial). Sorted for
    // deterministic ordering.
    let mut convention_serial: Vec<String> = dec
        .tasks
        .iter()
        .filter(|t| {
            !excluded.contains(t.id.as_str())
                && t.touched_files
                    .iter()
                    .any(|f| violates_repo_relative_convention(f))
        })
        .map(|t| t.id.clone())
        .collect();
    convention_serial.sort();
    for id in convention_serial {
        forced_serial.insert(id);
    }

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
    let cap = harness_core::parallel::clamp(max_parallel);
    for d in depths {
        for ids in color_by_conflict(&by_depth[&d]) {
            // Cut, do not drop: every chunk is pushed as its own batch, so the
            // colour class is fully scheduled — just across more waves.
            for chunk in ids.chunks(cap) {
                sched.batches.push(Batch {
                    parallel: chunk.to_vec(),
                });
            }
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
    /// The cap tests schedule with: the shipped ceiling, so batch-shape
    /// assertions describe what a real run produces.
    const TEST_CAP: usize = harness_core::parallel::SESSION_MAX_PARALLEL;
    use super::*;
    use crate::model::{Class, Decomposition, Task};

    // ── fail-closed: an undetermined risk classification force-gates ──────────

    #[test]
    fn undetermined_classification_force_gates_with_a_naming_warning() {
        use blastguard::diffrisk::SensitiveConfig;
        use harness_core::verdict::Determination;

        // Real end-to-end signal: a misconfigured glob list makes
        // `classify_change` undetermined ...
        let bad = SensitiveConfig::from_globs(vec!["[".to_string()]);
        let d = classify_change(
            "write a unit test",
            &["src/parser.rs".to_string()],
            "",
            &bad,
        );
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "precondition: a bad glob list must classify as Undetermined"
        );

        // ... and the scheduler's resolution force-gates it, naming the cause.
        let (force_gate, why) = resolve_force_gate(d);
        assert!(
            force_gate,
            "an unmeasurable risk must force-gate, not fall through as Low"
        );
        let why = why.expect("the undetermined arm must carry a reason");
        assert!(
            why.contains("invalid sensitive glob"),
            "the warning must name the misconfiguration, got {why:?}"
        );
    }

    #[test]
    fn determinable_classification_keeps_its_previous_gate_decision() {
        use blastguard::diffrisk::SensitiveConfig;

        let cfg = SensitiveConfig::default();
        // Low + reversible + no sensitive path -> NOT gated (unchanged), and no
        // undetermined reason is fabricated.
        let (gate, why) = resolve_force_gate(classify_change(
            "write a unit test",
            &["src/parser.rs".to_string()],
            "",
            &cfg,
        ));
        assert!(!gate);
        assert!(why.is_none());

        // Sensitive path -> Medium -> gated (unchanged), still via the measured
        // arm (no undetermined reason).
        let (gate, why) = resolve_force_gate(classify_change(
            "write a unit test",
            &["src/auth/login.rs".to_string()],
            "",
            &cfg,
        ));
        assert!(gate);
        assert!(why.is_none());

        // Deploy text -> High/irreversible -> gated (unchanged).
        let (gate, why) = resolve_force_gate(classify_change(
            "git push origin main",
            &["README.md".to_string()],
            "",
            &cfg,
        ));
        assert!(gate);
        assert!(why.is_none());
    }

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
            checks: Vec::new(),
            expected_trajectory: None,
            is_behavioral: None,
            mechanical_check: None,
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
        let s = schedule(&d, &[], TEST_CAP);
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["a", "b"]);
        assert!(s.serial.is_empty());
    }

    #[test]
    fn shared_file_demotes_both_off_the_parallel_track() {
        // Both touch src/a.rs. Coloring alone would split them into two batches
        // yet leave both on the parallel-worktree track (two branches from one
        // base -> Phase-7 merge conflict). Overlap is authoritative: both are
        // demoted to serial and no parallel batch is produced.
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(s.batches.is_empty());
        assert_eq!(s.serial, vec!["a", "b"]);
    }

    #[test]
    fn empty_touched_files_parallel_task_is_serialized() {
        // A Class::Parallel task that declares NO touched_files has an unknown
        // blast radius. `files_conflict` is an any()==false over an empty set,
        // so overlap-demotion can never fire for it and it would ride a
        // parallel worktree, colliding invisibly with peers. "When in doubt,
        // serialize" (conservative) -> it must land on the serial track and
        // never appear in any parallel batch. Tasks that DO declare files keep
        // their existing behavior.
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("blind", &[], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        let batched: Vec<&String> = s.batches.iter().flat_map(|b| b.parallel.iter()).collect();
        assert!(
            !batched.iter().any(|id| *id == "blind"),
            "empty-touched_files parallel task must NOT ride a parallel batch; batched={batched:?}",
        );
        assert!(
            s.serial.contains(&"blind".to_string()),
            "empty-touched_files task must be on the serial track; serial={:?}",
            s.serial,
        );
        // The file-declaring parallel sibling is unaffected (no over-serializing).
        assert!(
            batched.iter().any(|id| *id == "a"),
            "file-declaring parallel task must still batch; batched={batched:?}",
        );
    }

    #[test]
    fn abs_vs_relative_same_file_forces_both_serial() {
        // Two Class::Parallel tasks touch ONE file spelled two ways: "a" names
        // it with an absolute path (violates the repo-relative convention),
        // "b" with the plain repo-relative spelling. `entries_conflict` used to
        // compare raw strings and MISS the abs-vs-relative match, so both rode
        // parallel worktrees cut from the same base and 3-way merge-conflicted
        // at the shared file. Now a convention violation is force-serialized
        // AND its suffix match is detected, so BOTH land on the serial track
        // and neither appears in any parallel batch.
        let d = dec(vec![
            task("a", &["/repo/src/a.rs"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        let batched: Vec<&String> = s.batches.iter().flat_map(|b| b.parallel.iter()).collect();
        assert!(
            !batched.iter().any(|id| *id == "a" || *id == "b"),
            "abs-vs-relative same-file tasks must NOT ride a parallel batch; batched={batched:?}",
        );
        assert_eq!(
            s.serial,
            vec!["a", "b"],
            "both tasks must land on the serial track; serial={:?}",
            s.serial,
        );
        // The convention violation is still surfaced as a warning (warn AND
        // serialize), not silently swallowed.
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("'a'") && w.contains("repo-relative")),
            "convention-violation warning must still be emitted; warnings={:?}",
            s.warnings,
        );
    }

    #[test]
    fn lone_convention_violating_parallel_task_is_serialized() {
        // A single Class::Parallel task with an absolute-path entry has no peer
        // to suffix-match, but an unnormalizable path is still an unknown
        // collision surface — it must be force-serialized (not left riding a
        // lone parallel batch), while an ordinary relative sibling batches.
        let d = dec(vec![
            task("clean", &["src/clean.rs"], &[], Class::Parallel),
            task("bad", &["/etc/passwd"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(
            s.serial.contains(&"bad".to_string()),
            "convention-violating task must be serial; serial={:?}",
            s.serial,
        );
        let batched: Vec<&String> = s.batches.iter().flat_map(|b| b.parallel.iter()).collect();
        assert!(
            !batched.iter().any(|id| *id == "bad"),
            "convention-violating task must NOT batch; batched={batched:?}",
        );
        // The clean relative sibling is unaffected (no over-serializing).
        assert!(
            batched.iter().any(|id| *id == "clean"),
            "disjoint relative task must still batch; batched={batched:?}",
        );
    }

    #[test]
    fn overlapping_parallel_tasks_demoted_to_serial() {
        // Two Class::Parallel tasks touch the SAME file. Greedy coloring alone
        // keeps them out of one batch but leaves BOTH on the parallel-worktree
        // track — each gets its own branch cut from the same base, and Phase-7
        // merges every branch, so the second branch 3-way merge-conflicts at the
        // shared file. Overlap is authoritative over the advisory `class` label:
        // at least one (here both) is demoted to serial and a warning is emitted.
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        // Neither conflicting task may sit on the parallel batches track.
        let batched: Vec<&String> = s.batches.iter().flat_map(|b| b.parallel.iter()).collect();
        assert!(
            !batched.iter().any(|id| *id == "a" || *id == "b"),
            "file-overlapping parallel tasks must NOT both batch; batched={batched:?}",
        );
        assert_eq!(s.serial, vec!["a", "b"]);
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("'a'") && w.contains("overlap")),
            "a demotion must be announced; warnings={:?}",
            s.warnings,
        );
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("'b'") && w.contains("overlap")),
            "b demotion must be announced; warnings={:?}",
            s.warnings,
        );
    }

    #[test]
    fn overlap_demotion_spares_disjoint_parallel_tasks() {
        // a & b overlap (both src/shared.rs) -> both serial. c is genuinely
        // disjoint -> STILL batches in parallel. Overlap is authoritative
        // WITHOUT over-serializing independent work (no regression).
        let d = dec(vec![
            task("a", &["src/shared.rs"], &[], Class::Parallel),
            task("b", &["src/shared.rs"], &[], Class::Parallel),
            task("c", &["src/c.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert_eq!(s.serial, vec!["a", "b"]);
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["c"]);
    }

    #[test]
    fn deps_create_ordered_layers() {
        let d = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("b", &["src/b.rs"], &["a"], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
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
        let s = schedule(&d, &[], TEST_CAP);

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
    fn sensitive_path_task_tagged_parallel_is_force_gated() {
        // A task whose touched_files hit a sensitive-path glob (auth/**) must be
        // force-gated via classify_change's sensitive-path signal, exactly like a
        // mislabelled deploy — even though nothing in its title/done-criteria
        // text looks like a deploy/push action.
        let d = dec(vec![
            task("safe", &["src/a.rs"], &[], Class::Parallel),
            task("touch-auth", &["src/auth/login.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);

        assert!(
            s.gated.contains(&"touch-auth".to_string()),
            "a task touching a sensitive path must be force-gated even when tagged parallel; gated={:?}",
            s.gated,
        );
        let batched: Vec<&String> = s.batches.iter().flat_map(|b| b.parallel.iter()).collect();
        assert!(
            !batched.iter().any(|id| *id == "touch-auth"),
            "force-gated sensitive-path task must NOT appear in a parallel batch; batched={batched:?}",
        );
        assert!(
            s.warnings.iter().any(|w| w.contains("touch-auth")),
            "force-gating must be announced in warnings; warnings={:?}",
            s.warnings,
        );
        // The genuinely non-sensitive parallel task is unaffected — additive,
        // not over-gating unrelated tasks.
        assert!(
            !s.gated.contains(&"safe".to_string()),
            "non-sensitive task must NOT be gated; gated={:?}",
            s.gated,
        );
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
        let s = schedule(&d, &[], TEST_CAP);
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
        let s = schedule(&d, &["**/models.py".into()], TEST_CAP);
        assert_eq!(s.serial, vec!["a"]);
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["b"]);
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn disjoint_globs_under_shared_parent_dir_do_not_conflict() {
        // "src/foo/*.rs" and "src/bar/*.rs" share the literal parent "src/" but
        // are genuinely disjoint (globset's `*` does not cross `/`). Must NOT
        // false-positive into serial just because both live under src/.
        let d = dec(vec![
            task("a", &["src/foo/*.rs"], &[], Class::Parallel),
            task("b", &["src/bar/*.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert_eq!(s.batches.len(), 1);
        assert_eq!(s.batches[0].parallel, vec!["a", "b"]);
        assert!(s.serial.is_empty());
    }

    #[test]
    fn nested_glob_prefix_is_conservatively_demoted_even_when_disjoint() {
        // "src/*.rs" (prefix "src/") vs "src/sub/*.rs" (prefix "src/sub/"): the
        // prefixes nest ("src/sub/".starts_with("src/")) so entries_conflict
        // reports a conflict, even though globset's `*` wouldn't actually let
        // "src/*.rs" match anything under src/sub/ — a known false positive.
        // Documented as intentional/safe per this module's own doc comment
        // ("a false conflict only serializes work (safe)"): this test pins that
        // choice so a future change to the heuristic doesn't silently flip it.
        let d = dec(vec![
            task("a", &["src/*.rs"], &[], Class::Parallel),
            task("b", &["src/sub/*.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(s.batches.is_empty());
        assert_eq!(s.serial, vec!["a", "b"]);
    }

    #[test]
    fn dot_slash_prefixed_path_recognized_as_same_file() {
        // "./src/a.rs" and "src/a.rs" name the identical file. Without
        // normalization, entries_conflict compares raw strings and misses the
        // overlap (a genuine false negative: two workers could race the same
        // file if touched_files ever mixes "./"-prefixed and bare spellings).
        let d = dec(vec![
            task("a", &["./src/a.rs"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(s.batches.is_empty(), "batches={:?}", s.batches);
        assert_eq!(s.serial, vec!["a", "b"]);
    }

    #[test]
    fn repeated_slash_path_recognized_as_same_file() {
        // "src//a.rs" (double slash) and "src/a.rs" name the identical file.
        let d = dec(vec![
            task("a", &["src//a.rs"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(s.batches.is_empty(), "batches={:?}", s.batches);
        assert_eq!(s.serial, vec!["a", "b"]);
    }

    #[test]
    fn glob_touched_files_detected_as_conflict() {
        // "src/*" overlaps "src/a.rs" -> the two Parallel tasks conflict, so
        // overlap-authoritative demotion puts BOTH on the serial track (not
        // merely into separate parallel batches).
        let d = dec(vec![
            task("a", &["src/*"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(s.batches.is_empty());
        assert_eq!(s.serial, vec!["a", "b"]);
    }

    #[test]
    fn leading_wildcard_glob_pair_conservatively_serialized() {
        // AXIS 1 (twin of the nested-prefix fix): "*/foo.rs" has an EMPTY
        // literal prefix, so the empty-prefix guard used to short-circuit
        // entries_conflict to "no conflict". It GENUINELY overlaps "src/*.rs"
        // at "src/foo.rs", yet the glob-vs-literal probe misses the pair —
        // "*/foo.rs" as a glob does not match the literal string "src/*.rs"
        // (it needs a "/foo.rs" tail) and vice-versa — so two workers used to
        // ride parallel worktrees onto src/foo.rs. glob-vs-glob overlap cannot
        // be cheaply proven disjoint, so a leading-wildcard glob paired with
        // another glob is conservatively a conflict -> both serial.
        assert!(
            entries_conflict("*/foo.rs", "src/*.rs"),
            "leading-wildcard globs that genuinely overlap (at src/foo.rs) must conflict",
        );
        let d = dec(vec![
            task("a", &["*/foo.rs"], &[], Class::Parallel),
            task("b", &["src/*.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(s.batches.is_empty(), "batches={:?}", s.batches);
        assert_eq!(s.serial, vec!["a", "b"]);

        // MUST NOT over-serialize genuinely-disjoint globs whose two literal
        // prefixes are non-empty and do not nest (neither has a leading
        // wildcard, so the conservative branch never fires for them).
        let d2 = dec(vec![
            task("a", &["src/foo/*.rs"], &[], Class::Parallel),
            task("b", &["src/bar/*.rs"], &[], Class::Parallel),
        ]);
        let s2 = schedule(&d2, &[], TEST_CAP);
        assert_eq!(s2.batches.len(), 1);
        assert_eq!(s2.batches[0].parallel, vec!["a", "b"]);
        assert!(s2.serial.is_empty());
    }

    #[test]
    fn case_only_spelling_difference_is_same_file() {
        // AXIS 2 (twin of the ./-prefix / double-slash normalization fixes):
        // on a case-insensitive filesystem (macOS default) "Src/a.rs" and
        // "src/a.rs" name the SAME file. Without casefolding, entries_conflict
        // compared case-sensitive strings and scheduled both in parallel,
        // racing one file. normalize_entry now casefolds -> overlap detected,
        // both serialize.
        let d = dec(vec![
            task("a", &["Src/a.rs"], &[], Class::Parallel),
            task("b", &["src/a.rs"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(s.batches.is_empty(), "batches={:?}", s.batches);
        assert_eq!(s.serial, vec!["a", "b"]);

        // MUST NOT over-serialize genuinely-different files.
        let d2 = dec(vec![
            task("a", &["src/a.rs"], &[], Class::Parallel),
            task("b", &["src/b.rs"], &[], Class::Parallel),
        ]);
        let s2 = schedule(&d2, &[], TEST_CAP);
        assert_eq!(s2.batches.len(), 1);
        assert_eq!(s2.batches[0].parallel, vec!["a", "b"]);
        assert!(s2.serial.is_empty());
    }

    #[test]
    fn shared_glob_demotes_glob_valued_touched_file() {
        // AXIS 3 (twin of the one-directional shared-glob check): the shared
        // path is the literal "src/models.rs"; task "a"'s touched_files entry
        // is itself a glob "src/*.rs" that OVERLAPS it. The old
        // `gs.is_match(f)` treated the glob-valued entry as an opaque literal
        // and missed the overlap, letting "a" ride a parallel worktree onto a
        // shared file. Bidirectional entries_conflict now catches it -> "a"
        // demotes to serial. A disjoint peer is untouched.
        let d = dec(vec![
            task("a", &["src/*.rs"], &[], Class::Parallel),
            task("b", &["docs/x.md"], &[], Class::Parallel),
        ]);
        let s = schedule(&d, &["src/models.rs".into()], TEST_CAP);
        assert!(
            s.serial.contains(&"a".to_string()),
            "glob-valued touched_files overlapping a shared glob must serialize; serial={:?}",
            s.serial,
        );
        let batched: Vec<&String> = s.batches.iter().flat_map(|b| b.parallel.iter()).collect();
        assert!(
            !batched.iter().any(|id| *id == "a"),
            "shared-glob-overlapping glob-valued task must NOT batch; batched={batched:?}",
        );
        assert!(
            batched.iter().any(|id| *id == "b"),
            "disjoint peer must still batch; batched={batched:?}",
        );
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("'a'") && w.contains("shared")),
            "demotion must be announced; warnings={:?}",
            s.warnings,
        );
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
        let s = schedule(&d, &[], TEST_CAP);
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

    #[test]
    fn absolute_unix_path_in_touched_files_warns() {
        // /etc/passwd is not a repo-relative touched_files entry; it silently
        // defeats string-based conflict comparison against equivalent relative
        // spellings. schedule() must surface this, not stay silent.
        let d = dec(vec![task("a", &["/etc/passwd"], &[], Class::Parallel)]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains('a') && w.contains("repo-relative")),
            "expected a repo-relative-convention warning; warnings={:?}",
            s.warnings,
        );
    }

    #[test]
    fn windows_drive_letter_path_in_touched_files_warns() {
        let d = dec(vec![task(
            "a",
            &["C:\\Users\\evil\\file.rs"],
            &[],
            Class::Parallel,
        )]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(
            s.warnings.iter().any(|w| w.contains("repo-relative")),
            "expected a repo-relative-convention warning for a drive-letter path; warnings={:?}",
            s.warnings,
        );
    }

    #[test]
    fn dot_dot_component_in_touched_files_warns() {
        let d = dec(vec![task("a", &["../../etc/passwd"], &[], Class::Parallel)]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(
            s.warnings.iter().any(|w| w.contains("repo-relative")),
            "expected a repo-relative-convention warning for a '..' path; warnings={:?}",
            s.warnings,
        );
    }

    #[test]
    fn ordinary_relative_path_does_not_warn() {
        let d = dec(vec![task("a", &["src/a.rs"], &[], Class::Parallel)]);
        let s = schedule(&d, &[], TEST_CAP);
        assert!(
            !s.warnings.iter().any(|w| w.contains("repo-relative")),
            "a plain relative path must not trigger the sanity-check warning; warnings={:?}",
            s.warnings,
        );
    }
}

#[cfg(test)]
mod prop_tests {
    /// The cap tests schedule with: the shipped ceiling, so batch-shape
    /// assertions describe what a real run produces.
    const TEST_CAP: usize = harness_core::parallel::SESSION_MAX_PARALLEL;
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
            let sched = schedule(&pd(tasks), &[], TEST_CAP);
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
            let sched = schedule(&pd(tasks.clone()), &[], TEST_CAP);
            for t in &tasks {
                let in_batch = sched.batches.iter().any(|b| b.parallel.contains(&t.id));
                prop_assert!(in_batch, "task {} should be in a batch", t.id);
            }
        }

        /// Gated tasks always end up in sched.gated and nowhere else.
        #[test]
        fn gated_tasks_always_in_gated(n in 1usize..6) {
            let tasks: Vec<Task> = (0..n).map(|i| pt(&format!("t{i}"), vec![], vec![], Class::Gated)).collect();
            let sched = schedule(&pd(tasks.clone()), &[], TEST_CAP);
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
            let sched = schedule(&pd(tasks.clone()), &[], TEST_CAP);
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
            let sched = schedule(&dec, &[], TEST_CAP);
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
            let sched = schedule(&pd(tasks), &[], TEST_CAP);
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
            let sched = schedule(&pd(tasks), &[], TEST_CAP);
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
            let sched = schedule(&pd(tasks.clone()), &["src/shared.rs".to_string()], TEST_CAP);
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
            let sched = schedule(&pd(tasks), &[], TEST_CAP);
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
            let sched = schedule(&pd(tasks), &[], TEST_CAP);
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
            let sched = schedule(&pd(tasks), &["shared.rs".to_string()], TEST_CAP);
            prop_assert!(!sched.warnings.is_empty(), "expected demotion warnings");
        }

        /// Single parallel task (with a declared file) → exactly one batch of
        /// size one. It must declare a file: an empty touched_files set is now
        /// an unknown blast radius that is conservatively serialized.
        ///
        /// Regression (id = "scp"): the `pt` helper sets `title = id`, and the
        /// deterministic force-gate classifies `task_action_text` (title +
        /// done_criteria + touched_files — NOT the id). A fuzzed id that
        /// happens to spell a deploy/egress token — "scp" folds into
        /// `task_action_text` as "scp src/only.rs", matching the `"scp "`
        /// DEPLOY_SIGNAL — force-gated the task, yielding 0 batches (the task
        /// landed in `gated`, not "dropped"). That is the gate working as
        /// designed on a title; the conflation of the fuzzed *id* with the
        /// risk-classified *title* is the test artifact (in a real
        /// `Decomposition` id and title are distinct fields). This property is
        /// about file-conflict batching, which is independent of the id, so we
        /// pin a fixed benign title and keep fuzzing the id: batching must be
        /// invariant under the id regardless of whether the id spells a token.
        #[test]
        fn single_parallel_task_one_batch(id in "[a-z]{2,5}") {
            let mut t = pt(&id, vec!["src/only.rs".into()], vec![], Class::Parallel);
            t.title = "implement the feature".into();
            let sched = schedule(&pd(vec![t]), &[], TEST_CAP);
            prop_assert_eq!(sched.batches.len(), 1);
            prop_assert_eq!(sched.batches[0].parallel.len(), 1);
            prop_assert!(sched.serial.is_empty());
            prop_assert!(sched.gated.is_empty());
        }
    }

    /// Empty decomposition → empty schedule (deterministic sanity check).
    #[test]
    fn empty_decomp_empty_schedule() {
        let sched = schedule(&pd(vec![]), &[], TEST_CAP);
        assert!(sched.batches.is_empty());
        assert!(sched.serial.is_empty());
        assert!(sched.gated.is_empty());
    }
}

#[cfg(test)]
mod session_cap_tests {
    /// The cap tests schedule with: the shipped ceiling, so batch-shape
    /// assertions describe what a real run produces.
    const TEST_CAP: usize = harness_core::parallel::SESSION_MAX_PARALLEL;
    use super::*;
    use crate::model::{Class, Decomposition, Task};
    use harness_core::parallel::SESSION_MAX_PARALLEL;

    fn ptask(id: &str, file: &str) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            touched_files: vec![file.to_string()],
            deps: Vec::new(),
            class: Class::Parallel,
            suggested_model: None,
            done_criteria: None,
            size: None,
            target_symbols: Vec::new(),
            reproduction_tests: None,
            confidence: None,
            kind: None,
            checks: Vec::new(),
            expected_trajectory: None,
            is_behavioral: None,
            mechanical_check: None,
        }
    }

    fn disjoint(n: usize) -> Decomposition {
        Decomposition {
            goal: "g".into(),
            tasks: (0..n)
                .map(|i| ptask(&format!("t{i}"), &format!("src/f{i}.rs")))
                .collect(),
        }
    }

    #[test]
    fn parallel_batch_never_exceeds_the_session_cap() {
        // A batch IS the fan-out width: the skill spawns every id in it in one
        // message. An unbounded batch is an unbounded session, which is what
        // `max_parallel` only ever *claimed* to prevent ("advisory; the skill
        // honors it" — i.e. nothing enforced it).
        let n = SESSION_MAX_PARALLEL + 2;
        let s = schedule(&disjoint(n), &[], TEST_CAP);
        for b in &s.batches {
            assert!(
                b.parallel.len() <= SESSION_MAX_PARALLEL,
                "batch of {} exceeds the session cap {SESSION_MAX_PARALLEL}: {:?}",
                b.parallel.len(),
                b.parallel
            );
        }
    }

    #[test]
    fn capping_a_batch_drops_no_task() {
        // Splitting must be a partition, not a truncation: capping the width is
        // not a licence to silently stop scheduling work.
        let n = SESSION_MAX_PARALLEL + 2;
        let s = schedule(&disjoint(n), &[], TEST_CAP);
        let mut got: Vec<&String> = s
            .batches
            .iter()
            .flat_map(|b| b.parallel.iter())
            .chain(s.serial.iter())
            .collect();
        got.sort();
        got.dedup();
        assert_eq!(got.len(), n, "every task must be scheduled exactly once");
    }
}
