//! main-tree guard — machine enforcement of CLAUDE.md §8 ("work in a worktree;
//! do not share-edit the primary working tree").
//!
//! # What this decides
//!
//! One question, at commit time: *is this commit being made from the shared
//! primary working tree while another session is live?* If it is, the two
//! sessions are sharing a git index, which is the one collision git cannot
//! merge away — the measured incident (2026-07-23, this repo) had two sessions'
//! uncommitted changes in a single `git status`, on files that did not even
//! conflict.
//!
//! §8 was prose only, and §6 states that a norm resting on the implementer's
//! self-report is not enforced. This module is the enforcement: the decision is
//! a pure function over observations, so both branches are exercisable in unit
//! tests without spawning two real sessions.
//!
//! # Shape of the decision
//!
//! [`decide`] is pure. Everything that touches the world is an
//! [`Observations`] field, each a [`Determination`], so "could not observe"
//! arrives as its own answer rather than as a permissive default. The verdict
//! is [`Verdict`]: `Undetermined` blocks exactly like `Violation` (§3).
//!
//! The gate passes — and it must pass, a detector that always fires has
//! detected nothing — in four cases, each with its own test:
//!
//! 1. the commit is made from a **linked worktree** (not the primary tree);
//! 2. an **integration is in progress**. §8 explicitly permits merge and
//!    conflict resolution in the main tree; a gate that blocked integration
//!    would make the worktree workflow undischargeable, because the merges it
//!    prescribes could never land. This needs *two* sources, covering disjoint
//!    moments — getting it wrong the first time (on-disk markers only) blocked a
//!    real merge of this very branch:
//!    * the **invocation context** ([`declared_integration`]) for a merge that
//!      is succeeding, where git has written no marker at all;
//!    * the **on-disk markers** (`MERGE_HEAD`, `CHERRY_PICK_HEAD`,
//!      `REVERT_HEAD`, `rebase-merge/`, `rebase-apply/`) for an integration that
//!      stopped and is being finished by hand.
//!
//!    An integration pass is printed to stderr rather than being silent, so an
//!    exclusion that fires wrongly is visible in the output of the command it
//!    let through;
//! 3. **nothing is staged** — there is no shared-index content to certify;
//! 4. **no other live session** is reported by either liveness input.
//!
//! # The escape hatch is session-scoped, and it does not forge a pass
//!
//! `CONDUKT_MAINTREE_OVERRIDE=<reason>` unlocks a blocking verdict. It is an
//! environment variable, not a file: §5 forbids a project-root skip file
//! because it is consumed once and would wave through a *different* session's
//! legitimate gate. An empty or whitespace-only value does not unlock.
//!
//! An override does **not** turn the verdict into `Clean`. [`Decision`] keeps
//! the real `Violation`/`Undetermined` and records the reason alongside it; only
//! [`Decision::exit_code`] yields 0. So the record of what the gate found is not
//! rewritten by the bypass, and the caller prints the reason to stderr.
//!
//! # Known limit of the liveness input, stated rather than hidden
//!
//! `overwatch status --json` omits the `sessions` key when the roster is empty,
//! so an object without that key is read here as "zero live leases". That is
//! `overwatch`'s serializer contract (`skip_serializing_if = "Vec::is_empty"`),
//! but upstream of it `overwatch::aggregate::build` binds its lease load with
//! `if let Ok(..)` — an *unreadable* ledger also produces an empty roster, and
//! the JSON cannot tell the two apart. This module therefore cannot distinguish
//! "overwatch saw no leases" from "overwatch could not read its ledger". The
//! second liveness input (`backlog lock status`) is read independently and is
//! not subject to that flattening, but it does not fully cover the gap. This is
//! a real residual hole, not a safe degradation.

use std::path::{Path, PathBuf};
use std::process::Command;

use harness_core::boundary;
use harness_core::verdict::{Determination, Reason, Required, Verdict};

/// The environment variable that carries a human's stated reason for bypassing
/// this gate. Session-scoped by construction: it lives in one process's
/// environment and cannot be observed, or consumed, by another session.
pub const OVERRIDE_ENV: &str = "CONDUKT_MAINTREE_OVERRIDE";

/// The environment variable holding this session's id, used to tell "another
/// session is live" from "I am live".
pub const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// Which working tree the commit is being made from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeRole {
    /// The shared primary working tree — the one §8 says not to share-edit.
    Primary,
    /// A linked worktree (`git worktree add`), which has its own index.
    Linked,
}

/// An in-progress operation that §8 explicitly permits in the primary tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationKind {
    /// A merge is being concluded.
    Merge,
    /// A rebase is replaying commits.
    Rebase,
    /// A cherry-pick is being concluded.
    CherryPick,
    /// A revert is being concluded.
    Revert,
}

/// How the in-progress integration was observed.
///
/// Two sources are needed because they cover disjoint moments, which was the
/// defect in the first version of this gate: it had only the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationEvidence {
    /// A marker git wrote into the git dir (`MERGE_HEAD`, `CHERRY_PICK_HEAD`,
    /// `REVERT_HEAD`, `rebase-merge/`, `rebase-apply/`).
    ///
    /// Measured: git writes `MERGE_HEAD` only when a merge **stops** — a
    /// conflict, or `--no-commit`. A merge that succeeds never writes it, so
    /// this evidence exists precisely when the integration is *not* proceeding.
    /// It is what a hand-completed conflicted merge/cherry-pick/revert presents
    /// at `pre-commit` time.
    OnDisk(&'static str),
    /// The invocation context: git ran `.githooks/pre-merge-commit`, which is
    /// fired for a merge commit and nothing else, and that hook declared it via
    /// [`HOOK_ENV`] before `exec`ing `pre-commit`.
    ///
    /// This is the moment the on-disk markers do not cover — measured in
    /// `tests/main_tree_guard_merge.rs`: at `pre-merge-commit` time none of
    /// them exist. Honored only when git's own [`REFLOG_ACTION_ENV`]
    /// corroborates it; see [`declared_integration`] for why that is not a
    /// blanket bypass.
    HookInvocation,
}

/// An integration in progress, with what makes it observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Integration {
    /// Which operation.
    pub kind: IntegrationKind,
    /// How it was observed.
    pub evidence: IntegrationEvidence,
}

impl Integration {
    /// One line naming the exclusion, printed whenever it is what lets a commit
    /// through, so an integration pass is never silent.
    #[must_use]
    pub fn describe(&self) -> String {
        let how = match self.evidence {
            IntegrationEvidence::OnDisk(marker) => format!("{marker} on disk"),
            IntegrationEvidence::HookInvocation => {
                "declared by .githooks/pre-merge-commit and corroborated by GIT_REFLOG_ACTION"
                    .to_string()
            }
        };
        format!("{:?} in progress ({how})", self.kind)
    }
}

/// Set by `.githooks/pre-merge-commit` before it `exec`s `pre-commit`, naming
/// the hook git actually invoked.
pub const HOOK_ENV: &str = "CONDUKT_GIT_HOOK";

/// git's own record of the operation it is performing. Measured: git sets it to
/// `merge <ref>` while running `pre-merge-commit`, and leaves it **unset** for
/// an ordinary `git commit`.
pub const REFLOG_ACTION_ENV: &str = "GIT_REFLOG_ACTION";

/// Where a peer-session observation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerSource {
    /// An `overwatch` lease roster entry with a live (non-stale) lease.
    OverwatchLease,
    /// An active (non-stale) `backlog` run lock for this project.
    BacklogLock,
}

impl PeerSource {
    /// Short name used in the block message.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PeerSource::OverwatchLease => "overwatch lease",
            PeerSource::BacklogLock => "backlog lock",
        }
    }
}

/// A session other than this one that a liveness input reports as live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSession {
    /// The peer's session id as reported by the input.
    pub session_id: String,
    /// Which input reported it.
    pub source: PeerSource,
    /// Extra human detail (lease count, held-since, ...).
    pub detail: String,
}

/// Everything [`decide`] is allowed to look at. Each field is a
/// [`Determination`] so an input that could not be observed stays
/// distinguishable from one observed to be empty — and so tests can drive every
/// branch without a second live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observations {
    /// Primary tree or linked worktree.
    pub tree_role: Determination<TreeRole>,
    /// `Known(None)` = no integration in progress.
    pub integration: Determination<Option<Integration>>,
    /// Paths staged for this commit. `Known(vec![])` = nothing staged.
    pub staged_paths: Determination<Vec<String>>,
    /// Live sessions that are not this one. `Known(vec![])` = observed, none.
    pub peers: Determination<Vec<PeerSession>>,
}

/// The gate's answer: the verdict the checks actually reached, plus the bypass
/// reason if one was supplied *and* the verdict was blocking.
///
/// The verdict is not rewritten by a bypass — see the module docs.
#[must_use = "a gate decision must be acted on, never computed and dropped"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// What the checks found.
    pub verdict: Verdict,
    /// `Some(reason)` iff the verdict blocks and a non-blank override was set.
    pub override_reason: Option<String>,
    /// `Some(description)` iff an exclusion — rather than "nothing to object
    /// to" — is what made this pass. An integration pass is announced instead of
    /// being silent, so an exclusion that fired wrongly is visible in the output
    /// of the very command it let through.
    pub pass_note: Option<String>,
}

impl Decision {
    /// True iff this commit is refused.
    #[must_use]
    pub fn blocks(&self) -> bool {
        self.override_reason.is_none() && self.verdict.blocks()
    }

    /// Process exit status. `Clean` → 0. A blocking verdict → 1 for a
    /// `Violation` and 2 for an `Undetermined`, mirroring the convention the
    /// other scanners in `.githooks/pre-commit` already use so the hook can name
    /// "could not determine" as such — both still block. An acknowledged
    /// override → 0.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.override_reason.is_some() {
            return 0;
        }
        let block_code = match self.verdict {
            Verdict::Undetermined(_) => 2,
            _ => 1,
        };
        self.verdict.exit_code(block_code)
    }
}

/// Trim an override value, treating empty and whitespace-only as "not set".
/// A bypass must carry a human reason; a bare `CONDUKT_MAINTREE_OVERRIDE=` is
/// not one.
fn sanitize_override(raw: Option<&str>) -> Option<String> {
    let text = raw?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Whether the invocation context itself says an integration is under way.
///
/// `hook` is [`HOOK_ENV`] — set by `.githooks/pre-merge-commit`, the hook git
/// fires for a merge commit and for nothing else — and `reflog_action` is git's
/// own [`REFLOG_ACTION_ENV`].
///
/// **Why this is not a knob that waves an ordinary commit through, and what it
/// still is.** Both halves must agree, and the second is written by git, not by
/// the caller: measured (`tests/main_tree_guard_merge.rs`, and the same result
/// in a scratch repo), git sets `GIT_REFLOG_ACTION=merge <ref>` while running
/// `pre-merge-commit` and leaves it **unset** for an ordinary `git commit`. So
/// exporting the declaration by hand and running a normal commit does not
/// exclude anything — there is a test for exactly that. What this is *not* is
/// unforgeable: a caller who sets both variables deliberately can reach the
/// exclusion, exactly as `git commit --no-verify` can skip the hook entirely.
/// That is not the failure mode being defended against; the defended failure
/// mode is an exclusion firing *silently* or *by accident*, and against that
/// the answer is that every integration pass prints [`Integration::describe`]
/// to stderr, in the output of the very command it let through.
///
/// A declaration git does not corroborate yields `None` — "not an integration"
/// — rather than `Undetermined`. It is a real observation: nothing about the
/// invocation says a merge is happening. If a future git stopped setting
/// `GIT_REFLOG_ACTION`, real merges would fall through to the ordinary checks
/// and could block; that direction is the restricted one, and the operator
/// still has the documented override.
#[must_use]
pub fn declared_integration(
    hook: Option<&str>,
    reflog_action: Option<&str>,
) -> Option<Integration> {
    if hook?.trim() != "pre-merge-commit" {
        // Only the merge hook declares. An unknown hook name is not honored.
        return None;
    }
    let action = reflog_action?.trim();
    let verb = action.split_whitespace().next()?;
    // `git pull` that merges reports "pull"; `git merge` reports "merge".
    if verb != "merge" && verb != "pull" {
        return None;
    }
    Some(Integration {
        kind: IntegrationKind::Merge,
        evidence: IntegrationEvidence::HookInvocation,
    })
}

/// The pure decision. `override_raw` is the raw environment value (or `None`).
pub fn decide(obs: Observations, override_raw: Option<&str>) -> Decision {
    let Adjudication { verdict, pass_note } = adjudicate(obs);
    let override_reason = if verdict.blocks() {
        sanitize_override(override_raw)
    } else {
        // No bypass is recorded for a passing gate: nothing was bypassed.
        None
    };
    Decision {
        verdict,
        override_reason,
        pass_note,
    }
}

/// What [`adjudicate`] produces: the verdict, and — when an exclusion is what
/// made it clean — a description of that exclusion.
struct Adjudication {
    verdict: Verdict,
    pass_note: Option<String>,
}

impl Adjudication {
    fn verdict(verdict: Verdict) -> Self {
        Adjudication {
            verdict,
            pass_note: None,
        }
    }

    fn excluded(note: String) -> Self {
        Adjudication {
            verdict: Verdict::from_findings(vec![]),
            pass_note: Some(note),
        }
    }
}

/// The checks, in the order that lets each exclusion answer before the more
/// expensive question is asked.
fn adjudicate(obs: Observations) -> Adjudication {
    let Observations {
        tree_role,
        integration,
        staged_paths,
        peers,
    } = obs;

    let role = match tree_role.require() {
        Required::Determined(r) => r,
        Required::Blocked(v) => return Adjudication::verdict(v),
    };
    if role == TreeRole::Linked {
        // Exclusion 1: a linked worktree has its own index. This is the
        // behaviour §8 asks for, so it is the common green path and is not
        // announced.
        return Adjudication::verdict(Verdict::from_findings(vec![]));
    }

    let integration = match integration.require() {
        Required::Determined(i) => i,
        Required::Blocked(v) => return Adjudication::verdict(v),
    };
    if let Some(integration) = integration {
        // Exclusion 2: §8 permits integration in the primary tree, and a gate
        // that blocked it would make the worktree workflow undischargeable —
        // nothing a worktree produced could ever land. Announced, because this
        // is the exclusion with the widest reach.
        return Adjudication::excluded(integration.describe());
    }

    let staged = match staged_paths.require() {
        Required::Determined(s) => s,
        Required::Blocked(v) => return Adjudication::verdict(v),
    };
    if staged.is_empty() {
        // Exclusion 3: no shared-index content is being committed.
        return Adjudication::verdict(Verdict::from_findings(vec![]));
    }

    let peers = match peers.require() {
        Required::Determined(p) => p,
        Required::Blocked(v) => return Adjudication::verdict(v),
    };

    // Exclusion 4 is the empty-findings case below: observed, no peer.
    let findings: Vec<Reason> = peers
        .iter()
        .map(|p| {
            Reason::new(format!(
                "session {} is live ({}{}{}) while {} path(s) are staged in the PRIMARY working tree \
                 — two sessions sharing one index is the collision git cannot merge (CLAUDE.md §8). \
                 Commit from a worktree instead: `condukt worktree create`, or move the change \
                 with `git stash` + `git worktree add`.",
                p.session_id,
                p.source.label(),
                if p.detail.is_empty() { "" } else { ": " },
                p.detail,
                staged.len(),
            ))
        })
        .collect();
    Adjudication::verdict(Verdict::from_findings(findings))
}

// ---------------------------------------------------------------------------
// Observation gathering (the impure half).
// ---------------------------------------------------------------------------

/// Primary tree vs linked worktree, by comparing this worktree's git dir with
/// the repository's common git dir. They are the same directory only in the
/// primary tree.
///
/// Any git failure, or a path that cannot be canonicalised, is `Undetermined`:
/// not knowing which tree this is means not knowing whether the gate applies.
pub fn observe_tree_role(repo: &Path) -> Determination<TreeRole> {
    let own = match git_line(repo, &["rev-parse", "--absolute-git-dir"]).require() {
        Required::Determined(v) => v,
        Required::Blocked(v) => {
            return Determination::undetermined(format!(
                "cannot resolve --absolute-git-dir ({}); which working tree this is cannot be told",
                v.reason().map_or("no reason", Reason::as_str)
            ))
        }
    };
    let common = match git_line(repo, &["rev-parse", "--git-common-dir"]).require() {
        Required::Determined(v) => v,
        Required::Blocked(v) => {
            return Determination::undetermined(format!(
                "cannot resolve --git-common-dir ({}); which working tree this is cannot be told",
                v.reason().map_or("no reason", Reason::as_str)
            ))
        }
    };

    // --git-common-dir may be relative to the repo root; --absolute-git-dir is not.
    let common_path = {
        let p = PathBuf::from(&common);
        if p.is_absolute() {
            p
        } else {
            repo.join(p)
        }
    };
    let own_c = match std::fs::canonicalize(&own) {
        Ok(p) => p,
        Err(e) => {
            return Determination::undetermined(format!("cannot canonicalize git dir {own}: {e}"))
        }
    };
    let common_c = match std::fs::canonicalize(&common_path) {
        Ok(p) => p,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot canonicalize common git dir {}: {e}",
                common_path.display()
            ))
        }
    };
    Determination::known(if own_c == common_c {
        TreeRole::Primary
    } else {
        TreeRole::Linked
    })
}

/// Whether a merge / rebase / cherry-pick / revert is being concluded.
///
/// Two sources, because they cover disjoint moments:
///
/// 1. the invocation context ([`declared_integration`]) — the only thing
///    available while a *succeeding* merge is being committed, since git writes
///    no marker then;
/// 2. the on-disk markers — what a *stopped* integration (conflicted merge,
///    cherry-pick, revert, rebase) presents when a human finishes it with
///    `git commit`.
///
/// A missing marker is a real absence (`Known(None)`); a marker that exists but
/// cannot be read is `Undetermined` — `boundary::read_to_string` draws exactly
/// that line.
pub fn observe_integration(
    repo: &Path,
    hook: Option<&str>,
    reflog_action: Option<&str>,
) -> Determination<Option<Integration>> {
    if let Some(declared) = declared_integration(hook, reflog_action) {
        return Determination::known(Some(declared));
    }

    let git_dir = match git_line(repo, &["rev-parse", "--absolute-git-dir"]).require() {
        Required::Determined(v) => PathBuf::from(v),
        Required::Blocked(v) => {
            return Determination::undetermined(format!(
                "cannot resolve --absolute-git-dir ({}); an in-progress integration cannot be \
                 ruled out",
                v.reason().map_or("no reason", Reason::as_str)
            ))
        }
    };

    for (name, kind) in [
        ("MERGE_HEAD", IntegrationKind::Merge),
        ("CHERRY_PICK_HEAD", IntegrationKind::CherryPick),
        ("REVERT_HEAD", IntegrationKind::Revert),
    ] {
        match boundary::read_to_string(&git_dir.join(name)).require() {
            Required::Determined(Some(_)) => {
                return Determination::known(Some(Integration {
                    kind,
                    evidence: IntegrationEvidence::OnDisk(name),
                }))
            }
            Required::Determined(None) => {}
            Required::Blocked(_) => {
                return Determination::undetermined(format!(
                    "cannot read {name}; an in-progress integration cannot be ruled out"
                ))
            }
        }
    }
    for dir in ["rebase-merge", "rebase-apply"] {
        let marker: &'static str = dir;
        // A rebase state directory: presence is what matters. `read_dir_entries`
        // answers Known(empty) for "not there" and Undetermined for "there but
        // unreadable", but cannot distinguish "not there" from "there and
        // empty" — so consult the metadata directly and keep the same split.
        match std::fs::metadata(git_dir.join(dir)) {
            Ok(_) => {
                return Determination::known(Some(Integration {
                    kind: IntegrationKind::Rebase,
                    evidence: IntegrationEvidence::OnDisk(marker),
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "cannot stat {dir}: {e}; an in-progress rebase cannot be ruled out"
                ))
            }
        }
    }
    Determination::known(None)
}

/// Paths staged for the pending commit.
pub fn observe_staged(repo: &Path) -> Determination<Vec<String>> {
    match git_lines(repo, &["diff", "--cached", "--name-only"]).require() {
        Required::Determined(lines) => Determination::known(lines),
        Required::Blocked(v) => Determination::undetermined(format!(
            "`git diff --cached --name-only` did not run to a conclusion ({}); what this \
             commit contains is unknown",
            v.reason().map_or("no reason", Reason::as_str)
        )),
    }
}

/// Live sessions other than `self_session`, from both liveness inputs.
///
/// Either input failing to run to a conclusion — binary absent, non-zero exit,
/// output that does not parse — makes the whole observation `Undetermined`.
/// A missing `overwatch` is not "nobody is live" (§3).
///
/// `self_session` of `None` means this process cannot name itself, so no
/// reported session can be attributed to it and every live session counts as a
/// peer. That is the fail-closed direction.
pub fn observe_peers(repo: &Path, self_session: Option<&str>) -> Determination<Vec<PeerSession>> {
    let mut peers = Vec::new();

    let overwatch_json = match run_tool(repo, "overwatch", &["status", "--json"]).require() {
        Required::Determined(s) => s,
        Required::Blocked(Verdict::Undetermined(r)) => return Determination::Undetermined(r),
        Required::Blocked(_) => {
            return Determination::undetermined("overwatch status --json: no result")
        }
    };
    match parse_overwatch_sessions(&overwatch_json, self_session) {
        Determination::Known(mut found) => peers.append(&mut found),
        Determination::Undetermined(r) => return Determination::Undetermined(r),
    }

    let repo_arg = repo.display().to_string();
    let backlog_json =
        match run_tool(repo, "backlog", &["lock", "status", "--project", &repo_arg]).require() {
            Required::Determined(s) => s,
            Required::Blocked(Verdict::Undetermined(r)) => return Determination::Undetermined(r),
            Required::Blocked(_) => {
                return Determination::undetermined("backlog lock status: no result")
            }
        };
    match parse_backlog_lock(&backlog_json, self_session) {
        Determination::Known(Some(peer)) => peers.push(peer),
        Determination::Known(None) => {}
        Determination::Undetermined(r) => return Determination::Undetermined(r),
    }

    Determination::known(peers)
}

/// Peers from an `overwatch status --json` document.
///
/// An object with no `sessions` key is `overwatch`'s rendering of an empty
/// roster (`skip_serializing_if = "Vec::is_empty"`), so it is read as zero
/// sessions. The module docs record what that cannot distinguish. Anything that
/// is not a JSON object, or a `sessions` value with the wrong shape, is
/// `Undetermined`.
pub fn parse_overwatch_sessions(
    stdout: &str,
    self_session: Option<&str>,
) -> Determination<Vec<PeerSession>> {
    let doc: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => {
            return Determination::undetermined(format!(
                "overwatch status --json did not emit JSON ({e}); liveness is unknown"
            ))
        }
    };
    let Some(obj) = doc.as_object() else {
        return Determination::undetermined(
            "overwatch status --json emitted a non-object; liveness is unknown",
        );
    };
    let Some(sessions) = obj.get("sessions") else {
        return Determination::known(Vec::new());
    };
    let Some(arr) = sessions.as_array() else {
        return Determination::undetermined(
            "overwatch status --json: `sessions` is not an array; liveness is unknown",
        );
    };

    let mut peers = Vec::new();
    for entry in arr {
        let Some(id) = entry.get("session_id").and_then(serde_json::Value::as_str) else {
            return Determination::undetermined(
                "overwatch status --json: a session roster has no string `session_id`; \
                 liveness is unknown",
            );
        };
        let Some(live) = entry.get("live_count").and_then(serde_json::Value::as_u64) else {
            return Determination::undetermined(
                "overwatch status --json: a session roster has no numeric `live_count`; \
                 liveness is unknown",
            );
        };
        if live == 0 {
            continue;
        }
        if Some(id) == self_session {
            continue;
        }
        peers.push(PeerSession {
            session_id: id.to_string(),
            source: PeerSource::OverwatchLease,
            detail: format!("{live} live lease(s)"),
        });
    }
    Determination::known(peers)
}

/// A peer from `backlog lock status --project <repo>`.
///
/// The command prints the literal `none` when no lock file exists, and a JSON
/// object otherwise, with `"stale": true` added when the holder's heartbeat has
/// passed the TTL. A stale lock is not a live session — it is an explicit
/// observation that the holder stopped reporting — so it is not a peer;
/// blocking on it would leave the gate permanently red after any crash, which
/// is the "detector that always fires" failure.
pub fn parse_backlog_lock(
    stdout: &str,
    self_session: Option<&str>,
) -> Determination<Option<PeerSession>> {
    let text = stdout.trim();
    if text == "none" {
        return Determination::known(None);
    }
    let doc: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return Determination::undetermined(format!(
                "backlog lock status emitted neither `none` nor JSON ({e}); liveness is unknown"
            ))
        }
    };
    if doc
        .get("stale")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        // `stale` absent means "not stale": the field is only added on the
        // stale branch. Absence here is a real observation, not a default
        // standing in for an unread value.
        return Determination::known(None);
    }
    let Some(id) = doc.get("session_id").and_then(serde_json::Value::as_str) else {
        return Determination::undetermined(
            "backlog lock status: lock JSON has no string `session_id`; liveness is unknown",
        );
    };
    if Some(id) == self_session {
        return Determination::known(None);
    }
    let pid = doc
        .get("pid")
        .and_then(serde_json::Value::as_i64)
        .map(|p| format!("pid {p}"))
        .unwrap_or_else(|| "pid unknown".to_string());
    Determination::known(Some(PeerSession {
        session_id: id.to_string(),
        source: PeerSource::BacklogLock,
        detail: pid,
    }))
}

/// Run a tool and take its stdout only when it exited 0.
fn run_tool(repo: &Path, program: &str, args: &[&str]) -> Determination<String> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(repo);
    match boundary::run(&mut cmd).require() {
        Required::Determined(out) => out.stdout_on_success(),
        Required::Blocked(Verdict::Undetermined(r)) => Determination::Undetermined(r),
        Required::Blocked(_) => Determination::undetermined(format!("{program}: no result")),
    }
}

/// A single trimmed line of git output.
fn git_line(repo: &Path, args: &[&str]) -> Determination<String> {
    git_lines(repo, args).map(|lines| lines.into_iter().next().unwrap_or_default())
}

/// git stdout split into non-empty trimmed lines.
fn git_lines(repo: &Path, args: &[&str]) -> Determination<Vec<String>> {
    run_tool(repo, "git", args).map(|s| {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    })
}

/// Gather every observation from the real world.
pub fn observe(repo: &Path) -> Observations {
    let self_session = std::env::var(SESSION_ENV).ok();
    let hook = std::env::var(HOOK_ENV).ok();
    let reflog_action = std::env::var(REFLOG_ACTION_ENV).ok();
    let tree_role = observe_tree_role(repo);

    // Only the primary-tree path needs the rest; but the struct is built whole
    // so `decide` stays the single place that knows the ordering. The peer
    // probe (two subprocesses) is skipped when the tree role already excludes,
    // because it is the expensive one.
    let excluded = matches!(tree_role, Determination::Known(TreeRole::Linked));
    Observations {
        tree_role,
        integration: if excluded {
            Determination::known(None)
        } else {
            observe_integration(repo, hook.as_deref(), reflog_action.as_deref())
        },
        staged_paths: if excluded {
            Determination::known(Vec::new())
        } else {
            observe_staged(repo)
        },
        peers: if excluded {
            Determination::known(Vec::new())
        } else {
            observe_peers(repo, self_session.as_deref())
        },
    }
}

/// The working tree root containing `cwd`, so every probe below runs from the
/// same place regardless of which subdirectory `git commit` was typed in.
fn toplevel(cwd: &Path) -> Determination<PathBuf> {
    git_line(cwd, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// CLI entry point for `condukt guard main-tree`. Prints the outcome and
/// returns the process exit status.
pub fn run_guard(cwd: &Path, json: bool) -> i32 {
    let override_raw = std::env::var(OVERRIDE_ENV).ok();
    let root = match toplevel(cwd).require() {
        Required::Determined(p) => p,
        Required::Blocked(verdict) => {
            // Not inside a work tree, or git could not answer: which tree this
            // is cannot be told, which blocks (the override still applies).
            let decision = Decision {
                override_reason: if verdict.blocks() {
                    sanitize_override(override_raw.as_deref())
                } else {
                    None
                },
                verdict,
                pass_note: None,
            };
            eprintln!(
                "main-tree-guard: UNDETERMINED — cannot resolve the working tree root; \
                 this blocks."
            );
            return decision.exit_code();
        }
    };
    let repo = root.as_path();
    let obs = observe(repo);
    let decision = decide(obs, override_raw.as_deref());

    if json {
        let payload = serde_json::json!({
            "gate": "main-tree",
            "verdict": match decision.verdict {
                Verdict::Clean(_) => "clean",
                Verdict::Violation(_) => "violation",
                Verdict::Undetermined(_) => "undetermined",
            },
            "reason": decision.verdict.reason().map(Reason::as_str),
            "override_reason": decision.override_reason,
            "pass_note": decision.pass_note,
            "blocks": decision.blocks(),
            "exit_code": decision.exit_code(),
        });
        println!("{payload}");
        return decision.exit_code();
    }

    // An exclusion that let the commit through says so, in the output of the
    // command it let through. A pass that nobody can see is a pass nobody can
    // audit.
    if let Some(note) = &decision.pass_note {
        eprintln!("main-tree-guard: allowed — {note} (CLAUDE.md §8 permits this in the main tree)");
    }

    match (&decision.verdict, &decision.override_reason) {
        (Verdict::Clean(_), _) => {}
        (v, Some(reason)) => {
            eprintln!(
                "main-tree-guard: BYPASSED by {OVERRIDE_ENV}: {reason}\n  the gate's finding \
                 stands and is not rewritten by the bypass:\n  {}",
                v.reason().map_or("", Reason::as_str)
            );
        }
        (Verdict::Violation(r), None) => {
            eprintln!("main-tree-guard: BLOCKED — {r}");
        }
        (Verdict::Undetermined(r), None) => {
            eprintln!(
                "main-tree-guard: UNDETERMINED — {r}\n  \"could not check\" is not \"clean\", \
                 so this blocks."
            );
        }
    }
    decision.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str) -> PeerSession {
        PeerSession {
            session_id: id.to_string(),
            source: PeerSource::OverwatchLease,
            detail: "1 live lease(s)".to_string(),
        }
    }

    /// Primary tree, nothing in progress, something staged, a peer live.
    fn blocking() -> Observations {
        Observations {
            tree_role: Determination::known(TreeRole::Primary),
            integration: Determination::known(None),
            staged_paths: Determination::known(vec!["crates/condukt/src/main.rs".to_string()]),
            peers: Determination::known(vec![peer("peer-1")]),
        }
    }

    #[test]
    fn primary_tree_with_a_live_peer_and_staged_files_blocks() {
        let d = decide(blocking(), None);
        assert!(d.blocks(), "expected a block, got {d:?}");
        assert!(matches!(d.verdict, Verdict::Violation(_)));
        assert_eq!(d.exit_code(), 1);
        assert!(d
            .verdict
            .reason()
            .expect("a violation carries a reason")
            .as_str()
            .contains("peer-1"));
    }

    #[test]
    fn exclusion_committing_from_a_linked_worktree_passes() {
        let obs = Observations {
            tree_role: Determination::known(TreeRole::Linked),
            ..blocking()
        };
        let d = decide(obs, None);
        assert!(!d.blocks(), "a worktree commit must pass, got {d:?}");
        assert_eq!(d.exit_code(), 0);
    }

    #[test]
    fn exclusion_no_other_live_session_passes() {
        let obs = Observations {
            peers: Determination::known(vec![]),
            ..blocking()
        };
        let d = decide(obs, None);
        assert!(!d.blocks(), "a solo session must pass, got {d:?}");
        assert_eq!(d.exit_code(), 0);
    }

    #[test]
    fn exclusion_integration_commit_passes_for_every_kind() {
        for kind in [
            IntegrationKind::Merge,
            IntegrationKind::Rebase,
            IntegrationKind::CherryPick,
            IntegrationKind::Revert,
        ] {
            let obs = Observations {
                integration: Determination::known(Some(Integration {
                    kind,
                    evidence: IntegrationEvidence::OnDisk("MARKER"),
                })),
                ..blocking()
            };
            let d = decide(obs, None);
            assert!(
                !d.blocks(),
                "{kind:?} is integration, which §8 permits in the main tree: {d:?}"
            );
            assert_eq!(d.exit_code(), 0);
            assert!(
                d.pass_note.is_some(),
                "an integration pass must be announced, not silent"
            );
        }
    }

    #[test]
    fn an_integration_pass_names_its_evidence() {
        for (evidence, needle) in [
            (
                IntegrationEvidence::OnDisk("MERGE_HEAD"),
                "MERGE_HEAD on disk",
            ),
            (IntegrationEvidence::HookInvocation, "pre-merge-commit"),
        ] {
            let obs = Observations {
                integration: Determination::known(Some(Integration {
                    kind: IntegrationKind::Merge,
                    evidence,
                })),
                ..blocking()
            };
            let note = decide(obs, None).pass_note.expect("announced");
            assert!(note.contains(needle), "{note:?} should name {needle:?}");
        }
    }

    #[test]
    fn only_the_ordinary_pass_is_silent() {
        // A linked-worktree pass and a nothing-staged pass are the normal
        // course of events and carry no note; only an exclusion does.
        let quiet = Observations {
            tree_role: Determination::known(TreeRole::Linked),
            ..blocking()
        };
        assert_eq!(decide(quiet, None).pass_note, None);
        let quiet = Observations {
            peers: Determination::known(vec![]),
            ..blocking()
        };
        assert_eq!(decide(quiet, None).pass_note, None);
    }

    #[test]
    fn the_hook_declaration_alone_does_not_make_an_integration() {
        // git leaves GIT_REFLOG_ACTION unset for an ordinary commit (measured),
        // so a declaration with nothing corroborating it excludes nothing.
        assert_eq!(declared_integration(Some("pre-merge-commit"), None), None);
        assert_eq!(
            declared_integration(Some("pre-merge-commit"), Some("")),
            None
        );
        assert_eq!(
            declared_integration(Some("pre-merge-commit"), Some("commit")),
            None
        );
        assert_eq!(
            declared_integration(Some("pre-merge-commit"), Some("commit (amend)")),
            None
        );
    }

    #[test]
    fn a_corroborated_merge_declaration_is_an_integration() {
        for action in ["merge side", "merge", "pull --no-rebase origin main"] {
            assert_eq!(
                declared_integration(Some("pre-merge-commit"), Some(action)),
                Some(Integration {
                    kind: IntegrationKind::Merge,
                    evidence: IntegrationEvidence::HookInvocation,
                }),
                "{action:?} should corroborate the declaration"
            );
        }
    }

    #[test]
    fn an_unknown_or_absent_hook_declaration_is_not_honored() {
        assert_eq!(declared_integration(None, Some("merge side")), None);
        assert_eq!(
            declared_integration(Some("pre-commit"), Some("merge side")),
            None,
            "only the merge hook declares; pre-commit runs for ordinary commits too"
        );
        assert_eq!(
            declared_integration(Some("post-commit"), Some("merge side")),
            None
        );
    }

    #[test]
    fn exclusion_nothing_staged_passes() {
        let obs = Observations {
            staged_paths: Determination::known(vec![]),
            ..blocking()
        };
        assert!(!decide(obs, None).blocks());
    }

    #[test]
    fn every_undetermined_input_blocks_with_exit_2() {
        let cases: Vec<(&str, Observations)> = vec![
            (
                "tree role",
                Observations {
                    tree_role: Determination::undetermined("git failed"),
                    ..blocking()
                },
            ),
            (
                "integration",
                Observations {
                    integration: Determination::undetermined("cannot stat MERGE_HEAD"),
                    ..blocking()
                },
            ),
            (
                "staged",
                Observations {
                    staged_paths: Determination::undetermined("git diff failed"),
                    ..blocking()
                },
            ),
            (
                "peers",
                Observations {
                    peers: Determination::undetermined("overwatch is not installed"),
                    ..blocking()
                },
            ),
        ];
        for (label, obs) in cases {
            let d = decide(obs, None);
            assert!(d.blocks(), "undetermined {label} must block, got {d:?}");
            assert!(matches!(d.verdict, Verdict::Undetermined(_)));
            assert_eq!(d.exit_code(), 2, "undetermined {label} exits 2");
        }
    }

    #[test]
    fn override_needs_a_non_blank_reason() {
        for blank in ["", "   ", "\t\n "] {
            let d = decide(blocking(), Some(blank));
            assert!(d.blocks(), "{blank:?} must not unlock the gate");
            assert_eq!(d.override_reason, None);
        }
    }

    #[test]
    fn override_with_a_reason_exits_zero_without_forging_a_clean() {
        let d = decide(blocking(), Some("  integrating a hotfix by hand  "));
        assert!(!d.blocks());
        assert_eq!(d.exit_code(), 0);
        assert_eq!(
            d.override_reason.as_deref(),
            Some("integrating a hotfix by hand")
        );
        assert!(
            matches!(d.verdict, Verdict::Violation(_)),
            "the finding must survive the bypass, not be rewritten to Clean"
        );
    }

    #[test]
    fn override_also_unlocks_an_undetermined_verdict() {
        let obs = Observations {
            peers: Determination::undetermined("overwatch absent"),
            ..blocking()
        };
        let d = decide(obs, Some("overwatch is being rebuilt"));
        assert!(!d.blocks());
        assert!(matches!(d.verdict, Verdict::Undetermined(_)));
    }

    #[test]
    fn override_is_not_recorded_when_the_gate_already_passes() {
        let obs = Observations {
            tree_role: Determination::known(TreeRole::Linked),
            ..blocking()
        };
        let d = decide(obs, Some("not needed"));
        assert_eq!(d.override_reason, None, "nothing was bypassed");
    }

    #[test]
    fn overwatch_absent_sessions_key_is_zero_sessions() {
        let d = parse_overwatch_sessions(r#"{"backlog":{"pending":1}}"#, None);
        assert_eq!(d, Determination::Known(vec![]));
    }

    #[test]
    fn overwatch_live_roster_yields_a_peer_and_self_is_filtered() {
        let json = r#"{"sessions":[
            {"session_id":"me","leases":[],"live_count":1},
            {"session_id":"other","leases":[],"live_count":2},
            {"session_id":"idle","leases":[],"live_count":0}
        ]}"#;
        let d = parse_overwatch_sessions(json, Some("me"));
        let peers = match d {
            Determination::Known(p) => p,
            other => panic!("expected Known, got {other:?}"),
        };
        assert_eq!(peers.len(), 1, "self and idle sessions are not peers");
        assert_eq!(peers[0].session_id, "other");
    }

    #[test]
    fn overwatch_unparseable_or_misshapen_output_is_undetermined() {
        for bad in [
            "",
            "not json",
            "[]",
            r#"{"sessions":"nope"}"#,
            r#"{"sessions":[{"live_count":1}]}"#,
            r#"{"sessions":[{"session_id":"x"}]}"#,
        ] {
            assert!(
                matches!(
                    parse_overwatch_sessions(bad, None),
                    Determination::Undetermined(_)
                ),
                "{bad:?} must be undetermined, never an empty roster"
            );
        }
    }

    #[test]
    fn backlog_lock_none_and_stale_are_not_peers() {
        assert_eq!(
            parse_backlog_lock("none\n", None),
            Determination::Known(None)
        );
        let stale = r#"{"session_id":"ghost","pid":1,"stale":true}"#;
        assert_eq!(parse_backlog_lock(stale, None), Determination::Known(None));
    }

    #[test]
    fn backlog_lock_active_other_session_is_a_peer_and_self_is_not() {
        let held = r#"{"session_id":"other","pid":4242,"project":"/repo"}"#;
        match parse_backlog_lock(held, Some("me")) {
            Determination::Known(Some(p)) => {
                assert_eq!(p.session_id, "other");
                assert_eq!(p.source, PeerSource::BacklogLock);
                assert_eq!(p.detail, "pid 4242");
            }
            other => panic!("expected a peer, got {other:?}"),
        }
        assert_eq!(
            parse_backlog_lock(held, Some("other")),
            Determination::Known(None),
            "my own lock is not a peer"
        );
    }

    #[test]
    fn backlog_lock_garbage_is_undetermined() {
        for bad in ["", "held", r#"{"pid":1}"#] {
            assert!(
                matches!(
                    parse_backlog_lock(bad, None),
                    Determination::Undetermined(_)
                ),
                "{bad:?} must be undetermined"
            );
        }
    }

    #[test]
    fn a_missing_self_session_id_makes_every_live_session_a_peer() {
        // Fail-closed: unable to name itself, the process cannot claim a
        // reported session as its own.
        let json = r#"{"sessions":[{"session_id":"me","leases":[],"live_count":1}]}"#;
        match parse_overwatch_sessions(json, None) {
            Determination::Known(p) => assert_eq!(p.len(), 1),
            other => panic!("expected Known, got {other:?}"),
        }
    }
}
