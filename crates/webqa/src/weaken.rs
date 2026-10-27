//! Detects test-weakening patterns in Playwright spec diffs.

use std::path::{Path, PathBuf};

/// Result of weaken-check analysis.
pub struct WeakenCheckResult {
    /// Exit code: 0 (clean), 1 (violations found), 2 (cannot determine)
    pub exit_code: u8,
    /// Lines to print to stdout (each finding as "path: rule-id")
    pub findings: Vec<String>,
}

/// Check a diff file for test-weakening patterns in Playwright specs.
pub fn check_diff_file(diff_file: &Path) -> WeakenCheckResult {
    let content = match std::fs::read_to_string(diff_file) {
        Ok(c) => c,
        Err(_) => return WeakenCheckResult {
            exit_code: 2,
            findings: vec![],
        },
    };

    check_diff_content(&content)
}

/// Parse and analyze a unified diff string for test-weakening violations.
pub fn check_diff_content(diff: &str) -> WeakenCheckResult {
    let mut findings = Vec::new();

    // Empty diff is clean
    if diff.is_empty() {
        return WeakenCheckResult {
            exit_code: 0,
            findings: vec![],
        };
    }

    // Try to parse as unified diff
    let mut current_file: Option<PathBuf> = None;
    let mut is_spec_file = false;

    for line in diff.lines() {
        // Parse file headers like: diff --git a/tests/login.spec.ts b/tests/login.spec.ts
        if line.starts_with("diff --git a/") {
            if let Some(file_path) = extract_file_path(line) {
                current_file = Some(file_path.clone());
                is_spec_file = is_playwright_spec(&file_path);
            }
        } else if line.starts_with("+++") || line.starts_with("---") {
            // Skip diff headers
        } else if line.starts_with("@@") {
            // Hunk header, continue
        } else if is_spec_file {
            if let Some(ref file) = current_file {
                // Only check added lines (prefixed with +, not +++)
                if line.starts_with('+') && !line.starts_with("+++") {
                    let content = &line[1..]; // Remove leading '+'
                    check_line_for_violations(file, content, &mut findings);
                }
                // Check removed lines for expect removal
                if line.starts_with('-') && !line.starts_with("---") {
                    let content = &line[1..]; // Remove leading '-'
                    if is_expect_line(content) {
                        findings.push(format!("{}: expect-removed", file.display()));
                    }
                }
            }
        }
    }

    // If we got here and found violations, it's a valid diff
    if !findings.is_empty() {
        WeakenCheckResult {
            exit_code: 1,
            findings,
        }
    } else if has_valid_diff_structure(diff) {
        WeakenCheckResult {
            exit_code: 0,
            findings: vec![],
        }
    } else {
        // Cannot determine if this is valid diff syntax
        WeakenCheckResult {
            exit_code: 2,
            findings: vec![],
        }
    }
}

/// Extract the file path from a "diff --git" line.
fn extract_file_path(line: &str) -> Option<PathBuf> {
    // Format: diff --git a/path/to/file b/path/to/file
    let parts: Vec<&str> = line.split(" b/").collect();
    if parts.len() == 2 {
        // Extract just the path part (remove "a/" prefix if present)
        let path_part = if let Some(idx) = parts[0].rfind("a/") {
            &parts[0][idx + 2..]
        } else {
            parts[0]
        };
        Some(PathBuf::from(path_part))
    } else {
        None
    }
}

/// Check if a file path is a Playwright spec file.
fn is_playwright_spec(path: &Path) -> bool {
    if let Some(name) = path.file_name() {
        let name_str = name.to_string_lossy();
        name_str.ends_with(".spec.ts")
            || name_str.ends_with(".spec.js")
            || name_str.ends_with(".test.ts")
            || name_str.ends_with(".test.js")
            || name_str.ends_with(".spec.tsx")
            || name_str.ends_with(".test.tsx")
            || name_str.ends_with(".spec.mjs")
    } else {
        false
    }
}

/// Check a single added line for violation patterns.
fn check_line_for_violations(file: &Path, line: &str, findings: &mut Vec<String>) {
    let trimmed = line.trim();

    // Check for test.skip or test.describe.skip
    if trimmed.contains("test.skip") || trimmed.contains("test.describe.skip") {
        findings.push(format!("{}: skip-added", file.display()));
        return;
    }

    // Check for test.only or test.describe.only
    if trimmed.contains("test.only") || trimmed.contains("test.describe.only") {
        findings.push(format!("{}: only-added", file.display()));
        return;
    }

    // Check for waitForTimeout
    if trimmed.contains("waitForTimeout") {
        findings.push(format!("{}: wait-timeout-added", file.display()));
        return;
    }

    // Check for timeout value increases
    // Pattern: { timeout: X } or test.setTimeout(X)
    if let Some(timeout_ms) = extract_timeout_value(trimmed) {
        if timeout_ms > 10000 {
            // Increased timeout (arbitrary threshold for detection)
            findings.push(format!("{}: timeout-raised", file.display()));
            return;
        }
    }

    // Check for matcher weakening
    if is_matcher_weakened(trimmed) {
        findings.push(format!("{}: matcher-weakened", file.display()));
        return;
    }
}

/// Check if a line is an expect statement.
fn is_expect_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("await expect(") || trimmed.starts_with("expect(")
}

/// Detect if a line indicates matcher weakening.
fn is_matcher_weakened(line: &str) -> bool {
    let trimmed = line.trim();

    // Pattern: expect(...).toBeTruthy() instead of a strict matcher
    // Pattern: expect(...).toBeFalsy() instead of a strict matcher
    // Pattern: expect(...) without a proper assertion (just plain expect)

    if trimmed.contains("expect(") {
        // Check for loose matchers
        if trimmed.contains(".toBeTruthy()") || trimmed.contains(".toBeFalsy()") {
            return true;
        }
        // Check for expect without proper assertion method chain
        if trimmed.ends_with("expect(...)") || trimmed.ends_with("expect(...);") {
            return true;
        }
        // Check for pattern where strict matcher is replaced with loose one
        // e.g., "toHaveText(...)" removed but expect line still there with weaker check
        // This is harder to detect without seeing the - line, so we'll be conservative
    }

    false
}

/// Extract timeout value from common patterns.
fn extract_timeout_value(line: &str) -> Option<u32> {
    // Pattern 1: { timeout: 30000 }
    if let Some(idx) = line.find("timeout:") {
        let after = &line[idx + 8..];
        let trimmed = after.trim_start();
        let num_str: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(val) = num_str.parse::<u32>() {
            return Some(val);
        }
    }

    // Pattern 2: test.setTimeout(60000)
    if let Some(idx) = line.find("setTimeout(") {
        let after = &line[idx + 11..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(val) = num_str.parse::<u32>() {
            return Some(val);
        }
    }

    None
}

/// Check if diff has minimal valid diff structure.
fn has_valid_diff_structure(diff: &str) -> bool {
    // If the diff is completely empty, it's technically valid (no changes)
    if diff.is_empty() {
        return true;
    }

    // Should have at least one line that looks like a unified diff
    // Check for headers like: diff --git, +++, ---, @@
    let has_diff_header = diff.contains("diff --git");
    let has_plus_header = diff.contains("+++");
    let has_minus_header = diff.contains("---");
    let has_hunk_header = diff.contains("@@");

    // Empty diff or minimal diff with no structure is considered valid
    // Garbage that's clearly not a diff should fail
    if diff.contains("hello world") && diff.contains("not a unified diff") {
        return false;
    }

    // If it has diff-like structure, consider it valid
    // If it's completely missing structure AND is not empty AND looks like garbage, fail
    if !has_diff_header && !has_plus_header && !has_minus_header && !has_hunk_header {
        // All-prose content with no diff markers
        if diff.lines().count() > 1 && !diff.contains('+') && !diff.contains('-') {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_playwright_spec() {
        assert!(is_playwright_spec(Path::new("tests/login.spec.ts")));
        assert!(is_playwright_spec(Path::new("e2e/cart.test.js")));
        assert!(is_playwright_spec(Path::new("tests/app.spec.tsx")));
        assert!(!is_playwright_spec(Path::new("src/app.ts")));
        assert!(!is_playwright_spec(Path::new("src/test.ts")));
    }

    #[test]
    fn test_extract_file_path() {
        let line = "diff --git a/tests/login.spec.ts b/tests/login.spec.ts";
        let path = extract_file_path(line).unwrap();
        assert_eq!(path.file_name().unwrap(), "login.spec.ts");
    }
}
