//! GitHub-project-only helpers: remote detection + an injectable `gh issue
//! create` wrapper. Phase 1 is a one-way push (task -> GitHub issue); this
//! module supplies the pure decision logic only. Call-site integration
//! (deciding *when* to push, wiring into the driver) is a separate task.
//!
//! Design invariants preserved here (mirrors `condukt::pr`):
//!
//! 1. **Injectable runner**: the `gh` invocation is never spawned directly by
//!    these functions — the caller injects a `run(argv) -> Option<(bool,
//!    String)>` closure, so this module is unit-testable without `gh`
//!    installed and without a real GitHub-backed repo.
//! 2. **Pure argv/detection functions**: remote detection and argv
//!    construction take/return plain data, no IO, no panics.

/// Whether a git remote URL points at github.com (HTTPS or SSH form).
///
/// Pure string match: no network, no git invocation. Recognizes
/// `https://github.com/...`, `git@github.com:...`, and `ssh://git@github.com/...`.
pub fn is_github_remote(remote_url: &str) -> bool {
    let lower = remote_url.to_lowercase();
    lower.contains("github.com")
}

/// Deterministically format the argv for `gh issue create`. Pure, no side effects.
///
/// Shape: `["issue","create","--title",<title>,"--body",<body>]`.
pub fn build_issue_create_args(title: &str, body: &str) -> Vec<String> {
    vec![
        "issue".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
        "--body".to_string(),
        body.to_string(),
    ]
}

/// The fail-soft outcome of the `gh issue create` step.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "outcome")]
pub enum IssueOutcome {
    /// gh executed `issue create` and returned the new issue URL (from stdout).
    Created { url: String },
    /// Fail-soft: gh absent, not run against a GitHub remote, or the `gh
    /// issue create` invocation itself failed — the task is left local-only.
    DegradedLocalOnly { reason: String },
}

/// Decide the outcome of a `gh issue create` invocation from its injected
/// result. Never spawns a process itself; never panics.
///
/// - `remote_url` not pointing at github.com ⇒ `DegradedLocalOnly` (Phase 1 is
///   GitHub-project-only; non-GitHub remotes are left untouched).
/// - `run(argv)` returns `None` ⇒ gh absent ⇒ `DegradedLocalOnly`.
/// - `run(argv)` returns `Some((false, _))` ⇒ gh ran but failed (non-zero exit)
///   ⇒ `DegradedLocalOnly`.
/// - `run(argv)` returns `Some((true, stdout))` ⇒ `Created { url: stdout.trim() }`.
pub fn decide_issue_create<R: Fn(&[&str]) -> Option<(bool, String)>>(
    remote_url: &str,
    title: &str,
    body: &str,
    run: R,
) -> IssueOutcome {
    if !is_github_remote(remote_url) {
        return IssueOutcome::DegradedLocalOnly {
            reason: "remote is not github.com; left task as local-only".to_string(),
        };
    }
    let args = build_issue_create_args(title, body);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&argv) {
        None => IssueOutcome::DegradedLocalOnly {
            reason: "gh CLI not found; left task as local-only".to_string(),
        },
        Some((false, stdout)) => IssueOutcome::DegradedLocalOnly {
            reason: format!("gh issue create failed: {}", stdout.trim()),
        },
        Some((true, stdout)) => IssueOutcome::Created {
            url: stdout.trim().to_string(),
        },
    }
}

/// Extract the trailing issue number from a `gh issue create` URL
/// (e.g. `https://github.com/owner/repo/issues/42` -> `Some(42)`). Pure, no IO.
pub fn parse_issue_number(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_github_remote_recognizes_https_and_ssh_forms() {
        assert!(is_github_remote("https://github.com/owner/repo.git"));
        assert!(is_github_remote("git@github.com:owner/repo.git"));
        assert!(is_github_remote("ssh://git@github.com/owner/repo.git"));
    }

    #[test]
    fn is_github_remote_rejects_non_github_remotes() {
        assert!(!is_github_remote("https://gitlab.com/owner/repo.git"));
        assert!(!is_github_remote(
            "https://git.internal.example.com/owner/repo.git"
        ));
        assert!(!is_github_remote(""));
    }

    #[test]
    fn build_issue_create_args_is_deterministic() {
        let args = build_issue_create_args("Add feature X", "Some body text");
        assert_eq!(
            args,
            vec![
                "issue".to_string(),
                "create".to_string(),
                "--title".to_string(),
                "Add feature X".to_string(),
                "--body".to_string(),
                "Some body text".to_string(),
            ]
        );
    }

    /// Non-GitHub remote must degrade to local-only WITHOUT invoking `run` at
    /// all (Phase 1 is GitHub-project-only).
    #[test]
    fn non_github_remote_degrades_without_running_gh() {
        let outcome = decide_issue_create(
            "https://gitlab.com/owner/repo.git",
            "title",
            "body",
            |_argv| panic!("run must not be called for a non-github remote"),
        );
        match outcome {
            IssueOutcome::DegradedLocalOnly { reason } => {
                assert!(
                    reason.contains("not github.com"),
                    "reason must explain the remote is not github.com: {reason:?}"
                );
            }
            other => panic!("non-github remote must degrade to local-only, got {other:?}"),
        }
    }

    /// gh absent (runner always returns None) ⇒ DegradedLocalOnly.
    #[test]
    fn gh_absent_degrades_to_local_only() {
        let outcome = decide_issue_create(
            "https://github.com/owner/repo.git",
            "title",
            "body",
            |_argv| None,
        );
        match outcome {
            IssueOutcome::DegradedLocalOnly { reason } => {
                assert!(
                    reason.contains("not found"),
                    "reason must explain gh is absent: {reason:?}"
                );
            }
            other => panic!("gh-absent must degrade to local-only, got {other:?}"),
        }
    }

    /// gh present but `issue create` exits non-zero ⇒ DegradedLocalOnly.
    #[test]
    fn gh_failure_degrades_to_local_only() {
        let outcome = decide_issue_create(
            "https://github.com/owner/repo.git",
            "title",
            "body",
            |_argv| Some((false, "HTTP 422: validation failed".to_string())),
        );
        match outcome {
            IssueOutcome::DegradedLocalOnly { reason } => {
                assert!(
                    reason.contains("gh issue create failed"),
                    "reason must explain gh failed: {reason:?}"
                );
                assert!(
                    reason.contains("validation failed"),
                    "reason must carry gh's stdout: {reason:?}"
                );
            }
            other => panic!("gh failure must degrade to local-only, got {other:?}"),
        }
    }

    #[test]
    fn parse_issue_number_extracts_trailing_segment() {
        assert_eq!(
            parse_issue_number("https://github.com/owner/repo/issues/42"),
            Some(42)
        );
        assert_eq!(parse_issue_number("not-a-url"), None);
        assert_eq!(parse_issue_number(""), None);
    }

    /// gh present and `issue create` succeeds ⇒ Created{url} from stdout.
    #[test]
    fn gh_success_yields_created_with_url() {
        let outcome = decide_issue_create(
            "https://github.com/owner/repo.git",
            "Add feature X",
            "Some body text",
            |argv| {
                assert_eq!(
                    argv,
                    [
                        "issue",
                        "create",
                        "--title",
                        "Add feature X",
                        "--body",
                        "Some body text"
                    ]
                );
                Some((
                    true,
                    "https://github.com/owner/repo/issues/42\n".to_string(),
                ))
            },
        );
        assert_eq!(
            outcome,
            IssueOutcome::Created {
                url: "https://github.com/owner/repo/issues/42".to_string()
            }
        );
    }
}
