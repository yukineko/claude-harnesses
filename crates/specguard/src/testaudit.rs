// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! testaudit — pure-function core for detecting skipped / unreachable tests.
//!
//! All functions in this module are pure: they take string slices and return
//! owned data.  I/O (filesystem, git) is left to the caller; this module only
//! analyses Rust source text passed in as `&str`.
//!
//! # Detection kinds
//!
//! | Kind | Description |
//! |------|-------------|
//! | [`TestFindingKind::Ignored`] | `#[test]` annotated with `#[ignore]` |
//! | [`TestFindingKind::CfgGated`] | `#[test]` inside a `#[cfg(not(test))]` block, or a `#[test]` at top-level that is paired with a cfg-gate that effectively excludes it |
//! | [`TestFindingKind::UnincludedMod`] | A `tests` module file exists on disk but is not referenced by a `mod` declaration in its parent |
//! | [`TestFindingKind::IntegrationTest`] | A Rust file under a `tests/` directory (integration-test candidates) |

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Category of a test-audit finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestFindingKind {
    /// The test function carries `#[ignore]`.
    Ignored,
    /// The test is gated by a cfg expression that prevents it from running
    /// in normal `cargo test` invocations (e.g. `#[cfg(not(test))]`).
    CfgGated,
    /// A file that looks like a test module exists on disk but is not
    /// declared with `mod` in its parent source file.
    UnincludedMod,
    /// A Rust source file under a `tests/` directory (integration test).
    IntegrationTest,
}

impl TestFindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TestFindingKind::Ignored => "ignored",
            TestFindingKind::CfgGated => "cfg_gated",
            TestFindingKind::UnincludedMod => "unincluded_mod",
            TestFindingKind::IntegrationTest => "integration_test",
        }
    }
}

/// A single finding produced by the audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFinding {
    /// Category.
    pub kind: TestFindingKind,
    /// Relative file path (as provided by the caller).
    pub file: String,
    /// Test / module name (empty when not applicable).
    pub name: String,
    /// Human-readable reason explaining why this was flagged.
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Scan a single Rust source file for skipped / unreachable tests and report
/// each as a [`TestFinding`].
///
/// # Arguments
/// * `src`  — full text of the Rust source file.
/// * `file` — display path (used in findings; not read from disk).
///
/// # Detection
/// * `#[ignore]` on a `#[test]` → [`TestFindingKind::Ignored`]
/// * `#[test]` inside a `#[cfg(not(test))]` block → [`TestFindingKind::CfgGated`]
/// * A `#[test]` decorated with a cfg-gate that names a target platform
///   (`#[cfg(target_os = "...")]` / `#[cfg(target_arch = "...")]` etc.) →
///   [`TestFindingKind::CfgGated`]
/// * A file whose path contains `tests/` → [`TestFindingKind::IntegrationTest`]
///
/// Note: cfg-gate detection is intentionally conservative (single-level
/// pattern matching).  Complex nested expressions are not analysed.
pub fn scan_source(src: &str, file: &str) -> Vec<TestFinding> {
    let mut findings = Vec::new();

    // Integration-test file detection: any path component named "tests".
    if is_integration_test_path(file) {
        findings.push(TestFinding {
            kind: TestFindingKind::IntegrationTest,
            file: file.to_string(),
            name: String::new(),
            reason: "file is located under a tests/ directory (integration test)".to_string(),
        });
        // Integration test files may also contain ignored / cfg-gated tests —
        // fall through and scan the body too.
    }

    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();

        // Detect entry into a `#[cfg(not(test))]` block.  Everything inside
        // such a block is excluded from `cargo test` runs.
        if is_cfg_not_test(line) {
            // Collect every `#[test]` fn name within this cfg block.
            let depth_start = i;
            let tests_in_block = collect_tests_in_cfg_block(&lines, &mut i);
            for name in tests_in_block {
                findings.push(TestFinding {
                    kind: TestFindingKind::CfgGated,
                    file: file.to_string(),
                    name,
                    reason: format!(
                        "test is inside a `#[cfg(not(test))]` block (line {})",
                        depth_start + 1
                    ),
                });
            }
            continue; // `i` was already advanced by collect_tests_in_cfg_block
        }

        // Detect `#[test]` (possibly preceded by attributes on adjacent lines).
        // Strategy: accumulate attribute lines, then check the `fn` line.
        if is_test_attr(line) {
            // Gather remaining attributes and the fn declaration.
            let mut attrs: Vec<&str> = vec![line];
            let mut j = i + 1;
            while j < lines.len() {
                let next = lines[j].trim();
                if next.starts_with('#') || next.starts_with("//") {
                    attrs.push(next);
                    j += 1;
                } else {
                    break;
                }
            }
            let fn_line = if j < lines.len() { lines[j].trim() } else { "" };
            let name = extract_fn_name(fn_line).unwrap_or_default();

            if has_ignore_attr(&attrs) {
                findings.push(TestFinding {
                    kind: TestFindingKind::Ignored,
                    file: file.to_string(),
                    name: name.clone(),
                    reason: "#[ignore] attribute on test function".to_string(),
                });
            }

            if has_platform_cfg_attr(&attrs) {
                findings.push(TestFinding {
                    kind: TestFindingKind::CfgGated,
                    file: file.to_string(),
                    name,
                    reason: "test is gated by a platform-specific cfg attribute".to_string(),
                });
            }

            i += 1;
            continue;
        }

        i += 1;
    }

    findings
}

/// Detect test module files that exist on disk but are not declared with a
/// `mod` statement in their parent source file.
///
/// # Arguments
/// * `parent_src`   — source text of the parent module (e.g. `src/lib.rs`).
/// * `parent_path`  — display path of the parent file (used in findings).
/// * `candidate_paths` — paths of files that *might* be child modules
///   (e.g. every `*.rs` in the same directory).  The function derives the
///   expected module name from the file stem and checks whether the parent
///   declares it.
///
/// # Returns
/// One [`TestFinding`] with kind [`TestFindingKind::UnincludedMod`] for each
/// candidate whose stem is not found as a `mod <stem>` declaration in
/// `parent_src`.
pub fn find_unincluded_mods(
    parent_src: &str,
    parent_path: &str,
    candidate_paths: &[&str],
) -> Vec<TestFinding> {
    let declared = collect_mod_declarations(parent_src);
    let mut findings = Vec::new();

    for &candidate in candidate_paths {
        let stem = file_stem(candidate);
        if stem.is_empty() {
            continue;
        }
        // Skip the parent itself and common non-module stems.
        if is_self_or_ignored_stem(stem, parent_path) {
            continue;
        }
        if !declared.contains(&stem.to_string()) {
            findings.push(TestFinding {
                kind: TestFindingKind::UnincludedMod,
                file: candidate.to_string(),
                name: stem.to_string(),
                reason: format!("`mod {stem};` not found in parent `{parent_path}`"),
            });
        }
    }

    findings
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// True when the file path has a `tests` directory component.
fn is_integration_test_path(file: &str) -> bool {
    file.split('/').any(|c| c == "tests")
}

/// True when `line` is `#[cfg(not(test))]`.
fn is_cfg_not_test(line: &str) -> bool {
    // Match the canonical form; normalise whitespace minimally.
    let normalised: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    normalised == "#[cfg(not(test))]"
}

/// True when `line` is `#[test]` (exact, no arguments).
fn is_test_attr(line: &str) -> bool {
    let normalised: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    normalised == "#[test]"
}

/// True when any of `attrs` is `#[ignore]` or `#[ignore = "..."]`.
fn has_ignore_attr(attrs: &[&str]) -> bool {
    attrs.iter().any(|a| {
        let n: String = a.chars().filter(|c| !c.is_whitespace()).collect();
        n == "#[ignore]" || n.starts_with("#[ignore=") || n.starts_with("#[ignore(")
    })
}

/// True when any of `attrs` is a platform-specific cfg gate
/// (`target_os`, `target_arch`, `target_env`, `target_family`).
fn has_platform_cfg_attr(attrs: &[&str]) -> bool {
    const PLATFORM_KEYS: &[&str] = &[
        "target_os",
        "target_arch",
        "target_env",
        "target_family",
        "target_vendor",
    ];
    attrs.iter().any(|a| {
        let n: String = a.chars().filter(|c| !c.is_whitespace()).collect();
        if !n.starts_with("#[cfg(") {
            return false;
        }
        PLATFORM_KEYS.iter().any(|k| n.contains(k))
    })
}

/// Advance `i` past a `#[cfg(not(test))]`-gated block and return names of any
/// `#[test]` functions found inside it.
///
/// Uses a simple brace-counting heuristic: we look for the opening `{` on the
/// same or next line after the attribute, then track depth until it reaches 0.
fn collect_tests_in_cfg_block(lines: &[&str], i: &mut usize) -> Vec<String> {
    let start = *i;
    // Skip the attribute line itself.
    *i += 1;

    // Find the opening `{`.
    let mut depth: i32 = 0;
    let mut inside = false;
    let mut tests = Vec::new();
    let mut pending_test = false;

    while *i < lines.len() {
        let line = lines[*i].trim();

        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    inside = true;
                }
                '}' => {
                    depth -= 1;
                }
                _ => {}
            }
        }

        if inside {
            // Check for `#[test]` inside the block.
            if is_test_attr(line) {
                pending_test = true;
            } else if pending_test {
                if let Some(name) = extract_fn_name(line) {
                    tests.push(name);
                }
                pending_test = false;
            }
        }

        *i += 1;

        if inside && depth == 0 {
            break;
        }
    }

    // If we never found the opening brace (malformed source), just skip
    // the attribute line (already done).
    let _ = start;
    tests
}

/// Extract the function name from a `fn <name>(...)` line.
fn extract_fn_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    // Handle optional visibility (`pub`, `pub(crate)`, etc.) and `async fn`.
    let after_fn = trimmed
        .find("fn ")
        .map(|pos| trimmed[pos + 3..].trim_start())?;
    let name_end = after_fn
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(after_fn.len());
    let name = &after_fn[..name_end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Collect all bare `mod <name>;` declarations from source text.
fn collect_mod_declarations(src: &str) -> Vec<String> {
    let mut mods = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim();
        // Match `mod <name>;` optionally preceded by visibility.
        // We intentionally ignore `mod <name> { ... }` inline modules.
        let rest = trimmed
            .strip_prefix("pub(crate) mod ")
            .or_else(|| trimmed.strip_prefix("pub(super) mod "))
            .or_else(|| trimmed.strip_prefix("pub mod "))
            .or_else(|| trimmed.strip_prefix("mod "));
        if let Some(rest) = rest {
            if let Some(name) = rest.strip_suffix(';') {
                let name = name.trim();
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    mods.push(name.to_string());
                }
            }
        }
    }
    mods
}

/// Extract the file stem (filename without `.rs` extension) from a path.
fn file_stem(path: &str) -> &str {
    let filename = path.rsplit('/').next().unwrap_or(path);
    filename.strip_suffix(".rs").unwrap_or(filename)
}

/// Return true when the candidate stem matches the parent itself or a stem
/// that should never be treated as a missing module (`mod.rs`, `lib`, `main`).
fn is_self_or_ignored_stem(stem: &str, parent_path: &str) -> bool {
    // If the candidate *is* the parent file.
    let parent_stem = file_stem(parent_path);
    if stem == parent_stem {
        return true;
    }
    // Conventional "entry point" stems that are not `mod`-declared.
    matches!(stem, "mod" | "lib" | "main")
}

// ---------------------------------------------------------------------------
// I/O layer — repo scan
// ---------------------------------------------------------------------------

/// Walk the repository and collect all TestFindings by:
/// 1. Running `scan_source` on every `.rs` file.
/// 2. For each directory that contains a `lib.rs`, `main.rs`, or `mod.rs`,
///    running `find_unincluded_mods` against the sibling `.rs` files.
///
/// Fails closed (`Err`) when the walk is INCOMPLETE — a directory or `.rs` file
/// that exists but cannot be read. An incomplete scan must NOT masquerade as
/// "no skipped tests" (empty findings → GREEN); the caller maps the `Err` to a
/// dedicated undetermined exit rather than exit 0. Twin of
/// [`crate::decision::list_files`] (CA-specguard-001).
pub fn scan_repo(repo_root: &std::path::Path) -> Result<Vec<TestFinding>> {
    let rs_files = collect_rs_files(repo_root)?;
    let mut findings = Vec::new();

    // Pass 1: per-file scan (ignore/cfg-gate/integration-test).
    for path in &rs_files {
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        // A `.rs` file that was ENUMERATED but is now unreadable is
        // cannot-determine, not "no findings from this file" → fail closed.
        let src = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read source file '{}'", path.display()))?;
        findings.extend(scan_source(&src, &rel));
    }

    // Pass 2: mod-include graph per directory.
    let mod_graph = collect_mod_graph(repo_root, &rs_files)?;
    for (parent_path, parent_src, candidates) in &mod_graph {
        let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
        findings.extend(find_unincluded_mods(parent_src, parent_path, &refs));
    }

    Ok(findings)
}

type ModGraphEntry = (String, String, Vec<String>);

/// For each directory under `repo_root` that has a `lib.rs`, `main.rs`, or
/// `mod.rs`, return `(parent_rel_path, parent_src, [sibling_rel_paths])`.
pub fn collect_mod_graph(
    repo_root: &std::path::Path,
    rs_files: &[std::path::PathBuf],
) -> Result<Vec<ModGraphEntry>> {
    use std::collections::HashMap;

    // Group .rs files by directory.
    let mut by_dir: HashMap<std::path::PathBuf, Vec<std::path::PathBuf>> = HashMap::new();
    for f in rs_files {
        if let Some(dir) = f.parent() {
            by_dir.entry(dir.to_path_buf()).or_default().push(f.clone());
        }
    }

    let mut result = Vec::new();
    for (dir, files) in &by_dir {
        // Find the parent file (lib.rs, main.rs, or mod.rs).
        let parent = ["lib.rs", "main.rs", "mod.rs"].iter().find_map(|name| {
            let p = dir.join(name);
            if files.contains(&p) {
                Some(p)
            } else {
                None
            }
        });
        let Some(parent_path) = parent else { continue };
        // A parent module (lib.rs/main.rs/mod.rs) that EXISTS but is unreadable
        // means the mod-include graph for this directory cannot be built —
        // cannot-determine, not "no unincluded mods here" → fail closed rather
        // than silently drop this directory's UnincludedMod findings.
        let parent_src = std::fs::read_to_string(&parent_path)
            .with_context(|| format!("cannot read parent module '{}'", parent_path.display()))?;
        let parent_rel = parent_path
            .strip_prefix(repo_root)
            .unwrap_or(&parent_path)
            .to_string_lossy()
            .replace('\\', "/");

        // Candidate siblings = all .rs files in the same dir except the parent.
        let candidates: Vec<String> = files
            .iter()
            .filter(|f| *f != &parent_path)
            .map(|f| {
                f.strip_prefix(repo_root)
                    .unwrap_or(f)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();

        if !candidates.is_empty() {
            result.push((parent_rel, parent_src, candidates));
        }
    }
    Ok(result)
}

/// Recursively collect all `.rs` files under `root`, skipping `target/` and
/// hidden dirs. Fails closed on any unreadable (but existing) directory or
/// listing entry — see [`collect_rs_files_inner`].
fn collect_rs_files(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    collect_rs_files_inner(root, &mut out)?;
    Ok(out)
}

fn collect_rs_files_inner(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // A MISSING dir legitimately contributes no files (e.g. a TOCTOU race
        // where a subdir vanished mid-walk). A dir that EXISTS but is unreadable
        // (PermissionDenied, IO error) is INCOMPLETE input, not "no files" →
        // fail closed so the audit can't pass GREEN on a subtree it never read.
        // Twin of decision.rs::list_files (CA-specguard-001); the old
        // `let Ok(..) else { return }` collapsed every error to "empty here".
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(anyhow::Error::new(err)
                .context(format!("cannot read directory '{}'", dir.display())));
        }
    };
    for entry in entries {
        // A per-entry error means the listing is INCOMPLETE. The old
        // `entries.flatten()` dropped it, so a dir with one unreadable entry
        // could silently under-report its `.rs` files. Fail closed instead.
        let entry =
            entry.with_context(|| format!("unreadable entry in directory '{}'", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip target/, .git/, and hidden dirs.
        if name_str == "target" || name_str.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_rs_files_inner(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // (a) #[ignore] detection
    // ------------------------------------------------------------------

    #[test]
    fn detects_ignored_test() {
        let src = r#"
#[test]
#[ignore]
fn skipped_by_ignore() {}
"#;
        let findings = scan_source(src, "src/lib.rs");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, TestFindingKind::Ignored);
        assert_eq!(findings[0].name, "skipped_by_ignore");
    }

    #[test]
    fn detects_ignore_with_message() {
        let src = r#"
#[test]
#[ignore = "not yet implemented"]
fn todo_test() {}
"#;
        let findings = scan_source(src, "src/lib.rs");
        assert!(findings.iter().any(|f| f.kind == TestFindingKind::Ignored));
    }

    #[test]
    fn normal_test_not_flagged() {
        let src = r#"
#[test]
fn normal_test() {
    assert_eq!(1 + 1, 2);
}
"#;
        let findings = scan_source(src, "src/lib.rs");
        assert!(
            findings.is_empty(),
            "normal test should not be flagged: {findings:?}"
        );
    }

    // ------------------------------------------------------------------
    // (b) cfg-gate detection
    // ------------------------------------------------------------------

    #[test]
    fn detects_test_inside_cfg_not_test_block() {
        let src = r#"
#[cfg(not(test))]
mod hidden {
    #[test]
    fn unreachable_test() {}
}
"#;
        let findings = scan_source(src, "src/lib.rs");
        let cfg_gated: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == TestFindingKind::CfgGated)
            .collect();
        assert!(
            !cfg_gated.is_empty(),
            "expected CfgGated finding: {findings:?}"
        );
        assert_eq!(cfg_gated[0].name, "unreachable_test");
    }

    #[test]
    fn detects_platform_cfg_gated_test() {
        let src = r#"
#[test]
#[cfg(target_os = "windows")]
fn windows_only_test() {}
"#;
        let findings = scan_source(src, "src/lib.rs");
        let cfg_gated: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == TestFindingKind::CfgGated)
            .collect();
        assert!(
            !cfg_gated.is_empty(),
            "expected CfgGated finding: {findings:?}"
        );
        assert_eq!(cfg_gated[0].name, "windows_only_test");
    }

    #[test]
    fn cfg_test_module_not_flagged() {
        // A normal `#[cfg(test)]` block — tests inside are REACHABLE.
        let src = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn reachable() {}
}
"#;
        let findings = scan_source(src, "src/lib.rs");
        let cfg_gated: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == TestFindingKind::CfgGated)
            .collect();
        assert!(
            cfg_gated.is_empty(),
            "cfg(test) should not be flagged as CfgGated: {findings:?}"
        );
    }

    // ------------------------------------------------------------------
    // (c) find_unincluded_mods
    // ------------------------------------------------------------------

    #[test]
    fn find_unincluded_mods_detects_missing_mod() {
        let parent_src = r#"
mod config;
mod parse;
"#;
        let candidates = &["src/config.rs", "src/parse.rs", "src/testutil.rs"];
        let findings = find_unincluded_mods(parent_src, "src/lib.rs", candidates);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, TestFindingKind::UnincludedMod);
        assert_eq!(findings[0].name, "testutil");
    }

    #[test]
    fn find_unincluded_mods_all_declared() {
        let parent_src = r#"
mod alpha;
mod beta;
"#;
        let candidates = &["src/alpha.rs", "src/beta.rs"];
        let findings = find_unincluded_mods(parent_src, "src/lib.rs", candidates);
        assert!(findings.is_empty(), "all mods declared: {findings:?}");
    }

    #[test]
    fn find_unincluded_mods_ignores_lib_and_main() {
        let parent_src = "";
        // lib.rs and main.rs are never `mod`-declared.
        let candidates = &["src/lib.rs", "src/main.rs"];
        let findings = find_unincluded_mods(parent_src, "src/lib.rs", candidates);
        assert!(
            findings.is_empty(),
            "lib/main should be ignored: {findings:?}"
        );
    }

    #[test]
    fn find_unincluded_mods_pub_mod_counts() {
        let parent_src = "pub mod utils;\n";
        let candidates = &["src/utils.rs", "src/secret.rs"];
        let findings = find_unincluded_mods(parent_src, "src/lib.rs", candidates);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].name, "secret");
    }

    // ------------------------------------------------------------------
    // (d) scan_source — integration test path detection
    // ------------------------------------------------------------------

    #[test]
    fn detects_integration_test_file() {
        let src = "// integration test\n";
        let findings = scan_source(src, "tests/my_integration.rs");
        let it: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == TestFindingKind::IntegrationTest)
            .collect();
        assert!(
            !it.is_empty(),
            "expected IntegrationTest finding: {findings:?}"
        );
    }

    #[test]
    fn non_tests_dir_not_flagged_as_integration() {
        let src = "// normal source\n";
        let findings = scan_source(src, "src/lib.rs");
        let it: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == TestFindingKind::IntegrationTest)
            .collect();
        assert!(
            it.is_empty(),
            "src/lib.rs should not be flagged: {findings:?}"
        );
    }

    #[test]
    fn integration_test_file_with_ignored_test() {
        let src = r#"
#[test]
#[ignore]
fn ignored_integration() {}
"#;
        let findings = scan_source(src, "tests/ignored_suite.rs");
        assert!(findings
            .iter()
            .any(|f| f.kind == TestFindingKind::IntegrationTest));
        assert!(findings.iter().any(|f| f.kind == TestFindingKind::Ignored));
    }

    // ------------------------------------------------------------------
    // scan_repo: I/O integration
    // ------------------------------------------------------------------

    #[test]
    fn scan_repo_detects_unincluded_mod() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // lib.rs doesn't declare `mod test_foo;`
        fs::write(src_dir.join("lib.rs"), "pub fn foo() {}\n").unwrap();
        fs::write(src_dir.join("test_foo.rs"), "#[test]\nfn it_works() {}\n").unwrap();

        let findings = scan_repo(tmp.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f.kind == TestFindingKind::UnincludedMod && f.name == "test_foo"),
            "expected UnincludedMod for test_foo.rs: {findings:?}"
        );
    }

    #[test]
    fn finding_kind_as_str_covers_all_variants() {
        assert_eq!(TestFindingKind::Ignored.as_str(), "ignored");
        assert_eq!(TestFindingKind::CfgGated.as_str(), "cfg_gated");
        assert_eq!(TestFindingKind::UnincludedMod.as_str(), "unincluded_mod");
        assert_eq!(
            TestFindingKind::IntegrationTest.as_str(),
            "integration_test"
        );
    }

    #[test]
    fn scan_repo_no_findings_when_all_declared() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        fs::write(src_dir.join("lib.rs"), "mod helper;\npub fn foo() {}\n").unwrap();
        fs::write(src_dir.join("helper.rs"), "pub fn bar() {}\n").unwrap();

        let findings = scan_repo(tmp.path()).unwrap();
        let unincluded: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == TestFindingKind::UnincludedMod)
            .collect();
        assert!(
            unincluded.is_empty(),
            "should be no unincluded mods: {unincluded:?}"
        );
    }

    /// A repo root that does not exist is legitimately "no `.rs` files" (the
    /// NotFound arm), not a cannot-determine — it must return `Ok(empty)`, not
    /// fail closed. Mirrors decision.rs::list_files_ok_and_empty_when_dir_absent.
    #[test]
    fn scan_repo_ok_and_empty_when_root_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let findings = scan_repo(&missing).expect("absent root is not cannot-determine");
        assert!(findings.is_empty(), "absent root: {findings:?}");
    }

    /// CA-specguard-001 (testaudit twin): a subdirectory that EXISTS but is
    /// unreadable must fail CLOSED (`Err`), not be silently dropped from the
    /// walk. If it were dropped, a `#[ignore]`d / cfg-gated / unincluded-`mod`
    /// test living in that subtree would never be scanned and the audit would
    /// report GREEN precisely when it could not determine the answer. Unix-only
    /// (chmod-based unreadability), matching decision.rs's convention.
    #[cfg(unix)]
    #[test]
    fn scan_repo_fails_closed_on_unreadable_subdir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "pub fn foo() {}\n").unwrap();
        // A subtree the walk can see but not read.
        let hidden_sub = src_dir.join("experimental");
        std::fs::create_dir_all(&hidden_sub).unwrap();
        std::fs::write(
            hidden_sub.join("skipped.rs"),
            "#[test]\n#[ignore]\nfn s() {}\n",
        )
        .unwrap();

        let mut perms = std::fs::metadata(&hidden_sub).unwrap().permissions();
        perms.set_mode(0o000); // unreadable/unsearchable
        std::fs::set_permissions(&hidden_sub, perms).unwrap();

        let got = scan_repo(tmp.path());

        // Restore perms so tempdir cleanup can remove it (before asserting).
        let mut restore = std::fs::metadata(&hidden_sub).unwrap().permissions();
        restore.set_mode(0o700);
        let _ = std::fs::set_permissions(&hidden_sub, restore);

        assert!(
            got.is_err(),
            "an unreadable subdir must fail closed, got {got:?}"
        );
    }
}
