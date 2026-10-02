//! Precedent store + structural-fingerprint matching — the Google LSC
//! "reviewed-once-applied-broadly" mechanism for `condukt review-brief`.
//!
//! A ratified [`Precedent`] records the DECLARED shape (touched files +
//! target symbols) of a change a human already reviewed and approved as
//! routine. [`match_precedent`] compares a candidate task's declared shape
//! against the store and, when it clears the bar, lets
//! [`crate::review_brief::build_review_brief`] downgrade the review tier to
//! `Low` (spot-check only) instead of spending human attention on a change
//! whose SHAPE was already reviewed.
//!
//! # Honest-scope note
//!
//! `review-brief` has no live diff to work from — only the task's DECLARED
//! `touched_files` / `target_symbols` (decomposition-level). The fingerprint
//! and match therefore key on those declared sets, not a recomputed diff.
//! This is an intentional, honest limitation, not an oversight: a precedent
//! match says "this task's declared shape matches one already reviewed", not
//! "the actual diff is provably identical".
//!
//! # Persistence
//!
//! Mirrors `escalate.rs`: a single project-scoped JSON file at
//! `<state_dir>/<project-key>/precedents.json`, atomic writes (temp +
//! rename), fail-soft reads (missing/corrupt file => empty registry, never an
//! error/panic).
//!
//! # Determinism
//!
//! [`structural_fingerprint`] normalizes (trim, dedup, sort) each input set
//! before hashing via [`harness_core::hash::Fnv1a64`], so the SAME sets in
//! any order produce the SAME fingerprint. [`jaccard`] and [`match_precedent`]
//! are pure (no I/O, no wall-clock) — timestamps are injected by the CLI
//! layer, never read inside these functions.

use crate::config::Config;
use crate::store::{project_key, repo_root};
use anyhow::Result;
use harness_core::hash::Fnv1a64;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One ratified precedent: the declared shape of a change a human already
/// reviewed and approved as routine, plus a free-text note explaining what it
/// covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Precedent {
    /// Deterministic structural fingerprint of `files` + `symbols`
    /// ([`structural_fingerprint`]).
    pub fingerprint: u64,
    /// The declared touched-files shape at ratification time.
    pub files: Vec<String>,
    /// The declared target-symbols shape at ratification time.
    pub symbols: Vec<String>,
    /// When this precedent was ratified (unix seconds) — observability only;
    /// never feeds the fingerprint or the match decision.
    pub ratified_ts: i64,
    /// Free-text note describing what this precedent covers (e.g. "bumping a
    /// dependency pin in Cargo.toml").
    pub note: String,
}

/// The on-disk registry: an ordered list of ratified precedents
/// (append-on-ratify). A bare list keeps the JSON trivially forward/backward
/// compatible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub precedents: Vec<Precedent>,
}

/// A candidate's best match against the precedent store: which precedent
/// (by fingerprint), how similar (1.0 for an exact-fingerprint match), and
/// its note (carried through so the review brief can explain the downgrade).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrecedentMatch {
    pub fingerprint: u64,
    pub similarity: f64,
    pub note: String,
}

/// Normalize a raw set of strings for fingerprinting/comparison: trim
/// whitespace, drop empties, dedup, and sort — so `["b", "a", "b"]` and
/// `["a", " b "]` normalize identically (order-independent, whitespace-safe).
fn normalize_set(items: &[String]) -> BTreeSet<String> {
    items
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Deterministic structural fingerprint of a declared `files` + `symbols`
/// shape. Normalizes each set (trim, dedup, SORT) before hashing, so the SAME
/// sets in ANY input order produce the SAME fingerprint via
/// [`harness_core::hash::Fnv1a64`] — the workspace's canonical hash.
pub fn structural_fingerprint(files: &[String], symbols: &[String]) -> u64 {
    let files = normalize_set(files);
    let symbols = normalize_set(symbols);

    let mut h = Fnv1a64::new();
    // Unit separator between fields/elements keeps the byte stream
    // unambiguous (no delimiter collision with legitimate path/symbol
    // characters like ':' or ',').
    for f in &files {
        h.update(f.as_bytes());
        h.update(b"\x1f");
    }
    h.update(b"\x1e");
    for s in &symbols {
        h.update(s.as_bytes());
        h.update(b"\x1f");
    }
    h.finish()
}

/// Jaccard similarity of two sets: `|intersection| / |union|`. Convention for
/// the degenerate case: two EMPTY sets are defined as fully similar (`1.0`)
/// rather than undefined/0 — an empty declared shape trivially "matches"
/// another empty declared shape (there is nothing to disagree on). Mirrors
/// the idea in `specguard::ratify`'s corpus similarity without depending on
/// specguard.
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        1.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Find the BEST matching precedent for a candidate's declared shape.
///
/// A candidate matches a precedent iff EITHER:
/// - its fingerprint is IDENTICAL to the precedent's (similarity forced to
///   `1.0`), OR
/// - `jaccard(files) >= tolerance AND jaccard(symbols) >= tolerance` (the
///   combined similarity is the MINIMUM of the two, i.e. the weaker axis
///   gates the match).
///
/// Among all precedents clearing the bar, returns the one with the highest
/// similarity; ties are broken deterministically by the SMALLEST fingerprint
/// (a stable, reproducible tie-break — no reliance on store iteration order).
/// Returns `None` if no precedent clears the bar (an empty store trivially
/// returns `None`). Pure: no I/O, no wall-clock.
pub fn match_precedent(
    cand_files: &[String],
    cand_symbols: &[String],
    precedents: &[Precedent],
    tolerance: f64,
) -> Option<PrecedentMatch> {
    let cand_fp = structural_fingerprint(cand_files, cand_symbols);
    let cand_files_set = normalize_set(cand_files);
    let cand_symbols_set = normalize_set(cand_symbols);

    let mut best: Option<(f64, &Precedent)> = None;

    for p in precedents {
        let similarity = if p.fingerprint == cand_fp {
            1.0
        } else {
            let p_files = normalize_set(&p.files);
            let p_symbols = normalize_set(&p.symbols);
            let sim_files = jaccard(&cand_files_set, &p_files);
            let sim_symbols = jaccard(&cand_symbols_set, &p_symbols);
            if sim_files >= tolerance && sim_symbols >= tolerance {
                sim_files.min(sim_symbols)
            } else {
                continue;
            }
        };

        best = match best {
            None => Some((similarity, p)),
            Some((best_sim, best_p)) => {
                if similarity > best_sim
                    || (similarity == best_sim && p.fingerprint < best_p.fingerprint)
                {
                    Some((similarity, p))
                } else {
                    Some((best_sim, best_p))
                }
            }
        };
    }

    best.map(|(similarity, p)| PrecedentMatch {
        fingerprint: p.fingerprint,
        similarity,
        note: p.note.clone(),
    })
}

/// `<state_dir>/<project-key>/precedents.json` — beside `escalations.json` /
/// `claims.json`, so it is per-project and unrelated projects never share a
/// precedent store.
fn precedents_path(cfg: &Config, cwd: &Path) -> PathBuf {
    cfg.state_dir
        .join(project_key(&repo_root(cwd)))
        .join("precedents.json")
}

/// Fail-soft load: a missing or corrupt registry is treated as empty rather
/// than breaking the caller (mirrors `escalate.rs::load`).
fn load(path: &Path) -> Registry {
    match std::fs::read_to_string(path) {
        Ok(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        Err(_) => Registry::default(),
    }
}

/// Atomic write (temp + rename), mirroring `escalate.rs::save`.
fn save(path: &Path, reg: &Registry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(reg)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Ratify (record) a new precedent from a declared `files`/`symbols` shape
/// and persist it. Returns the stored record (with its derived fingerprint).
/// Atomic write; fail-soft load. `now` is the ratification timestamp (unix
/// seconds) injected by the CLI layer — never read from wall-clock here.
pub fn record_precedent(
    cfg: &Config,
    cwd: &Path,
    files: &[String],
    symbols: &[String],
    note: &str,
    now: i64,
) -> Result<Precedent> {
    let path = precedents_path(cfg, cwd);
    let mut reg = load(&path);

    let rec = Precedent {
        fingerprint: structural_fingerprint(files, symbols),
        files: files.to_vec(),
        symbols: symbols.to_vec(),
        ratified_ts: now,
        note: note.to_string(),
    };
    reg.precedents.push(rec.clone());
    save(&path, &reg)?;
    Ok(rec)
}

/// Load ALL ratified precedents for this project, in ratification order.
/// Fail-soft: a missing or corrupt store returns an empty vec, never an
/// error.
pub fn load_precedents(cfg: &Config, cwd: &Path) -> Vec<Precedent> {
    let path = precedents_path(cfg, cwd);
    load(&path).precedents
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Unique fixture dir via atomic `mkdtemp` (backlog a6608aa0): never
    /// re-enters a leftover dir from a recycled pid, and the returned guard
    /// removes the dir on drop so fixtures no longer pile up in temp.
    fn make_tmp_dir(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("condukt-precedent-{tag}-"))
            .tempdir()
            .expect("tempdir")
    }

    /// backlog a6608aa0: a fixture dir must never be a leftover from an earlier
    /// run. The old `<kind>-<tag>-<pid>-<seq>` name re-entered such a dir on pid
    /// reuse and the stale file inside made `all.len() == 1` flaky. Seed every
    /// name the old scheme could hand out next, then require an EMPTY dir.
    #[test]
    fn make_tmp_dir_never_reenters_a_leftover_dir() {
        let upto = SEQ.load(Ordering::Relaxed) + 256;
        let seeded: Vec<PathBuf> = (0..upto)
            .map(|n| {
                let d = std::env::temp_dir().join(format!(
                    "condukt-precedent-collide-{}-{n}",
                    std::process::id()
                ));
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join("precedents.json"), "stale-from-a-dead-run").unwrap();
                d
            })
            .collect();
        let fresh = make_tmp_dir("collide");
        let p: &Path = fresh.as_ref();
        let leftovers: Vec<_> = std::fs::read_dir(p).unwrap().flatten().collect();
        for d in &seeded {
            let _ = std::fs::remove_dir_all(d);
        }
        assert!(
            leftovers.is_empty(),
            "fixture dir {} re-entered a leftover dir: {leftovers:?}",
            p.display()
        );
    }

    fn make_cfg(tmp: &Path) -> Config {
        Config {
            worktree_base: tmp.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: tmp.to_path_buf(),
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
            autonomy_source: harness_core::autonomy::Source::BuiltinDefault,
            consensus_enabled: false,
            consensus_samples: crate::consensus::DEFAULT_SAMPLES,
            consensus_threshold: crate::consensus::DEFAULT_THRESHOLD,
            adversarial_enabled: false,
            adversarial_size: crate::adversarial::DEFAULT_PANEL,
            adversarial_min_voters: crate::adversarial::DEFAULT_MIN_VOTERS,
            adversarial_block_ratio: crate::adversarial::DEFAULT_BLOCK_RATIO,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        }
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // --- structural_fingerprint ---

    #[test]
    fn fingerprint_is_order_independent() {
        let a = structural_fingerprint(&v(&["b.rs", "a.rs"]), &v(&["foo", "bar"]));
        let b = structural_fingerprint(&v(&["a.rs", "b.rs"]), &v(&["bar", "foo"]));
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_normalizes_whitespace_and_dedup() {
        let a = structural_fingerprint(&v(&["a.rs", "a.rs", " b.rs "]), &v(&["foo"]));
        let b = structural_fingerprint(&v(&["a.rs", "b.rs"]), &v(&["foo"]));
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_differs_for_different_shapes() {
        let a = structural_fingerprint(&v(&["a.rs"]), &v(&["foo"]));
        let b = structural_fingerprint(&v(&["a.rs"]), &v(&["bar"]));
        assert_ne!(a, b);
    }

    // --- jaccard (indirectly via match_precedent) / match_precedent ---

    #[test]
    fn match_precedent_exact_fingerprint_is_similarity_one() {
        let p = Precedent {
            fingerprint: structural_fingerprint(&v(&["a.rs", "b.rs"]), &v(&["foo"])),
            files: v(&["a.rs", "b.rs"]),
            symbols: v(&["foo"]),
            ratified_ts: 1,
            note: "n".to_string(),
        };
        // Same shape, different order -> same fingerprint -> exact match.
        let m = match_precedent(
            &v(&["b.rs", "a.rs"]),
            &v(&["foo"]),
            std::slice::from_ref(&p),
            0.8,
        )
        .unwrap();
        assert_eq!(m.fingerprint, p.fingerprint);
        assert_eq!(m.similarity, 1.0);
        assert_eq!(m.note, "n");
    }

    #[test]
    fn match_precedent_within_tolerance_matches() {
        let p = Precedent {
            fingerprint: structural_fingerprint(&v(&["a.rs", "b.rs", "c.rs"]), &v(&["foo", "bar"])),
            files: v(&["a.rs", "b.rs", "c.rs"]),
            symbols: v(&["foo", "bar"]),
            ratified_ts: 1,
            note: "n".to_string(),
        };
        // 2/3 files overlap (jaccard files = 2/4 = 0.5... let's pick a clearly
        // within-tolerance case instead): use 3/4 overlap.
        // cand files: a.rs, b.rs, c.rs, d.rs vs precedent a.rs,b.rs,c.rs ->
        // intersection 3, union 4 -> 0.75.
        let cand_files = v(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        let cand_symbols = v(&["foo", "bar"]); // identical -> jaccard 1.0
        let m = match_precedent(&cand_files, &cand_symbols, std::slice::from_ref(&p), 0.7).unwrap();
        assert_eq!(m.fingerprint, p.fingerprint);
        assert!((m.similarity - 0.75).abs() < 1e-9);
    }

    #[test]
    fn match_precedent_below_tolerance_is_none() {
        let p = Precedent {
            fingerprint: structural_fingerprint(&v(&["a.rs"]), &v(&["foo"])),
            files: v(&["a.rs"]),
            symbols: v(&["foo"]),
            ratified_ts: 1,
            note: "n".to_string(),
        };
        // Completely disjoint shape -> jaccard 0 on both axes, no fingerprint
        // match -> None.
        let m = match_precedent(&v(&["z.rs"]), &v(&["qux"]), &[p], 0.5);
        assert!(m.is_none());
    }

    #[test]
    fn match_precedent_empty_store_is_none() {
        let m = match_precedent(&v(&["a.rs"]), &v(&["foo"]), &[], 0.8);
        assert!(m.is_none());
    }

    #[test]
    fn match_precedent_empty_candidate_and_empty_precedent_matches_by_jaccard_convention() {
        // Both candidate and precedent declare NOTHING (empty files+symbols):
        // fingerprint is identical (both hash the empty-set stream), so this
        // is an exact match with similarity 1.0 either way.
        let p = Precedent {
            fingerprint: structural_fingerprint(&[], &[]),
            files: vec![],
            symbols: vec![],
            ratified_ts: 1,
            note: "empty".to_string(),
        };
        let m = match_precedent(&[], &[], &[p], 0.8).unwrap();
        assert_eq!(m.similarity, 1.0);
    }

    #[test]
    fn match_precedent_tie_break_is_deterministic_by_fingerprint() {
        // Two precedents that BOTH match with the SAME similarity (both exact
        // fingerprint matches are impossible simultaneously for one
        // candidate, so construct two DIFFERENT precedents that both clear
        // tolerance with equal jaccard similarity).
        let p_low = Precedent {
            fingerprint: 10,
            files: v(&["a.rs", "b.rs"]),
            symbols: v(&["foo"]),
            ratified_ts: 1,
            note: "low-fp".to_string(),
        };
        let p_high = Precedent {
            fingerprint: 20,
            files: v(&["a.rs", "b.rs"]),
            symbols: v(&["foo"]),
            ratified_ts: 2,
            note: "high-fp".to_string(),
        };
        let cand_files = v(&["a.rs", "b.rs"]);
        let cand_symbols = v(&["foo"]);
        // Order shouldn't matter for the tie-break outcome.
        let m1 = match_precedent(
            &cand_files,
            &cand_symbols,
            &[p_low.clone(), p_high.clone()],
            0.5,
        )
        .unwrap();
        let m2 = match_precedent(&cand_files, &cand_symbols, &[p_high, p_low], 0.5).unwrap();
        assert_eq!(m1.fingerprint, 10);
        assert_eq!(m2.fingerprint, 10);
        assert_eq!(m1.note, "low-fp");
    }

    #[test]
    fn match_precedent_is_order_independent_for_candidate_sets() {
        let p = Precedent {
            fingerprint: structural_fingerprint(&v(&["a.rs", "b.rs"]), &v(&["foo", "bar"])),
            files: v(&["a.rs", "b.rs"]),
            symbols: v(&["foo", "bar"]),
            ratified_ts: 1,
            note: "n".to_string(),
        };
        let m1 = match_precedent(
            &v(&["a.rs", "b.rs"]),
            &v(&["foo", "bar"]),
            std::slice::from_ref(&p),
            0.8,
        )
        .unwrap();
        let m2 = match_precedent(&v(&["b.rs", "a.rs"]), &v(&["bar", "foo"]), &[p], 0.8).unwrap();
        assert_eq!(m1.fingerprint, m2.fingerprint);
        assert_eq!(m1.similarity, m2.similarity);
    }

    // --- store persistence ---

    #[test]
    fn record_persists_and_is_retrievable() {
        let tmp_dir = make_tmp_dir("record");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        let rec = record_precedent(
            &cfg,
            tmp,
            &v(&["a.rs", "b.rs"]),
            &v(&["foo"]),
            "routine dep bump",
            1234,
        )
        .unwrap();
        assert_eq!(rec.ratified_ts, 1234);
        assert_eq!(rec.note, "routine dep bump");
        assert_ne!(rec.fingerprint, 0);

        let all = load_precedents(&cfg, tmp);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], rec);
        assert!(precedents_path(&cfg, tmp).exists());
    }

    #[test]
    fn missing_store_loads_empty() {
        let tmp_dir = make_tmp_dir("missing");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        assert!(load_precedents(&cfg, tmp).is_empty());
    }

    #[test]
    fn corrupt_store_is_treated_as_empty() {
        let tmp_dir = make_tmp_dir("corrupt");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        let path = precedents_path(&cfg, tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json at all {{{").unwrap();
        assert!(load_precedents(&cfg, tmp).is_empty());
        // Fail-soft: record still succeeds (registry read as empty then
        // overwritten with a fresh valid one).
        let rec = record_precedent(&cfg, tmp, &v(&["a.rs"]), &v(&["foo"]), "n", 1).unwrap();
        let all = load_precedents(&cfg, tmp);
        assert_eq!(all, vec![rec]);
    }

    #[test]
    fn record_appends_multiple() {
        let tmp_dir = make_tmp_dir("append");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        record_precedent(&cfg, tmp, &v(&["a.rs"]), &v(&["foo"]), "first", 1).unwrap();
        record_precedent(&cfg, tmp, &v(&["b.rs"]), &v(&["bar"]), "second", 2).unwrap();
        let all = load_precedents(&cfg, tmp);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].note, "first");
        assert_eq!(all[1].note, "second");
    }
}

#[cfg(test)]
mod backlog_a6608aa0 {
    //! backlog a6608aa0: the precedent/escalate test fixtures name their dir
    //! `temp_dir()/condukt-<kind>-<tag>-<pid>-<seq>`, `create_dir_all` it
    //! (silently re-entering an existing dir) and never clean up. When an OS
    //! pid is reused, a later run re-enters an earlier run's residue and its
    //! count assertions see the stale records.
    //!
    //! Pid reuse is made deterministic: a `sh` wrapper reports its own pid,
    //! waits until this test has seeded the residue a prior process with THAT
    //! pid would have left, then `exec`s the test binary — keeping the pid.
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn cfg_at(state_dir: &Path) -> Config {
        Config {
            worktree_base: state_dir.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: state_dir.to_path_buf(),
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
            autonomy_source: harness_core::autonomy::Source::BuiltinDefault,
            consensus_enabled: false,
            consensus_samples: crate::consensus::DEFAULT_SAMPLES,
            consensus_threshold: crate::consensus::DEFAULT_THRESHOLD,
            adversarial_enabled: false,
            adversarial_size: crate::adversarial::DEFAULT_PANEL,
            adversarial_min_voters: crate::adversarial::DEFAULT_MIN_VOTERS,
            adversarial_block_ratio: crate::adversarial::DEFAULT_BLOCK_RATIO,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        }
    }

    /// Run `target` (an exact test name) in a child copy of this test binary
    /// whose pid is known before it starts; `seed(pid, tmpdir)` plants the
    /// residue first. Returns (success, stdout).
    fn run_with_residue(target: &str, seed: impl FnOnce(u32, &Path)) -> (bool, String) {
        let work = tempfile::tempdir().expect("work dir");
        let tmpdir = work.path().join("T");
        std::fs::create_dir_all(&tmpdir).unwrap();
        let pidf = work.path().join("pid");
        let fifo = work.path().join("go");
        let ok = Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(ok.success(), "mkfifo");
        let exe = std::env::current_exe().expect("current test binary");
        let child = Command::new("sh")
            .arg("-c")
            .arg("echo $$ > \"$PIDF.tmp\" && mv \"$PIDF.tmp\" \"$PIDF\" && read go < \"$FIFO\" && exec \"$EXE\" \"$TARGET\" --exact --test-threads=1")
            .env("PIDF", &pidf)
            .env("FIFO", &fifo)
            .env("EXE", &exe)
            .env("TARGET", target)
            .env("TMPDIR", &tmpdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wrapper");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let pid: u32 = loop {
            if let Ok(s) = std::fs::read_to_string(&pidf) {
                if let Ok(p) = s.trim().parse() {
                    break p;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "wrapper never reported its pid"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        seed(pid, &tmpdir);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo)
            .unwrap()
            .write_all(b"go\n")
            .unwrap();
        let out = child.wait_with_output().expect("wait child");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            stdout.contains("running 1 test"),
            "fixture precondition: target test must run; stdout={stdout} stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), stdout)
    }

    #[test]
    #[ignore = "backlog a6608aa0: open defect, remove ignore when fixed"]
    fn precedent_test_does_not_reenter_a_reused_pid_residue() {
        let (ok, stdout) = run_with_residue(
            "precedent::tests::record_persists_and_is_retrievable",
            |pid, tmpdir| {
                let d = tmpdir.join(format!("condukt-precedent-record-{pid}-0"));
                std::fs::create_dir_all(&d).unwrap();
                record_precedent(&cfg_at(&d), &d, &["old.rs".to_string()], &[], "stale", 1)
                    .unwrap();
            },
        );
        assert!(
            ok,
            "record_persists_and_is_retrievable re-entered a residue left by an \
             earlier process with the same pid: {stdout}"
        );
    }

    #[test]
    #[ignore = "backlog a6608aa0: open defect, remove ignore when fixed"]
    fn escalate_test_does_not_reenter_a_reused_pid_residue() {
        let (ok, stdout) = run_with_residue(
            "escalate::tests::add_creates_new_open_record_after_resolve",
            |pid, tmpdir| {
                let d = tmpdir.join(format!("condukt-escalate-dedup-reask-{pid}-0"));
                std::fs::create_dir_all(&d).unwrap();
                crate::escalate::add_escalation(
                    &cfg_at(&d),
                    &d,
                    "runStale",
                    "tz",
                    "stale question",
                    &["x".to_string(), "y".to_string()],
                    0,
                    1,
                )
                .unwrap();
            },
        );
        assert!(
            ok,
            "add_creates_new_open_record_after_resolve re-entered a residue left by \
             an earlier process with the same pid: {stdout}"
        );
    }
}
