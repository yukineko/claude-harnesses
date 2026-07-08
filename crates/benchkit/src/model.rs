//! The typed SWE-bench Verified instance model.
//!
//! One [`Instance`] is one benchmark task: a repo pinned at a `base_commit`,
//! the gold `patch` that fixes it, a `test_patch` that adds/updates the tests,
//! and the two named test sets that grade a candidate — `FAIL_TO_PASS` (tests
//! that must flip red→green) and `PASS_TO_PASS` (tests that must stay green).
//!
//! The upstream SWE-bench JSONL uses upper-case keys for those two fields
//! (`FAIL_TO_PASS` / `PASS_TO_PASS`); we `#[serde(rename = ...)]` them onto
//! idiomatic snake_case Rust fields. Upstream sometimes encodes them as a
//! JSON-*string* holding an array; the vendored fixture uses a plain JSON list
//! of strings, which is what the model accepts here — the normalized shape the
//! `download` subcommand is responsible for producing.

use serde::{Deserialize, Serialize};

/// A single SWE-bench Verified task instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    /// Stable unique id, e.g. `"astropy__astropy-12907"`.
    pub instance_id: String,
    /// `owner/name` of the GitHub repository under test.
    pub repo: String,
    /// Commit the repo is checked out at before applying any patch.
    pub base_commit: String,
    /// The gold solution patch (unified diff) that resolves the issue.
    pub patch: String,
    /// The patch that introduces/updates the grading tests.
    pub test_patch: String,
    /// The natural-language issue / task description.
    pub problem_statement: String,
    /// Optional maintainer hints (may be empty).
    #[serde(default)]
    pub hints_text: String,
    /// Upstream creation timestamp (ISO-8601 string, kept verbatim).
    #[serde(default)]
    pub created_at: String,
    /// Project version label the instance targets (e.g. `"4.0"`).
    #[serde(default)]
    pub version: String,
    /// Tests that must go red→green for a candidate to count as resolved.
    #[serde(rename = "FAIL_TO_PASS", default)]
    pub fail_to_pass: Vec<String>,
    /// Tests that must stay green (guard against regressions).
    #[serde(rename = "PASS_TO_PASS", default)]
    pub pass_to_pass: Vec<String>,
    /// Commit whose environment (deps) the harness should set up against.
    #[serde(default)]
    pub environment_setup_commit: String,
}
