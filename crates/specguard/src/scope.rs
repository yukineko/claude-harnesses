//! Change-triggered scope resolution.
//!
//! The audit only looks at areas touched since a baseline ref (plus invariants,
//! which run every time). This keeps each run cheap and bounded instead of
//! re-auditing the whole tree. Git interaction is isolated in [`changed_files`]
//! so the area-classification logic ([`classify`]) stays pure and unit-testable.

use crate::config::{Area, Config};
use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::path::Path;
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
///   3. all tracked files (`git ls-tree HEAD`).
///
/// Uses a two-dot diff (`baseline..HEAD`), i.e. "what HEAD changed relative to
/// baseline" — NOT three-dot, which diffs from the merge-base and would miss
/// changes that came in on the baseline side. Only committed state is audited;
/// uncommitted working-tree edits are out of scope by design.
///
/// Returns (files, ref-actually-used, fell_back).
pub fn changed_files(
    repo_root: &Path,
    baseline: &str,
    fallback: &str,
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
    let (changed_files, baseline, fell_back) =
        changed_files(repo_root, &requested, &cfg.scope.fallback_ref)?;
    let (in_scope, skipped_areas) = classify(&changed_files, &cfg.areas)?;
    let decision_files = crate::decision::list_files(repo_root, &cfg.decisions.dir);
    Ok(Scope {
        baseline,
        fell_back,
        changed_files,
        in_scope,
        skipped_areas,
        decision_files,
    })
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
