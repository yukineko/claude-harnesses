//! Change-triggered scope resolution.
//!
//! The audit only looks at areas touched since a baseline ref (plus invariants
//! marked `always`, which run every time; non-`always` invariants are
//! diff-scoped like areas — only in scope when the diff touches their
//! canon). This keeps each run cheap and bounded instead of re-auditing the
//! whole tree. Git interaction is isolated in [`changed_files`] so the
//! area-classification logic ([`classify`]) stays pure and unit-testable.

use crate::config::{Area, Config, Invariant};
use crate::prompt::Shard;
use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The resolved scope for one audit run.
#[derive(Debug)]
pub struct Scope {
    /// The baseline ref actually used (after fallback resolution).
    pub baseline: String,
    /// Whether the configured/recorded baseline failed and we fell back.
    pub fell_back: bool,
    /// All files changed between `baseline` and HEAD.
    pub changed_files: Vec<String>,
    /// Indices into `config.areas` that are in scope, each with the changed
    /// files that landed in it (for prompt context).
    pub in_scope: Vec<AreaHit>,
    /// Names of areas with no changed files (reported as explicitly skipped).
    pub skipped_areas: Vec<String>,
    /// Decision record files (absolute paths) for the D3 audit; empty disables it.
    pub decision_files: Vec<String>,
}

#[derive(Debug)]
pub struct AreaHit {
    pub area_index: usize,
    /// Changed files matching the area's globs (implementation changes).
    pub matched_files: Vec<String>,
    /// Changed files that are this area's canon (spec changed → check the
    /// implementation still follows). An area is in scope if EITHER list is
    /// non-empty, so a pure-canon edit re-triggers the audit.
    pub changed_canon: Vec<String>,
}

/// Decide the baseline ref: explicit override > recorded last-ref > fallback.
pub fn resolve_baseline(
    cfg: &Config,
    override_ref: Option<&str>,
    last_ref: Option<&str>,
) -> String {
    if let Some(r) = override_ref {
        if !r.trim().is_empty() {
            return r.trim().to_string();
        }
    }
    if !cfg.scope.baseline_ref.trim().is_empty() {
        return cfg.scope.baseline_ref.trim().to_string();
    }
    if let Some(r) = last_ref {
        if !r.trim().is_empty() {
            return r.trim().to_string();
        }
    }
    cfg.scope.fallback_ref.clone()
}

/// Pure guard: is `r` a git ref safe to hand to `git diff` as a positional
/// argument? A hostile baseline (from `--baseline` or `SPECGUARD_BASELINE_REF`)
/// must never reach git.
///
/// We accept only conventional ref characters (letters, digits, `_`, `-`, `/`,
/// `.`, `~`, `^`) and reject empty/whitespace-only values, a leading `-` (which
/// git would parse as an option, e.g. `--upload-pack=evil`), and any whitespace
/// or shell metacharacter (`;`, `|`, `&`, `$`, backtick, `<`, `>`, newline,
/// quotes, `*`, etc.).
///
/// Genuine range refs like `HEAD~3` or `main~1^2` and paths like `feature/x`
/// remain valid.
pub fn is_safe_ref(r: &str) -> bool {
    let r = r.trim();
    if r.is_empty() {
        return false;
    }
    if r.starts_with('-') {
        return false;
    }
    r.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/' | '.' | '~' | '^'))
}

/// Label used when the audit falls all the way back to "every tracked file"
/// (a young/shallow repo where neither baseline nor fallback ref resolves).
pub const ALL_TRACKED: &str = "(all tracked files)";

/// Resolve the set of changed files via a 3-tier fallback so a first run on a
/// young repo never hard-errors:
///   1. the requested `baseline` (`baseline..HEAD`),
///   2. the configured `fallback` ref,
///   3. all tracked files (`git ls-tree HEAD`) — "audit everything".
///
/// Uses a two-dot diff (`baseline..HEAD`), i.e. "what HEAD changed relative to
/// baseline" — NOT three-dot, which diffs from the merge-base and would miss
/// changes that came in on the baseline side. Only committed state is audited;
/// uncommitted working-tree edits are out of scope by design.
///
/// `max_whole_tree_files` guards ONLY tier 3 (the normal git-diff tiers 1/2
/// are already naturally scoped to what changed). `0` disables the guard
/// (unlimited, the historical behavior). When tier 3 is reached and the
/// tracked-file count exceeds a positive budget, this returns `Err` instead
/// of silently handing the whole tree (e.g. 200k lines) to the audit agent —
/// see [`crate::config::ScopeConfig::whole_tree_fallback_max_files`].
///
/// Returns (files, ref-actually-used, fell_back).
pub fn changed_files(
    repo_root: &Path,
    baseline: &str,
    fallback: &str,
    max_whole_tree_files: usize,
) -> Result<(Vec<String>, String, bool)> {
    if let Ok(files) = git_diff_names(repo_root, baseline) {
        return Ok((files, baseline.to_string(), false));
    }
    if fallback != baseline {
        if let Ok(files) = git_diff_names(repo_root, fallback) {
            return Ok((files, fallback.to_string(), true));
        }
    }
    let files = all_tracked_files(repo_root).with_context(|| {
        format!("could not resolve baseline '{baseline}' or fallback '{fallback}', and listing all tracked files failed")
    })?;
    if max_whole_tree_files > 0 && files.len() > max_whole_tree_files {
        anyhow::bail!(
            "refusing whole-tree fallback audit: neither baseline '{baseline}' nor fallback \
             '{fallback}' resolved, and the tracked tree has {} files, exceeding the \
             scope.whole_tree_fallback_max_files budget of {max_whole_tree_files}. \
             Raise `whole_tree_fallback_max_files` in `[scope]` (0 = unlimited) if a full-tree \
             audit is intended, or narrow scope by fixing/recording a resolvable baseline ref \
             (e.g. `--baseline` or `specguard`'s recorded last-ref) so the normal changed-files \
             diff is used instead of auditing the whole tree.",
            files.len()
        );
    }
    Ok((files, ALL_TRACKED.to_string(), true))
}

fn git_diff_names(repo_root: &Path, baseline: &str) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--name-only")
        .arg(format!("{baseline}..HEAD"))
        .output()
        .context("spawning git")?;
    if !out.status.success() {
        anyhow::bail!(
            "git diff {baseline}..HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_name_list(&out.stdout))
}

/// All files tracked at HEAD. Used as the final fallback ("audit everything").
fn all_tracked_files(repo_root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-tree", "-r", "--name-only", "HEAD"])
        .output()
        .context("spawning git ls-tree")?;
    if !out.status.success() {
        anyhow::bail!(
            "git ls-tree HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_name_list(&out.stdout))
}

fn parse_name_list(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Record HEAD of `repo_root`, used to advance the baseline after a run.
pub fn current_head(repo_root: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .context("spawning git rev-parse")?;
    if !out.status.success() {
        anyhow::bail!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The file part of a canon pointer (`file` or `file:section`).
fn canon_file(pointer: &str) -> &str {
    pointer.split(':').next().unwrap_or(pointer).trim()
}

/// Whether an invariant belongs in this run's invariants shard: `always`
/// invariants unconditionally (the default, preserving prior behavior — every
/// invariant on every run); non-`always` (diff-scoped) invariants only when
/// the diff touched one of their `canon` paths. Mirrors [`classify`]'s
/// canon-changed check for areas.
pub(crate) fn invariant_in_scope(
    inv: &Invariant,
    changed: &std::collections::HashSet<&str>,
) -> bool {
    inv.always || inv.canon.iter().any(|c| changed.contains(canon_file(c)))
}

/// The invariants in scope for this run: `always` invariants unconditionally,
/// plus non-`always` (diff-scoped) invariants whose canon the diff touched.
/// Single source of truth for both [`shard_input_files`] (the invariants
/// shard's content-hash input) and the prompt's invariants-shard emission /
/// rendering ([`crate::prompt`]) so the two never drift.
pub(crate) fn invariants_in_scope<'a>(cfg: &'a Config, scope: &Scope) -> Vec<&'a Invariant> {
    let changed_set: std::collections::HashSet<&str> =
        scope.changed_files.iter().map(|s| s.as_str()).collect();
    cfg.invariants
        .iter()
        .filter(|inv| invariant_in_scope(inv, &changed_set))
        .collect()
}

/// Pure: map changed files onto configured areas. Returns (in-scope hits,
/// skipped area names). An area is in scope iff a changed file matches its globs
/// OR one of its canon pointers' files changed (so a pure-spec edit re-triggers
/// the audit). Errors only on an invalid glob pattern in the config.
pub fn classify(changed: &[String], areas: &[Area]) -> Result<(Vec<AreaHit>, Vec<String>)> {
    let mut in_scope = Vec::new();
    let mut skipped = Vec::new();
    let changed_set: std::collections::HashSet<&str> = changed.iter().map(|s| s.as_str()).collect();

    for (idx, area) in areas.iter().enumerate() {
        let mut builder = GlobSetBuilder::new();
        for g in &area.globs {
            builder.add(
                Glob::new(g)
                    .with_context(|| format!("invalid glob '{g}' in area '{}'", area.name))?,
            );
        }
        let set = builder.build().context("building glob set")?;

        let matched: Vec<String> = changed
            .iter()
            .filter(|f| set.is_match(f.as_str()))
            .cloned()
            .collect();

        // Canon pointers whose file changed since the baseline (deduped).
        let mut changed_canon: Vec<String> = Vec::new();
        for c in &area.canon {
            let f = canon_file(c);
            if changed_set.contains(f) && !changed_canon.iter().any(|x| x == f) {
                changed_canon.push(f.to_string());
            }
        }

        if matched.is_empty() && changed_canon.is_empty() {
            skipped.push(area.name.clone());
        } else {
            in_scope.push(AreaHit {
                area_index: idx,
                matched_files: matched,
                changed_canon,
            });
        }
    }
    Ok((in_scope, skipped))
}

/// Full resolution: baseline -> diff -> classification.
pub fn resolve(
    cfg: &Config,
    repo_root: &Path,
    override_ref: Option<&str>,
    last_ref: Option<&str>,
) -> Result<Scope> {
    let requested = resolve_baseline(cfg, override_ref, last_ref);
    // Reject a hostile baseline (leading dash / whitespace / shell metacharacters)
    // BEFORE it is handed to `git diff` as a positional argument.
    if !is_safe_ref(&requested) {
        anyhow::bail!(
            "refusing unsafe baseline ref '{requested}': only [A-Za-z0-9_./~^-] are allowed and it must not start with '-'"
        );
    }
    let (changed_files, baseline, fell_back) = changed_files(
        repo_root,
        &requested,
        &cfg.scope.fallback_ref,
        cfg.scope.whole_tree_fallback_max_files,
    )?;
    let (in_scope, skipped_areas) = classify(&changed_files, &cfg.areas)?;
    let decision_files = crate::decision::list_files(repo_root, &cfg.decisions.dir)?;
    Ok(Scope {
        baseline,
        fell_back,
        changed_files,
        in_scope,
        skipped_areas,
        decision_files,
    })
}

/// The set of files backing one shard's audit — its "input": what the
/// content-hash memoization ([`fingerprint_files`]) fingerprints, and what a
/// future relevant-file map (t4) should reuse rather than re-deriving. An area
/// shard's input is its area's canon files UNION its matched (changed)
/// files; the invariants shard's is the union of every invariant's canon; the
/// decisions shard's is its decision records UNION the in-scope canon they
/// cross-reference (mirroring `prompt::render_decisions`'s in-scope-canon
/// computation). Deduplicated and sorted so member order never affects the
/// result.
pub fn shard_input_files(cfg: &Config, scope: &Scope, shard: Shard) -> Vec<String> {
    let mut files: Vec<String> = match shard {
        Shard::Area(i) => {
            let hit = &scope.in_scope[i];
            let area = &cfg.areas[hit.area_index];
            let mut v: Vec<String> = area
                .canon
                .iter()
                .map(|c| canon_file(c).to_string())
                .collect();
            v.extend(hit.matched_files.iter().cloned());
            v
        }
        Shard::Invariants => invariants_in_scope(cfg, scope)
            .into_iter()
            .flat_map(|inv| inv.canon.iter().map(|c| canon_file(c).to_string()))
            .collect(),
        Shard::Decisions => {
            let mut v = scope.decision_files.clone();
            for hit in &scope.in_scope {
                v.extend(
                    cfg.areas[hit.area_index]
                        .canon
                        .iter()
                        .map(|c| canon_file(c).to_string()),
                );
            }
            for inv in &cfg.invariants {
                v.extend(inv.canon.iter().map(|c| canon_file(c).to_string()));
            }
            v
        }
    };
    files.sort();
    files.dedup();
    files
}

/// Upper bound on the relevant-file map handed to an area shard (t4). A BOUNDED
/// list keeps the auditor reading a limited, high-signal set first instead of
/// broadly re-scanning the tree.
pub const RELEVANT_MAP_MAX: usize = 24;

/// Initial code-index `k` (top-K symbols) for the first pass of an area shard.
pub const CODE_INDEX_K: usize = 8;

/// Widened code-index `k` used on the ONE progressive-escalation re-dispatch of a
/// shard that signalled insufficient context (t4 Part B).
pub const CODE_INDEX_K_WIDENED: usize = 20;

/// The `fugu-router` executable name — overridable via `SPECGUARD_FUGU_BIN` so
/// tests can point the deterministic code-index shell-out at a stub (or a bogus
/// path, to exercise the fail-soft "absent" path) without a real install.
fn fugu_bin() -> String {
    std::env::var("SPECGUARD_FUGU_BIN").unwrap_or_else(|_| "fugu-router".to_string())
}

/// The last path segment with any extension stripped — a cheap query term for
/// the code index (e.g. `logging/signature.py` -> `signature`).
fn stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name).to_string()
}

/// A deterministic code-index query for an area shard: the area name plus the
/// stems of its canon and (a bounded prefix of) its changed files. Non-area
/// shards get no query (the relevant-file map is an area-shard concept). Passed
/// to `fugu-router code-index search` as a single `--query` argument (no shell).
pub fn shard_query(cfg: &Config, scope: &Scope, shard: Shard) -> String {
    match shard {
        Shard::Area(i) => {
            let hit = &scope.in_scope[i];
            let area = &cfg.areas[hit.area_index];
            let mut terms = vec![area.name.clone()];
            for c in &area.canon {
                terms.push(stem(canon_file(c)));
            }
            for f in hit.matched_files.iter().take(8) {
                terms.push(stem(f));
            }
            terms.join(" ")
        }
        Shard::Invariants | Shard::Decisions => String::new(),
    }
}

/// Shell out to the repo's deterministic code index (`fugu-router`) to enrich a
/// shard's relevant-file map with the `.rs` symbol files most relevant to
/// `query`. Runs `code-index build --if-stale` (idempotent, cheap when the `.rs`
/// set is unchanged) then `code-index search --query <q> --k <k>`, parsing the
/// JSON array of `{name,kind,file,line,signature,score}`.
///
/// The JSON is treated as UNTRUSTED DATA: only the `file` field is read, only
/// `.rs` paths are kept, and nothing in it is ever executed or interpreted as an
/// instruction. FAIL-SOFT by construction: a missing binary, a non-zero exit,
/// non-UTF8/……invalid JSON, or an empty array all yield an empty vector, so the
/// caller falls back to today's behavior (no enrichment).
pub fn code_index_files(bin: &str, repo_root: &Path, query: &str, k: usize) -> Vec<String> {
    if query.trim().is_empty() || k == 0 {
        return Vec::new();
    }
    // Best-effort index refresh; ignore its outcome (search fail-softs on an
    // absent/empty index anyway).
    let _ = Command::new(bin)
        .args(["code-index", "build", "--if-stale", "--root"])
        .arg(repo_root)
        .output();

    let out = Command::new(bin)
        .args(["code-index", "search", "--query"])
        .arg(query)
        .arg("--k")
        .arg(k.to_string())
        .arg("--root")
        .arg(repo_root)
        .output();
    let Ok(out) = out else {
        return Vec::new(); // binary absent / spawn failed -> fail-soft
    };
    if !out.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_code_index_files(&stdout)
}

/// Pure JSON-array parse (split out for testing): extract the `.rs` `file` paths
/// from a `fugu-router code-index search` payload, deduped in first-seen order.
/// Any parse failure yields an empty vector (fail-soft — untrusted data).
fn parse_code_index_files(stdout: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Hit {
        #[serde(default)]
        file: String,
    }
    let Ok(hits) = serde_json::from_str::<Vec<Hit>>(stdout.trim()) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .map(|h| h.file)
        .filter(|f| f.ends_with(".rs") && !f.trim().is_empty())
        .filter(|f| seen.insert(f.clone()))
        .collect()
}

/// Merge a shard's base input files with code-index extras into a BOUNDED,
/// deterministic relevant-file map. Base files (the shard's own canon + changed
/// files — always relevant) take priority; extras fill the remaining budget.
/// Deduped and sorted so member/query order never affects the result; capped at
/// `max` so the auditor's first-read set stays bounded.
fn merge_relevant_map(base: Vec<String>, extra: Vec<String>, max: usize) -> Vec<String> {
    let mut base = base;
    base.sort();
    base.dedup();
    let mut out: Vec<String> = base.into_iter().take(max).collect();
    let mut seen: std::collections::HashSet<String> = out.iter().cloned().collect();
    let mut extra = extra;
    extra.sort();
    extra.dedup();
    for f in extra {
        if out.len() >= max {
            break;
        }
        if seen.insert(f.clone()) {
            out.push(f);
        }
    }
    out.sort();
    out
}

/// The bounded relevant-file map for an area shard (t4 Part A): the shard's own
/// input files ([`shard_input_files`]) enriched with the most-relevant `.rs`
/// symbol files from the deterministic code index, capped at [`RELEVANT_MAP_MAX`].
/// With `fugu-router` absent/erroring/returning `[]`, the enrichment is empty and
/// the map degrades to the base set — never errors, no network.
pub fn relevant_file_map(
    cfg: &Config,
    scope: &Scope,
    shard: Shard,
    repo_root: &Path,
    query: &str,
    k: usize,
) -> Vec<String> {
    let base = shard_input_files(cfg, scope, shard);
    let extra = code_index_files(&fugu_bin(), repo_root, query, k);
    merge_relevant_map(base, extra, RELEVANT_MAP_MAX)
}

/// Deterministic content fingerprint over a shard's input files (see
/// [`shard_input_files`]): hashes path + current file bytes (relative to
/// `repo_root`; an already-absolute entry, e.g. a decision record, is read
/// as-is) in sorted order via the shared FNV-1a 64-bit hash (see
/// `ratify::hash` for the same idiom over prompt templates) — deterministic,
/// no network, and stable across releases (unlike `DefaultHasher`). A missing
/// file hashes as present-with-empty-content, which still differs from that
/// path never having been in the set at all (its name is part of the digest
/// too). Used by the content-hash memoization that skips re-auditing a shard
/// whose input hasn't changed since its last clean audit (see `main.rs`).
pub fn fingerprint_files(repo_root: &Path, files: &[String]) -> String {
    let mut sorted: Vec<String> = files.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut h = harness_core::hash::Fnv1a64::new();
    for f in &sorted {
        h.update(f.as_bytes());
        h.update(&[0]);
        let full = if Path::new(f).is_absolute() {
            PathBuf::from(f)
        } else {
            repo_root.join(f)
        };
        if let Ok(bytes) = std::fs::read(&full) {
            h.update(&bytes);
        }
        h.update(&[0]);
    }
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Area;

    fn area(name: &str, globs: &[&str]) -> Area {
        Area {
            name: name.to_string(),
            globs: globs.iter().map(|s| s.to_string()).collect(),
            canon: vec![],
        }
    }

    fn area_with_canon(name: &str, globs: &[&str], canon: &[&str]) -> Area {
        Area {
            name: name.to_string(),
            globs: globs.iter().map(|s| s.to_string()).collect(),
            canon: canon.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn is_safe_ref_rejects_dash_and_metacharacters() {
        // Leading-dash → git would parse it as an option (arg injection).
        assert!(!is_safe_ref("--upload-pack=evil"));
        assert!(!is_safe_ref("-anything"));
        // Shell metacharacters / whitespace must be rejected.
        assert!(!is_safe_ref("main; touch x"));
        assert!(!is_safe_ref("main | cat"));
        assert!(!is_safe_ref("a$b"));
        assert!(!is_safe_ref("a`b`"));
        assert!(!is_safe_ref("a b"));
        assert!(!is_safe_ref(""));
        assert!(!is_safe_ref("   "));
        // Genuine refs and ranges stay valid.
        assert!(is_safe_ref("main"));
        assert!(is_safe_ref("HEAD~3"));
        assert!(is_safe_ref("HEAD~20"));
        assert!(is_safe_ref("feature/x"));
        assert!(is_safe_ref("v1.2.3"));
        assert!(is_safe_ref("main~1^2"));
    }

    #[test]
    fn classify_matches_recursive_glob() {
        let areas = vec![
            area("logging", &["aegis_logging/**"]),
            area("web", &["web/**"]),
        ];
        let changed = vec![
            "aegis_logging/signature.py".to_string(),
            "aegis_logging/vector/cfg.toml".to_string(),
            "README.md".to_string(),
        ];
        let (hits, skipped) = classify(&changed, &areas).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].area_index, 0);
        assert_eq!(hits[0].matched_files.len(), 2);
        assert!(hits[0].changed_canon.is_empty());
        assert_eq!(skipped, vec!["web".to_string()]);
    }

    #[test]
    fn classify_triggers_on_canon_change_only() {
        // No code change in the area, but its canon doc changed -> in scope, via
        // changed_canon (the file part of a `file:section` pointer matches).
        let areas = vec![area_with_canon(
            "logging",
            &["aegis_logging/**"],
            &["docs/logging.md:HMAC", "docs/glossary.md"],
        )];
        let changed = vec!["docs/logging.md".to_string()];
        let (hits, skipped) = classify(&changed, &areas).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].matched_files.is_empty(), "no impl change");
        assert_eq!(hits[0].changed_canon, vec!["docs/logging.md".to_string()]);
        assert!(skipped.is_empty());
    }

    #[test]
    fn classify_empty_diff_skips_all() {
        let areas = vec![area("a", &["a/**"])];
        let (hits, skipped) = classify(&[], &areas).unwrap();
        assert!(hits.is_empty());
        assert_eq!(skipped, vec!["a".to_string()]);
    }

    #[test]
    fn classify_multiple_globs_per_area() {
        let areas = vec![area("api", &["admin/**", "web/api/**"])];
        let changed = vec!["web/api/users.py".to_string()];
        let (hits, _) = classify(&changed, &areas).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn resolve_baseline_precedence() {
        let mut cfg: Config = toml::from_str(
            r#"
            [project]
            name = "x"
            [[area]]
            name = "a"
            globs = ["a/**"]
            "#,
        )
        .unwrap();
        // override wins
        assert_eq!(resolve_baseline(&cfg, Some("abc"), Some("zzz")), "abc");
        // then config.baseline_ref
        cfg.scope.baseline_ref = "cfgref".to_string();
        assert_eq!(resolve_baseline(&cfg, None, Some("zzz")), "cfgref");
        // then last_ref
        cfg.scope.baseline_ref = "".to_string();
        assert_eq!(resolve_baseline(&cfg, None, Some("zzz")), "zzz");
        // then fallback
        assert_eq!(resolve_baseline(&cfg, None, None), "HEAD~20");
    }

    fn sample_cfg_with_canon() -> Config {
        toml::from_str(
            r#"
            [project]
            name = "x"
            [[area]]
            name = "logging"
            globs = ["logging/**"]
            canon = ["docs/logging.md:HMAC"]
            [[invariant]]
            name = "signing"
            canon = ["docs/signing.md"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn shard_input_files_area_unions_canon_and_matched_deduped_sorted() {
        let cfg = sample_cfg_with_canon();
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec![],
            in_scope: vec![AreaHit {
                area_index: 0,
                matched_files: vec!["logging/sig.py".into(), "logging/vector.py".into()],
                changed_canon: vec![],
            }],
            skipped_areas: vec![],
            decision_files: vec![],
        };
        let files = shard_input_files(&cfg, &scope, Shard::Area(0));
        // canon_file() strips the `:HMAC` section suffix.
        assert_eq!(
            files,
            vec![
                "docs/logging.md".to_string(),
                "logging/sig.py".to_string(),
                "logging/vector.py".to_string(),
            ]
        );
    }

    /// A single-invariant config whose `always` flag is set by the caller —
    /// used by the diff-scoped-invariant tests below.
    fn cfg_with_invariant(always: bool) -> Config {
        toml::from_str(&format!(
            r#"
            [project]
            name = "x"
            [[invariant]]
            name = "scoped"
            canon = ["docs/scoped.md"]
            always = {always}
            "#,
        ))
        .unwrap()
    }

    #[test]
    fn shard_input_files_invariant_always_true_included_even_when_diff_untouched() {
        let cfg = cfg_with_invariant(true);
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec!["unrelated/file.rs".to_string()],
            in_scope: vec![],
            skipped_areas: vec![],
            decision_files: vec![],
        };
        let files = shard_input_files(&cfg, &scope, Shard::Invariants);
        assert_eq!(files, vec!["docs/scoped.md".to_string()]);
    }

    #[test]
    fn shard_input_files_invariant_not_always_excluded_when_diff_untouched() {
        let cfg = cfg_with_invariant(false);
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec!["unrelated/file.rs".to_string()],
            in_scope: vec![],
            skipped_areas: vec![],
            decision_files: vec![],
        };
        let files = shard_input_files(&cfg, &scope, Shard::Invariants);
        assert!(
            files.is_empty(),
            "always=false invariant whose canon was not touched must be excluded"
        );
    }

    #[test]
    fn shard_input_files_invariant_not_always_included_when_diff_touches_canon() {
        let cfg = cfg_with_invariant(false);
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec!["docs/scoped.md".to_string()],
            in_scope: vec![],
            skipped_areas: vec![],
            decision_files: vec![],
        };
        let files = shard_input_files(&cfg, &scope, Shard::Invariants);
        assert_eq!(files, vec!["docs/scoped.md".to_string()]);
    }

    #[test]
    fn shard_input_files_invariants_uses_invariant_canon() {
        let cfg = sample_cfg_with_canon();
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec![],
            in_scope: vec![],
            skipped_areas: vec![],
            decision_files: vec![],
        };
        let files = shard_input_files(&cfg, &scope, Shard::Invariants);
        assert_eq!(files, vec!["docs/signing.md".to_string()]);
    }

    #[test]
    fn shard_input_files_decisions_unions_records_and_inscope_canon() {
        let cfg = sample_cfg_with_canon();
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec![],
            in_scope: vec![AreaHit {
                area_index: 0,
                matched_files: vec![],
                changed_canon: vec![],
            }],
            skipped_areas: vec![],
            decision_files: vec!["/vault/decisions/x.md".into()],
        };
        let files = shard_input_files(&cfg, &scope, Shard::Decisions);
        assert_eq!(
            files,
            vec![
                "/vault/decisions/x.md".to_string(),
                "docs/logging.md".to_string(),
                "docs/signing.md".to_string(),
            ]
        );
    }

    #[test]
    fn fingerprint_files_is_deterministic_and_order_independent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "alpha").unwrap();
        std::fs::write(tmp.path().join("b.md"), "beta").unwrap();
        let f1 = fingerprint_files(tmp.path(), &["a.md".to_string(), "b.md".to_string()]);
        let f2 = fingerprint_files(tmp.path(), &["b.md".to_string(), "a.md".to_string()]);
        assert_eq!(f1, f2, "member order must not affect the fingerprint");
        // Deterministic across repeated calls with the same input.
        assert_eq!(
            f1,
            fingerprint_files(tmp.path(), &["a.md".to_string(), "b.md".to_string()])
        );
    }

    /// (b) The re-audit path: changed input content -> the fingerprint differs.
    #[test]
    fn fingerprint_files_changes_when_content_changes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "alpha").unwrap();
        let before = fingerprint_files(tmp.path(), &["a.md".to_string()]);
        std::fs::write(tmp.path().join("a.md"), "alpha, changed").unwrap();
        let after = fingerprint_files(tmp.path(), &["a.md".to_string()]);
        assert_ne!(
            before, after,
            "content change must produce a different fingerprint"
        );
    }

    /// (a) The skip path's underlying invariant: identical content on disk (no
    /// edits between two calls) yields an identical fingerprint, which is what
    /// lets the caller (main.rs) treat the shard as unchanged and skip it.
    #[test]
    fn fingerprint_files_unchanged_content_yields_same_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "alpha").unwrap();
        let first = fingerprint_files(tmp.path(), &["a.md".to_string()]);
        let second = fingerprint_files(tmp.path(), &["a.md".to_string()]);
        assert_eq!(first, second);
    }

    #[test]
    fn fingerprint_files_handles_missing_file_without_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        // No file written at all — must not panic/error, just fingerprint the
        // path with empty content.
        let fp = fingerprint_files(tmp.path(), &["missing.md".to_string()]);
        assert_eq!(fp.len(), 16);
    }

    // -- relevant-file map (t4 Part A) --------------------------------------

    #[test]
    fn parse_code_index_files_keeps_only_rs_deduped() {
        let json = r#"[
            {"name":"a","kind":"fn","file":"src/a.rs","line":1,"signature":"fn a()","score":0.9},
            {"name":"b","kind":"fn","file":"src/b.rs","line":2,"signature":"fn b()","score":0.8},
            {"name":"a2","kind":"fn","file":"src/a.rs","line":9,"signature":"fn a2()","score":0.5},
            {"name":"doc","kind":"md","file":"docs/spec.md","line":1,"signature":"","score":0.4}
        ]"#;
        let files = parse_code_index_files(json);
        // .md dropped, .rs kept in first-seen order, duplicate a.rs collapsed.
        assert_eq!(files, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
    }

    #[test]
    fn parse_code_index_files_bad_json_or_empty_is_empty() {
        assert!(parse_code_index_files("not json at all").is_empty());
        assert!(parse_code_index_files("[]").is_empty());
        assert!(parse_code_index_files("").is_empty());
    }

    /// A stub `fugu-router` (a shell script that prints a fixed JSON array) is
    /// driven through `code_index_files`: only the `.rs` files come back.
    #[test]
    fn code_index_files_reads_stub_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = tmp.path().join("fugu-stub.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\nprintf '%s' '[{\"file\":\"src/x.rs\",\"kind\":\"fn\"},{\"file\":\"docs/y.md\"}]'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let files = code_index_files(stub.to_str().unwrap(), tmp.path(), "some query", 8);
        assert_eq!(files, vec!["src/x.rs".to_string()]);
    }

    /// Fail-soft: an ABSENT fugu-router binary yields an empty enrichment (no
    /// panic, no error) — this is the graceful fallback to today's behavior.
    #[test]
    fn code_index_files_absent_binary_falls_back_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let files = code_index_files("specguard-no-such-fugu-binary-xyz", tmp.path(), "query", 8);
        assert!(files.is_empty(), "absent binary must fail-soft to empty");
    }

    #[test]
    fn code_index_files_empty_query_short_circuits() {
        let tmp = tempfile::tempdir().unwrap();
        // Even a working stub is never consulted for an empty query.
        assert!(code_index_files("fugu-router", tmp.path(), "   ", 8).is_empty());
        assert!(code_index_files("fugu-router", tmp.path(), "q", 0).is_empty());
    }

    #[test]
    fn merge_relevant_map_is_bounded_sorted_deduped_base_first() {
        // base has 3 files; extra adds more, some duplicating base, total > max.
        let base: Vec<String> = vec!["b.rs".into(), "a.rs".into(), "a.rs".into()];
        let extra: Vec<String> = (0..30).map(|i| format!("z{i:02}.rs")).collect();
        let map = merge_relevant_map(base, extra, RELEVANT_MAP_MAX);
        assert!(map.len() <= RELEVANT_MAP_MAX, "map must be bounded");
        // Base files survive the truncation (they take priority over extras).
        assert!(map.contains(&"a.rs".to_string()));
        assert!(map.contains(&"b.rs".to_string()));
        // Sorted + deduped.
        let mut sorted = map.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(map, sorted, "map must be sorted and deduped");
    }

    #[test]
    fn merge_relevant_map_no_extra_is_just_base() {
        let base: Vec<String> = vec!["m.rs".into(), "k.rs".into()];
        let map = merge_relevant_map(base, vec![], RELEVANT_MAP_MAX);
        assert_eq!(map, vec!["k.rs".to_string(), "m.rs".to_string()]);
    }

    #[test]
    fn shard_query_area_includes_name_and_stems() {
        let cfg = sample_cfg_with_canon();
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec![],
            in_scope: vec![AreaHit {
                area_index: 0,
                matched_files: vec!["logging/signature.py".into()],
                changed_canon: vec![],
            }],
            skipped_areas: vec![],
            decision_files: vec![],
        };
        let q = shard_query(&cfg, &scope, Shard::Area(0));
        assert!(q.contains("logging"), "area name in query: {q}");
        assert!(q.contains("signature"), "changed-file stem in query: {q}");
        // Non-area shards have no query (map is an area-shard concept).
        assert!(shard_query(&cfg, &scope, Shard::Invariants).is_empty());
    }

    /// End-to-end Part A: `relevant_file_map` is BOUNDED and PRESENT, containing
    /// the shard's base input files plus stub code-index `.rs` extras.
    #[test]
    fn relevant_file_map_is_bounded_and_contains_base_plus_extras() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = tmp.path().join("fugu-stub.sh");
        // Stub returns many .rs symbol files (more than the base) + a .md (dropped).
        let mut arr = String::from("[");
        for i in 0..30 {
            if i > 0 {
                arr.push(',');
            }
            arr.push_str(&format!("{{\"file\":\"src/gen_{i:02}.rs\"}}"));
        }
        arr.push_str(",{\"file\":\"docs/x.md\"}]");
        std::fs::write(&stub, format!("#!/bin/sh\nprintf '%s' '{arr}'\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let cfg = sample_cfg_with_canon();
        let scope = Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec![],
            in_scope: vec![AreaHit {
                area_index: 0,
                matched_files: vec!["logging/sig.py".into()],
                changed_canon: vec![],
            }],
            skipped_areas: vec![],
            decision_files: vec![],
        };

        std::env::set_var("SPECGUARD_FUGU_BIN", stub.to_str().unwrap());
        let map = relevant_file_map(&cfg, &scope, Shard::Area(0), tmp.path(), "logging sig", 8);
        std::env::remove_var("SPECGUARD_FUGU_BIN");

        assert!(!map.is_empty(), "map must be present");
        assert!(map.len() <= RELEVANT_MAP_MAX, "map must be bounded");
        // Base files (canon + changed) survive.
        assert!(map.contains(&"docs/logging.md".to_string()));
        assert!(map.contains(&"logging/sig.py".to_string()));
        // Enriched with code-index .rs extras.
        assert!(map.iter().any(|f| f.starts_with("src/gen_")));
        // The .md from the index was dropped by the .rs filter.
        assert!(!map.contains(&"docs/x.md".to_string()));
    }

    // -- whole-tree fallback budget guard -----------------------------------

    fn test_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A young repo (one commit) with `n` tracked files, ready to exercise the
    /// tier-3 "audit everything" fallback via a bogus baseline/fallback ref.
    fn repo_with_n_tracked_files(n: usize) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        test_git(repo, &["init", "-q"]);
        test_git(repo, &["config", "user.email", "t@t.t"]);
        test_git(repo, &["config", "user.name", "t"]);
        test_git(repo, &["config", "commit.gpgsign", "false"]);
        for i in 0..n {
            std::fs::write(repo.join(format!("f{i}.txt")), "x").unwrap();
        }
        test_git(repo, &["add", "-A"]);
        test_git(repo, &["commit", "-q", "-m", "seed"]);
        tmp
    }

    /// (a) Whole-tree fallback set exceeds the configured budget -> refused
    /// with an `Err` naming the count and the budget (not silently audited).
    #[test]
    fn whole_tree_fallback_exceeding_budget_is_refused() {
        let tmp = repo_with_n_tracked_files(5);
        let err = changed_files(tmp.path(), "no-such-ref", "also-no-such-ref", 3)
            .expect_err("5 tracked files must exceed a budget of 3");
        let msg = err.to_string();
        assert!(msg.contains('5'), "message should name the count: {msg}");
        assert!(msg.contains('3'), "message should name the budget: {msg}");
    }

    /// (b) Within budget: proceeds exactly as today (falls back to all
    /// tracked files, `fell_back = true`, `ALL_TRACKED` label).
    #[test]
    fn whole_tree_fallback_within_budget_proceeds_as_today() {
        let tmp = repo_with_n_tracked_files(5);
        let (files, baseline, fell_back) =
            changed_files(tmp.path(), "no-such-ref", "also-no-such-ref", 10)
                .expect("5 tracked files must fit within a budget of 10");
        assert_eq!(files.len(), 5);
        assert_eq!(baseline, ALL_TRACKED);
        assert!(fell_back);
    }

    /// (b) Budget disabled (`0`, the field default) proceeds exactly as
    /// today regardless of tree size — the historical, backward-compatible
    /// behavior that `unresolvable_baseline_falls_back_to_all_tracked`
    /// (tests/integration.rs) depends on.
    #[test]
    fn whole_tree_fallback_disabled_budget_proceeds_unbounded() {
        let tmp = repo_with_n_tracked_files(50);
        let (files, baseline, fell_back) =
            changed_files(tmp.path(), "no-such-ref", "also-no-such-ref", 0)
                .expect("budget 0 must never refuse");
        assert_eq!(files.len(), 50);
        assert_eq!(baseline, ALL_TRACKED);
        assert!(fell_back);
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::config::Area;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn mk_area(name: &str, globs: Vec<String>) -> Area {
        Area {
            name: name.into(),
            globs,
            canon: vec![],
        }
    }

    fn all_hit_files(hits: &[AreaHit]) -> Vec<String> {
        hits.iter()
            .flat_map(|h| h.matched_files.iter().cloned())
            .collect()
    }

    proptest! {
        /// With no areas, no changed files are classified into any area hit.
        #[test]
        fn no_areas_no_hits(
            files in prop::collection::vec("[a-z]{3,6}", 1..8),
        ) {
            let changed: Vec<String> = files.iter().map(|f| format!("src/{f}.rs")).collect();
            let (hits, _oob) = classify(&changed, &[]).unwrap();
            prop_assert!(hits.is_empty(), "with no areas there should be no hits");
        }

        /// With no changed files, no area hits are produced.
        #[test]
        fn empty_changed_no_hits(
            n_areas in 0usize..4,
        ) {
            let areas: Vec<Area> = (0..n_areas)
                .map(|i| mk_area(&format!("a{i}"), vec![format!("src/a{i}/**")]))
                .collect();
            let (hits, _oob) = classify(&[], &areas).unwrap();
            // hits come from matched changed files; with none, no area should have hits
            prop_assert!(hits.iter().all(|h| h.matched_files.is_empty()),
                "no changed files should yield no matched_files in any hit");
        }

        /// area_index is always < areas.len().
        #[test]
        fn area_index_always_in_bounds(
            files in prop::collection::vec("[a-z]{3,5}", 1..6),
            n_areas in 1usize..4,
        ) {
            let areas: Vec<Area> = (0..n_areas)
                .map(|i| mk_area(&format!("a{i}"), vec![format!("src/a{i}/**")]))
                .collect();
            let changed: Vec<String> = files.iter().map(|f| format!("src/a0/{f}.rs")).collect();
            let (hits, _) = classify(&changed, &areas).unwrap();
            for h in &hits {
                prop_assert!(h.area_index < areas.len(),
                    "area_index {} >= areas.len() {}", h.area_index, areas.len());
            }
        }

        /// No file appears in more than one AreaHit.
        #[test]
        fn no_file_in_multiple_hits(
            files in prop::collection::vec("[a-z]{3,5}", 1..6),
        ) {
            let areas = vec![
                mk_area("a0", vec!["src/a0/**".into()]),
                mk_area("a1", vec!["src/a1/**".into()]),
            ];
            let changed: Vec<String> = files.iter().enumerate().map(|(i, f)| {
                if i % 2 == 0 { format!("src/a0/{f}.rs") } else { format!("src/a1/{f}.rs") }
            }).collect();
            let (hits, _) = classify(&changed, &areas).unwrap();
            let all: Vec<String> = all_hit_files(&hits);
            let unique: HashSet<&String> = all.iter().collect();
            prop_assert_eq!(all.len(), unique.len(), "a file appeared in multiple hits");
        }

        /// Files in out_of_scope don't appear in any AreaHit.
        #[test]
        fn out_of_scope_disjoint_from_hits(
            files in prop::collection::vec("[a-z]{3,5}", 1..8),
        ) {
            let areas = vec![mk_area("a0", vec!["src/a0/**".into()])];
            let changed: Vec<String> = files.iter().map(|f| format!("src/other/{f}.rs")).collect();
            let (hits, oob) = classify(&changed, &areas).unwrap();
            let hit_set: HashSet<String> = all_hit_files(&hits).into_iter().collect();
            for f in &oob {
                prop_assert!(!hit_set.contains(f), "file {f} in both oob and hits");
            }
        }

        /// Total files in hits + out_of_scope equals changed (no file lost or duplicated).
        #[test]
        fn total_files_preserved(
            files in prop::collection::vec("[a-z]{3,6}", 1..8),
        ) {
            let areas = vec![mk_area("a0", vec!["src/**".into()])];
            let changed: Vec<String> = {
                let mut v: Vec<String> = files.iter().map(|f| format!("src/{f}.rs")).collect();
                v.sort(); v.dedup(); v
            };
            let (hits, oob) = classify(&changed, &areas).unwrap();
            let total_out = all_hit_files(&hits).len() + oob.len();
            prop_assert_eq!(total_out, changed.len());
        }

        /// Files matching an area's glob appear in that area's hit.
        #[test]
        fn matching_file_in_hit(f in "[a-z]{3,6}") {
            let file = format!("src/{f}.rs");
            let areas = vec![mk_area("a0", vec!["src/**".into()])];
            let (hits, oob) = classify(std::slice::from_ref(&file), &areas).unwrap();
            prop_assert!(oob.is_empty(), "file {file} should be in-scope");
            prop_assert!(!hits.is_empty());
            prop_assert!(hits[0].matched_files.contains(&file));
        }

        /// File not matching any area glob never appears in any hit's matched_files.
        #[test]
        fn non_matching_file_not_in_hits(f in "[a-z]{3,6}") {
            let file = format!("other/{f}.rs");
            let areas = vec![mk_area("a0", vec!["src/**".into()])];
            let (hits, _oob) = classify(std::slice::from_ref(&file), &areas).unwrap();
            for h in &hits {
                prop_assert!(!h.matched_files.contains(&file),
                    "non-matching file {file} should not appear in any hit");
            }
        }

        /// Number of AreaHits never exceeds number of areas.
        #[test]
        fn hit_count_le_area_count(
            files in prop::collection::vec("[a-z]{3,5}", 1..8),
            n_areas in 1usize..5,
        ) {
            let areas: Vec<Area> = (0..n_areas)
                .map(|i| mk_area(&format!("a{i}"), vec![format!("src/a{i}/**")]))
                .collect();
            let changed: Vec<String> = files.iter().map(|f| format!("src/a0/{f}.rs")).collect();
            let (hits, _) = classify(&changed, &areas).unwrap();
            prop_assert!(hits.len() <= n_areas);
        }

        /// changed_canon files are also in matched_files for that hit.
        #[test]
        fn changed_canon_subset_of_matched_files(f in "[a-z]{3,6}") {
            let canon_file = format!("src/{f}.md");
            let impl_file = format!("src/{f}.rs");
            let areas = vec![Area {
                name: "a0".into(),
                globs: vec!["src/**".into()],
                canon: vec![canon_file.clone()],
            }];
            let changed = vec![canon_file.clone(), impl_file];
            let (hits, _) = classify(&changed, &areas).unwrap();
            if let Some(h) = hits.iter().find(|h| h.area_index == 0) {
                for cc in &h.changed_canon {
                    prop_assert!(h.matched_files.contains(cc) || changed.contains(cc),
                        "changed_canon file {cc} not found");
                }
            }
        }
    }
}
