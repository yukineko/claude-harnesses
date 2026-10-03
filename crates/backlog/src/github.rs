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
///
/// Compares the parsed **host** exactly against `github.com` (case-insensitive)
/// rather than doing a substring match, so a lookalike host
/// (`github.company.internal`, `github.com.evil.example`) or a `github.com`
/// occurrence elsewhere in the URL (query string, path) is not mistaken for
/// the real remote.
pub fn is_github_remote(remote_url: &str) -> bool {
    let lower = remote_url.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }

    // SCP-like syntax has no "://": `[user@]host:path`, e.g. `git@github.com:owner/repo.git`.
    if !lower.contains("://") {
        return match lower.split_once('@') {
            Some((_, after_at)) => match after_at.split_once(':') {
                Some((host, _path)) => host == "github.com",
                None => false,
            },
            None => false,
        };
    }

    // URL form: `scheme://[user@]host[:port][/path...]`.
    let Some((_, after_scheme)) = lower.split_once("://") else {
        return false;
    };
    let host_and_rest = after_scheme.split('/').next().unwrap_or("");
    let host_part = match host_and_rest.rsplit_once('@') {
        Some((_, host)) => host,
        None => host_and_rest,
    };
    let host = host_part.split(':').next().unwrap_or("");
    host == "github.com"
}

/// Upper bound on an issue BODY handed to `gh`, measured in **bytes**
/// (`str::len`), not characters.
///
/// GitHub rejects a body over 65536 *characters*. Bounding bytes is the
/// conservative side of that limit: for UTF-8, bytes >= characters, so a body
/// under this many bytes is always under the same number of characters. The
/// headroom below 65536 covers the truncation marker.
///
/// This is the sibling of `store::CLOSE_COMMENT_MAX` and holds the same value
/// for the same reason. The two live in different modules because they bind at
/// different points: a closing comment is *rendered* from a task by
/// `store::build_close_comment`, so its bound belongs with the rendering, while
/// a body is passed through from the task's notes by several call sites, so its
/// bound belongs here — at the argv chokepoint every one of them must go
/// through. Before 0.3.24 only the close side was bounded, which is the
/// asymmetry filed as backlog `751b2d1e`: an oversized create does not degrade
/// to "filed without the content", it degrades to `gh issue create` exiting
/// non-zero and the task never being mirrored at all, retried identically on
/// every subsequent sync.
pub const ISSUE_BODY_MAX: usize = 60_000;

/// Appended when a body did not fit. Says so in the body itself: dropping
/// content silently would make the issue a worse record than an obviously
/// truncated one, because nothing downstream could tell a short task from a
/// trimmed one (CLAUDE.md §4 — never make an error invisible).
const ISSUE_BODY_TRUNCATED: &str = "\n\n*(body truncated here to stay under GitHub's \
     limit — the full text lives in this repo's `.backlog` store, keyed by this task's id.)*\n";

/// Clamp `body` to [`ISSUE_BODY_MAX`] bytes, marking it when anything was cut.
/// Pure; never panics.
///
/// Keeps the HEAD and drops the TAIL: the start of a task's notes is its
/// problem statement, while the tail is the accumulated investigation, so a
/// reader who gets the head can still find the rest by task id.
///
/// The cut lands on a **char boundary**, not a byte offset. These notes are
/// routinely Japanese, so slicing by byte would panic inside a function whose
/// whole job is to stop an oversized body from failing the invocation.
pub fn bound_issue_body(body: &str) -> String {
    if body.len() <= ISSUE_BODY_MAX {
        return body.to_string();
    }
    let room = ISSUE_BODY_MAX.saturating_sub(ISSUE_BODY_TRUNCATED.len());
    let cut = body
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= room)
        .last()
        .unwrap_or(0);
    format!("{}{ISSUE_BODY_TRUNCATED}", &body[..cut])
}

/// Deterministically format the argv for `gh issue create`. Pure, no side effects.
///
/// Shape: `["issue","create","--title",<title>,"--body",<body>]`, with the body
/// clamped by [`bound_issue_body`].
pub fn build_issue_create_args(title: &str, body: &str) -> Vec<String> {
    vec![
        "issue".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
        "--body".to_string(),
        bound_issue_body(body),
    ]
}

/// Deterministically format the argv for `gh issue edit`, re-pushing a body
/// whose local source has moved on. Pure, no side effects.
///
/// Shape: `["issue","edit",<number>,"--body",<body>]`, with the body clamped by
/// [`bound_issue_body`].
///
/// `--body` REPLACES the body rather than appending a comment. That is the
/// one-way-mirror contract applied consistently: the local store is
/// authoritative, so a body edited on GitHub is not an input and is not
/// preserved. Appending comments instead was considered and rejected — a
/// long-lived task accumulates many note revisions, and a growing comment chain
/// makes the current content harder to find, not easier.
pub fn build_issue_edit_args(number: u64, body: &str) -> Vec<String> {
    vec![
        "issue".to_string(),
        "edit".to_string(),
        number.to_string(),
        "--body".to_string(),
        bound_issue_body(body),
    ]
}

/// The outcome of the `gh issue edit --body` step.
///
/// Shaped like [`CloseOutcome`] rather than [`IssueOutcome`]: there is no
/// "degraded, carry on" arm the caller may record as success. The staleness
/// stamp is written ONLY on [`EditOutcome::Edited`], so a failed push leaves the
/// task in exactly the shape that put it in the plan and the next
/// `sync --only body` picks it up again.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "outcome")]
pub enum EditOutcome {
    /// gh executed `issue edit` and reported success.
    Edited,
    /// The body was NOT updated. The caller must not stamp the task.
    NotEdited { reason: String },
}

/// Decide the outcome of a `gh issue edit --body` invocation from its injected
/// result. Never spawns a process itself; never panics.
///
/// Every non-success path — non-GitHub remote, gh absent (`None`), gh ran and
/// failed (`Some((false, _))`) — resolves to [`EditOutcome::NotEdited`], the
/// restrictive side: the body is assumed still stale until GitHub says
/// otherwise.
pub fn decide_issue_edit<R: Fn(&[&str]) -> Option<(bool, String)>>(
    remote_url: &str,
    number: u64,
    body: &str,
    run: R,
) -> EditOutcome {
    if !is_github_remote(remote_url) {
        return EditOutcome::NotEdited {
            reason: "remote is not github.com; nothing to update".to_string(),
        };
    }
    let args = build_issue_edit_args(number, body);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&argv) {
        None => EditOutcome::NotEdited {
            reason: "gh CLI not found; issue body left stale".to_string(),
        },
        Some((false, output)) => EditOutcome::NotEdited {
            reason: format!("gh issue edit failed: {}", output.trim()),
        },
        Some((true, _)) => EditOutcome::Edited,
    }
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

/// Why an issue is being closed. Maps to `gh issue close --reason`, whose
/// accepted values are exactly `completed` and `"not planned"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The task was finished (`backlog done`).
    Completed,
    /// The task was abandoned (`status = "cancelled"`).
    NotPlanned,
}

impl CloseReason {
    /// The literal `gh issue close --reason` value.
    pub fn as_gh_reason(self) -> &'static str {
        match self {
            CloseReason::Completed => "completed",
            CloseReason::NotPlanned => "not planned",
        }
    }
}

/// Deterministically format the argv for `gh issue close`. Pure, no side effects.
///
/// Shape: `["issue","close","<number>","--reason",<reason>]`, followed by
/// `["--comment",<body>]` when `comment` carries a non-blank body.
///
/// The comment is what makes a close record **what** was finished instead of
/// only that something was: a bare close leaves an issue whose entire history
/// is "opened" and "closed", and the store that holds the actual resolution is
/// not visible from GitHub. `gh issue close --comment` is the documented way to
/// leave that record in the same call (verified against gh 2.102.0:
/// `-c, --comment string   Leave a closing comment`), so recording the content
/// costs no extra round trip and cannot half-succeed independently of the
/// close.
///
/// A blank body is **dropped rather than passed through**: `--comment ""` posts
/// an empty comment, which reads on the issue as "someone had something to say
/// and it was lost" — strictly worse than no comment. Callers with content build
/// it via `store::build_close_comment`, which never renders blank.
pub fn build_issue_close_args(
    number: u64,
    reason: CloseReason,
    comment: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "issue".to_string(),
        "close".to_string(),
        number.to_string(),
        "--reason".to_string(),
        reason.as_gh_reason().to_string(),
    ];
    if let Some(body) = comment.filter(|b| !b.trim().is_empty()) {
        args.push("--comment".to_string());
        args.push(body.to_string());
    }
    args
}

/// The outcome of the `gh issue close` step.
///
/// Unlike [`IssueOutcome`], this type has **no "degraded, carry on" arm that
/// the caller may treat as success**. `gh issue create` can degrade to
/// local-only because the task itself still exists and is still queued —
/// nothing is lost. A close is the opposite: the local store has already moved
/// on to `done`, so a close that silently does not happen leaves an open issue
/// that nothing will ever revisit. That asymmetry is the whole reason this is a
/// separate type, and why [`CloseOutcome::NotClosed`] carries its reason
/// instead of collapsing into a bool (CLAUDE.md §3: "could not close" is not
/// "closed").
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "outcome")]
pub enum CloseOutcome {
    /// gh executed `issue close` and reported success.
    Closed,
    /// The issue was NOT closed. The caller must not record a close: it leaves
    /// `issue_closed_at` unset so the next `backlog sync` retries.
    NotClosed { reason: String },
}

/// Decide the outcome of a `gh issue close` invocation from its injected
/// result. Never spawns a process itself; never panics.
///
/// Every non-success path — non-GitHub remote, gh absent (`None`), gh ran and
/// failed (`Some((false, _))`) — resolves to [`CloseOutcome::NotClosed`], i.e.
/// the restrictive side: the mirror is assumed still open until GitHub says
/// otherwise. Only an affirmative `Some((true, _))` yields `Closed`.
///
/// `comment` is the content recorded on the issue as it closes (see
/// [`build_issue_close_args`]). It rides the SAME invocation as the close on
/// purpose: a separate `gh issue comment` call could succeed while the close
/// failed, or the reverse, and there is no third state here to represent
/// "closed but the record of why is missing".
pub fn decide_issue_close<R: Fn(&[&str]) -> Option<(bool, String)>>(
    remote_url: &str,
    number: u64,
    reason: CloseReason,
    comment: Option<&str>,
    run: R,
) -> CloseOutcome {
    if !is_github_remote(remote_url) {
        return CloseOutcome::NotClosed {
            reason: "remote is not github.com; nothing to close".to_string(),
        };
    }
    let args = build_issue_close_args(number, reason, comment);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&argv) {
        None => CloseOutcome::NotClosed {
            reason: "gh CLI not found; issue left open".to_string(),
        },
        Some((false, output)) => CloseOutcome::NotClosed {
            reason: format!("gh issue close failed: {}", output.trim()),
        },
        Some((true, _)) => CloseOutcome::Closed,
    }
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

    /// A lookalike host (github.com as a prefix/suffix/substring of a
    /// DIFFERENT host, or appearing only in the path/query) must not be
    /// mistaken for the real github.com — this is an exact-host comparison,
    /// not a substring match.
    #[test]
    fn is_github_remote_rejects_lookalike_hosts() {
        assert!(!is_github_remote(
            "https://github.company.internal/owner/repo.git"
        ));
        assert!(!is_github_remote(
            "https://github.com.evil.example/owner/repo.git"
        ));
        assert!(!is_github_remote(
            "https://evil.example/redirect?to=github.com"
        ));
        assert!(!is_github_remote(
            "git@github.company.internal:owner/repo.git"
        ));
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

    //
    // Available in scope via `use super::*;`: decide_issue_close,
    // build_issue_close_args, CloseReason, CloseOutcome.
    // Mirrors the style of the existing decide_issue_create tests (panicking
    // runner to prove non-invocation; argv asserted inside the runner).
    // =============================================================================

    // --- gh issue close ------------------------------------------------------

    /// The `--reason` values are fixed by gh's CLI contract: exactly
    /// `completed` and `not planned` (with the space). Dies if either literal
    /// is changed — gh rejects anything else, so a typo here turns every
    /// close into a NotClosed at runtime with no local test failure.
    #[test]
    fn close_reason_maps_to_the_exact_gh_literals() {
        assert_eq!(CloseReason::Completed.as_gh_reason(), "completed");
        assert_eq!(CloseReason::NotPlanned.as_gh_reason(), "not planned");
    }

    /// Exact argv shape for both reasons, including the number's position.
    /// Dies if the argv order changes, if `--reason` is dropped, or if the
    /// issue number stops being rendered as its own positional argument.
    #[test]
    fn build_issue_close_args_is_deterministic() {
        assert_eq!(
            build_issue_close_args(42, CloseReason::Completed, None),
            vec![
                "issue".to_string(),
                "close".to_string(),
                "42".to_string(),
                "--reason".to_string(),
                "completed".to_string(),
            ]
        );
        assert_eq!(
            build_issue_close_args(7, CloseReason::NotPlanned, None),
            vec![
                "issue".to_string(),
                "close".to_string(),
                "7".to_string(),
                "--reason".to_string(),
                "not planned".to_string(),
            ]
        );
    }

    /// A supplied body is appended as `--comment <body>` AFTER the reason, and
    /// is passed through byte-for-byte: the body is multi-line prose, so any
    /// trimming/escaping here would silently alter what gets recorded on the
    /// issue. Dies if the flag is dropped, renamed, or if the body is mangled.
    #[test]
    fn build_issue_close_args_appends_a_supplied_comment_verbatim() {
        let body = "line one\n\nline two with  spaces and \"quotes\" and 日本語";
        assert_eq!(
            build_issue_close_args(42, CloseReason::Completed, Some(body)),
            vec![
                "issue".to_string(),
                "close".to_string(),
                "42".to_string(),
                "--reason".to_string(),
                "completed".to_string(),
                "--comment".to_string(),
                body.to_string(),
            ]
        );
    }

    /// A blank body yields NO `--comment` at all, rather than `--comment ""`.
    /// An empty comment posted on the issue would read as a lost message; the
    /// absence of a comment at least does not assert anything false.
    #[test]
    fn build_issue_close_args_drops_a_blank_comment_instead_of_passing_it() {
        let bare = build_issue_close_args(42, CloseReason::Completed, None);
        for blank in ["", "   ", "\n\t "] {
            assert_eq!(
                build_issue_close_args(42, CloseReason::Completed, Some(blank)),
                bare,
                "a blank comment ({blank:?}) must not reach the argv"
            );
        }
    }

    /// A non-GitHub remote must resolve to NotClosed WITHOUT invoking `run`
    /// at all — we never shell out to gh against someone else's forge.
    /// Dies if the `is_github_remote` guard is removed (the runner panics),
    /// and dies if `decide_issue_close` returns `Closed` unconditionally.
    #[test]
    fn close_non_github_remote_is_not_closed_without_running_gh() {
        let outcome = decide_issue_close(
            "https://gitlab.com/owner/repo.git",
            42,
            CloseReason::Completed,
            None,
            |_argv| panic!("run must not be called for a non-github remote"),
        );
        match outcome {
            CloseOutcome::NotClosed { reason } => assert!(
                reason.contains("not github.com"),
                "reason must explain the remote is not github.com: {reason:?}"
            ),
            other => panic!("non-github remote must not be Closed, got {other:?}"),
        }
    }

    /// Same invariant, proved by RECORDING invocation rather than panicking —
    /// a runner that swallowed the panic (or a future `catch_unwind`) cannot
    /// hide a spurious gh call from this one.
    /// Dies if the `is_github_remote` guard is removed.
    #[test]
    fn close_non_github_remote_records_zero_runner_calls() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let outcome = decide_issue_close(
            "git@gitlab.com:owner/repo.git",
            42,
            CloseReason::NotPlanned,
            None,
            |_argv| {
                calls.set(calls.get() + 1);
                Some((true, String::new()))
            },
        );
        assert_eq!(
            calls.get(),
            0,
            "gh must not be invoked for a non-github remote"
        );
        assert!(
            matches!(outcome, CloseOutcome::NotClosed { .. }),
            "non-github remote must not be Closed, got {outcome:?}"
        );
    }

    /// gh absent (runner returns None) ⇒ NotClosed, i.e. the issue is assumed
    /// STILL OPEN so the next sync retries (CLAUDE.md §3: "could not close"
    /// is not "closed").
    /// Dies if the `None` arm is changed to `Closed`, and dies if
    /// `decide_issue_close` returns `Closed` unconditionally.
    #[test]
    fn close_gh_absent_is_not_closed() {
        let outcome = decide_issue_close(
            "https://github.com/owner/repo.git",
            42,
            CloseReason::Completed,
            None,
            |_argv| None,
        );
        match outcome {
            CloseOutcome::NotClosed { reason } => assert!(
                reason.contains("not found"),
                "reason must explain gh is absent: {reason:?}"
            ),
            other => panic!("gh-absent must not be Closed, got {other:?}"),
        }
    }

    /// gh ran and exited non-zero ⇒ NotClosed, and the reason CARRIES gh's
    /// output so the failure is diagnosable rather than a bare bool.
    /// Dies if the exit status is ignored (the `Some((false, _))` arm folded
    /// into the success arm) — the exact fail-open this type exists to
    /// prevent — and dies if the output is dropped from the reason.
    #[test]
    fn close_gh_failure_is_not_closed_and_carries_the_output() {
        let outcome = decide_issue_close(
            "https://github.com/owner/repo.git",
            42,
            CloseReason::Completed,
            None,
            |_argv| Some((false, "HTTP 403: resource not accessible\n".to_string())),
        );
        match outcome {
            CloseOutcome::NotClosed { reason } => {
                assert!(
                    reason.contains("gh issue close failed"),
                    "reason must say the close failed: {reason:?}"
                );
                assert!(
                    reason.contains("resource not accessible"),
                    "reason must carry gh's own output: {reason:?}"
                );
            }
            other => panic!("a non-zero gh exit must not be Closed, got {other:?}"),
        }
    }

    /// gh success ⇒ Closed, AND the argv handed to gh is exactly
    /// ["issue","close","42","--reason","completed"].
    /// Dies if `decide_issue_close` returns `NotClosed` unconditionally, and
    /// dies if the argv construction drifts (asserted inside the runner).
    #[test]
    fn close_success_yields_closed_with_completed_argv() {
        let outcome = decide_issue_close(
            "https://github.com/owner/repo.git",
            42,
            CloseReason::Completed,
            None,
            |argv| {
                assert_eq!(argv, ["issue", "close", "42", "--reason", "completed"]);
                Some((
                    true,
                    "https://github.com/owner/repo/issues/42\n".to_string(),
                ))
            },
        );
        assert_eq!(outcome, CloseOutcome::Closed);
    }

    /// The NotPlanned reason must reach gh as the literal `not planned` (two
    /// words, one argv element), and success still yields Closed.
    /// Dies if `decide_issue_close` returns `NotClosed` unconditionally, if
    /// the reason is hard-wired to `completed`, or if `not planned` is split
    /// into two argv elements / hyphenated.
    #[test]
    fn close_success_yields_closed_with_not_planned_argv() {
        let outcome = decide_issue_close(
            "git@github.com:owner/repo.git",
            7,
            CloseReason::NotPlanned,
            None,
            |argv| {
                assert_eq!(argv, ["issue", "close", "7", "--reason", "not planned"]);
                Some((true, String::new()))
            },
        );
        assert_eq!(outcome, CloseOutcome::Closed);
    }

    // =============================================================================
}
