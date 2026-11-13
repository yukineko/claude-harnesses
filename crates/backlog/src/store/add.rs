//! The production `backlog add` body ([`add_finding`]) and its test wrapper.
//! A child module of `store` so it can use the store's private lock / load /
//! guard helpers; split out of `store.rs` to keep that file under its
//! file-growth cap.

use super::*;

/// Like [`add_with_weight`], but additionally mirrors the new task to GitHub
/// as an issue (Phase 1: `backlog add` -> `gh issue create`), recording
/// `issue_number`/`issue_url` on the persisted `Task` when it succeeds.
///
/// `remote_url` is the project's git remote URL (or `""`/anything
/// non-github.com to skip the mirror entirely — see
/// [`crate::github::is_github_remote`]). `run` is the SAME kind of injectable
/// command runner `crate::github::decide_issue_create` expects
/// (`Fn(&[&str]) -> Option<(bool, String)>`); the real `gh` spawn belongs at
/// the IO boundary in `main.rs`, never here, so this function stays fully
/// unit-testable with a fake closure and never spawns a process itself.
///
/// This wrapper passes a `land` that always succeeds (no git), so it drives
/// the same [`add_finding`] body; a `gh` that is absent or fails still leaves
/// a successful local add with the issue fields `None`. The ordering against
/// `land` is pinned by `land_order_tests` below. All guards (bare-label
/// ambiguity rejection, duplicate-content guard) run inside the locked write.
///
/// Locking differs from `add_with_weight` (backlog bd6d81df) and this is the
/// production `add` path, so this is the behaviour that matters: an `Ok(id)`
/// here is what makes `main.rs` print `added: {id}` and exit 0, so it must mean
/// the record is in the store. Two independent belts enforce that:
///   1. FAIL CLOSED on mutual exclusion: the critical section runs under
///      [`with_tasks_lock_aware`] and returns `Err` when the lock was not
///      acquired within the bounded budget, instead of performing the
///      read-modify-write unprotected. An unprotected RMW is what let a
///      concurrent degraded writer save a stale snapshot over a just-added
///      task while its adder had already reported success.
///   2. VERIFY THE WRITE: after `save`, the store is RELOADED FROM DISK and the
///      new id must be present ([`confirm_added_id_is_on_disk`]). This catches
///      a clobber (or a save that did not land) that happened anyway, and means
///      success is never reported from the in-memory `Vec` alone.
///
/// Both errors name what actually happened so the caller can retry; neither is
/// collapsed into a bool.
///
/// Test-only since the close-evidence change: it files the task straight into
/// the workable queue (`pending`) with a FIXTURE `reproduced` repro (no test
/// is run), so `next` hands it out as it would a real reproduced finding. The production entry point is
/// [`add_finding`], which takes the [`Intake`] (status + observed repro)
/// decided by `main.rs`; this wrapper exists so the locking/verify-the-write
/// tests in `store::tests` keep driving that SAME body.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn add_with_weight_and_github_push<R: Fn(&[&str]) -> Option<(bool, String)>>(
    path: &Path,
    title: &str,
    project: &str,
    tags: Vec<String>,
    notes: &str,
    weight: f64,
    force: bool,
    now: i64,
    remote_url: &str,
    run: R,
) -> Result<String> {
    add_finding(
        path,
        title,
        project,
        tags,
        notes,
        weight,
        force,
        now,
        remote_url,
        run,
        |_: &Path, _: &str| Ok(()),
        Intake {
            status: STATUS_PENDING,
            repro: Some(fixture_reproduced_repro(now)),
        },
    )
}

/// Where a new finding lands and what was observed about it.
///
/// Only a `reproduced` repro outcome may be filed at all (user rulings
/// 2026-10-04: 「testがないなら起票してはならない」). Every other outcome is NOT a
/// task and nothing is written for it (user ruling 2026-10-15). [`Intake::reproduced`] is the only production
/// constructor and returns `None` for anything that did not reproduce.
pub struct Intake {
    pub status: Status,
    pub repro: Option<Repro>,
}

impl Intake {
    /// `Some(pending)` exactly when `repro` observed the problem; `None`
    /// otherwise (not reproduced, or undetermined) — such a finding is not
    /// filed.
    pub fn reproduced(repro: Repro) -> Option<Self> {
        (repro.outcome == crate::task::REPRO_REPRODUCED).then_some(Intake {
            status: STATUS_PENDING,
            repro: Some(repro),
        })
    }
}

/// The production `add` (see [`add_with_weight_and_github_push`]'s docs for
/// the two bd6d81df belts, which live here). The task is written with
/// `intake.status` and `intake.repro`.
///
/// Order (user ruling: 「backlog addの時点で台帳も更新される。mainにいく、
/// issueができる」), applied only when `remote_url` is a GitHub remote:
///   1. the row is written to `path` (no issue fields) under the tasks lock;
///   2. `land(path, id)` must put that row on the primary branch (production:
///      `crate::integrate::land_row_on_main` — merge into the branch checked
///      out in the primary tree, then push). `Err` here returns `Err`: `run`
///      is NEVER called, so no issue exists, and the row stays only in `path`;
///   3. only then `decide_issue_create` runs `gh issue create` via `run`;
///   4. on `Created`, `issue_number`/`issue_url` are recorded in `path`
///      (`updated_at` untouched) and `land` runs again so the primary branch's
///      row carries the number. `Err` there returns `Err` naming the issue.
///
/// A non-GitHub remote never calls `land` or `run` (no issue is made, so the
/// two cannot diverge). A degraded `gh` (absent / failed) leaves the row on
/// the primary branch without an issue, says so on stderr, and still returns
/// `Ok` — `backlog sync --apply` mirrors it later.
#[allow(clippy::too_many_arguments)]
pub fn add_finding<R, L>(
    path: &Path,
    title: &str,
    project: &str,
    tags: Vec<String>,
    notes: &str,
    weight: f64,
    force: bool,
    now: i64,
    remote_url: &str,
    run: R,
    land: L,
    intake: Intake,
) -> Result<String>
where
    R: Fn(&[&str]) -> Option<(bool, String)>,
    L: Fn(&Path, &str) -> Result<()>,
{
    let is_bare =
        !(project.starts_with('/') || project.starts_with('.') || project.starts_with('~'));
    let CanonicalProject {
        label: project,
        unresolved: project_unresolved,
    } = canonicalize_project_with_marker(project);
    let project = &project;
    let id = with_tasks_lock_aware(path, |locked| {
        // Belt 1 (backlog bd6d81df): without the exclusive lock this add's
        // read-modify-write can be clobbered by a concurrent degraded writer
        // and nothing downstream can tell. "Could not establish mutual
        // exclusion" is not "added", so refuse and say so.
        if !locked {
            return Err(anyhow!(
                "backlog add refused: the tasks-file lock {} could not be acquired within {}s, \
                 so this add's read-modify-write would run without mutual exclusion and could \
                 be silently lost; nothing was added — retry. If this persists, read that \
                 lockfile: it holds the process id of its owner. A lock is reclaimed \
                 automatically ONLY when that pid is provably gone, so it is kept forever when \
                 the file is empty (written by a backlog older than 0.2.27, or a failed write), \
                 when the body is not a pid, or when the pid has been reused by an unrelated \
                 live process. Check whether that process is really a running backlog; if it is \
                 not, remove the lockfile by hand.",
                tasks_lock_path(path).display(),
                TASKS_LOCK_BUDGET.as_secs_f32(),
            ));
        }
        let mut tasks = load(path)?;
        if is_bare {
            reject_ambiguous_bare_label(&tasks, project)?;
        }
        if !force {
            check_duplicate(&tasks, title, project)?;
        }
        let id = new_id(title, now);
        let task = Task {
            id: id.clone(),
            title: title.to_string(),
            project: project.to_string(),
            // Same marker as `add_with_weight`: every write path records the
            // undetermined-scope degrade, otherwise the binary's real entry
            // point (this one) would be the fail-open hole.
            project_unresolved,
            tags,
            status: intake.status,
            notes: notes.to_string(),
            created_at: now,
            updated_at: now,
            defer_until: None,
            weight,
            issue_number: None,
            issue_url: None,
            issue_closed_at: None,
            issue_body_synced_rev: None,
            rev: 0,
            repro: intake.repro,
            closure: None,
            touched_files: Vec::new(),
        };
        tasks.push(task);
        save(path, &tasks)?;
        // Belt 2: report success only about what a fresh read of the file
        // actually shows. Done while the lock is still held, so a well-behaved
        // concurrent writer cannot have moved the file underneath the check.
        confirm_added_id_is_on_disk(path, &id)?;
        Ok(id)
    })?;

    let mirrors = crate::github::is_github_remote(remote_url);
    if mirrors {
        // `{e:#}` inlines the cause: `main` prints only the outer message.
        land(path, &id).map_err(|e| {
            anyhow!(
                "backlog add: task {id} is written to {} but its row could not be confirmed on \
                 the primary branch, so NO GitHub issue was created (the issue is filed only \
                 after the row is on the primary branch and pushed); the row stays in that \
                 store only. Cause: {e:#}",
                path.display()
            )
        })?;
    }
    // Only now may an issue exist. A degraded outcome on a GitHub remote is
    // SAID on stderr: the row is on the primary branch, but the mirror the
    // caller expects does not exist, and silence would read as "mirrored".
    match crate::github::decide_issue_create(remote_url, title, notes, &run) {
        crate::github::IssueOutcome::Created { url } => {
            let number = crate::github::parse_issue_number(&url);
            let shown = number.map_or_else(|| url.clone(), |n| format!("#{n}"));
            record_issue_ref(path, &id, number, &url).map_err(|e| {
                anyhow!(
                    "backlog add: GitHub issue {shown} WAS created for task {id}, but it could \
                     not be recorded in {}; record it by hand before `backlog sync --apply` \
                     (which would otherwise file a duplicate issue). Cause: {e:#}",
                    path.display()
                )
            })?;
            if mirrors {
                land(path, &id).map_err(|e| {
                    anyhow!(
                        "backlog add: GitHub issue {shown} WAS created for task {id} and recorded \
                         in {}, but the primary branch's row does not carry it yet; until it \
                         does, `backlog sync --apply` from another checkout would file a \
                         duplicate issue. Cause: {e:#}",
                        path.display()
                    )
                })?;
            }
        }
        crate::github::IssueOutcome::DegradedLocalOnly { reason } => {
            if mirrors {
                eprintln!(
                    "backlog add: GitHub issue NOT created for task {id} ({reason}); \
                     the task is local-only until `backlog sync --apply` mirrors it"
                );
            }
        }
    }
    Ok(id)
}

/// Record a just-created issue's number/url on row `id` under the tasks lock.
/// `updated_at` is deliberately NOT bumped: the second `land` must make the
/// primary branch's row byte-identical to this one, and the issue reference is
/// a mirror fact, not an edit of the finding.
fn record_issue_ref(path: &Path, id: &str, number: Option<u64>, url: &str) -> Result<()> {
    with_tasks_lock_required(path, "add (record issue)", || {
        let mut tasks = load(path)?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("task {id} is no longer in {}", path.display()))?;
        task.issue_number = number;
        task.issue_url = Some(url.to_string());
        save(path, &tasks)?;
        match load(path)?.iter().find(|t| t.id == id) {
            Some(t) if t.issue_url.as_deref() == Some(url) => Ok(()),
            _ => Err(anyhow!(
                "a fresh load of {} does not show the issue url",
                path.display()
            )),
        }
    })
}

/// Reload the store FROM DISK and require `id` to be present, so `add` can only
/// report success about a write that is observable to the next reader.
///
/// Returns `Err` both when the reload itself fails and when the reload succeeds
/// but does not contain `id` (the measured backlog bd6d81df shape: the record
/// was saved, then a concurrent degraded writer's stale snapshot replaced it).
/// A load failure is NOT treated as "probably fine": an unverifiable write is
/// reported as unverified, naming the store path and what was seen.
fn confirm_added_id_is_on_disk(path: &Path, id: &str) -> Result<()> {
    let reloaded = load(path).with_context(|| {
        format!(
            "backlog add saved task {id} to {} but the store could not be re-read to confirm it; \
             treat the add as unconfirmed and re-check before retrying",
            path.display()
        )
    })?;
    if reloaded.iter().any(|t| t.id == id) {
        return Ok(());
    }
    Err(anyhow!(
        "backlog add saved task {id} to {} but a fresh load of that file does not contain it — \
         the write did not survive (a concurrent writer replaced the file), so the task is NOT \
         in the store as of that read; retry. Store now holds {} task(s): {:?}",
        path.display(),
        reloaded.len(),
        reloaded.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
    ))
}

/// The order `add_finding` imposes between the row write, `land` (row onto
/// the primary branch) and `run` (`gh`). Written by the implementer; the
/// end-to-end order against real git is pinned independently by
/// `tests/add_integrates_row_to_main_before_issue.rs`.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod land_order_tests {
    use super::*;
    use std::cell::RefCell;

    const GH: &str = "https://github.com/o/r.git";

    fn tmp_path() -> PathBuf {
        let dir = tempfile::tempdir().expect("tmp dir");
        let path = dir.path().join("tasks.toml");
        std::mem::forget(dir);
        path
    }

    fn intake() -> Intake {
        Intake {
            status: STATUS_PENDING,
            repro: Some(fixture_reproduced_repro(1000)),
        }
    }

    fn add_with<R, L>(path: &Path, remote: &str, run: R, land: L) -> Result<String>
    where
        R: Fn(&[&str]) -> Option<(bool, String)>,
        L: Fn(&Path, &str) -> Result<()>,
    {
        add_finding(
            path,
            "T",
            "/repo",
            vec![],
            "",
            0.0,
            false,
            1000,
            remote,
            run,
            land,
            intake(),
        )
    }

    #[test]
    fn land_err_means_no_gh_call_and_err() {
        let path = tmp_path();
        let err = add_with(
            &path,
            GH,
            |_| panic!("gh must not run when the row is not on main"),
            |_, _| Err(anyhow!("refused by test")),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("NO GitHub issue was created"), "{msg}");
        assert!(msg.contains("refused by test"), "{msg}");
        // The row stays in the worktree store, without issue fields.
        let tasks = load(&path).unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].issue_number.is_none());
    }

    #[test]
    fn land_before_gh_and_again_after_created_with_the_number() {
        let path = tmp_path();
        let events = RefCell::new(Vec::<String>::new());
        let id = add_with(
            &path,
            GH,
            |_| {
                events.borrow_mut().push("gh".into());
                Some((true, "https://github.com/o/r/issues/77\n".into()))
            },
            |p, id| {
                let row = load(p)?.into_iter().find(|t| t.id == id).unwrap();
                events
                    .borrow_mut()
                    .push(format!("land:{:?}", row.issue_number));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *events.borrow(),
            ["land:None", "gh", "land:Some(77)"],
            "row must be landed before gh, then re-landed carrying the number"
        );
        let t = load(&path)
            .unwrap()
            .into_iter()
            .find(|t| t.id == id)
            .unwrap();
        assert_eq!(t.issue_number, Some(77));
        assert_eq!(
            t.updated_at, 1000,
            "recording the issue must not bump updated_at"
        );
    }

    #[test]
    fn second_land_err_names_the_created_issue() {
        let path = tmp_path();
        let calls = RefCell::new(0);
        let err = add_with(
            &path,
            GH,
            |_| Some((true, "https://github.com/o/r/issues/78\n".into())),
            |_, _| {
                *calls.borrow_mut() += 1;
                if *calls.borrow() == 2 {
                    Err(anyhow!("second land refused"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("#78"), "{msg}");
        assert_eq!(*calls.borrow(), 2);
    }

    #[test]
    fn degraded_gh_lands_once_and_succeeds() {
        let path = tmp_path();
        let calls = RefCell::new(0);
        add_with(
            &path,
            GH,
            |_| None,
            |_, _| {
                *calls.borrow_mut() += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn non_github_remote_never_lands() {
        let path = tmp_path();
        add_with(
            &path,
            "https://gitlab.com/o/r.git",
            |_| panic!("no gh for a non-github remote"),
            |_, _| panic!("no land for a non-github remote"),
        )
        .unwrap();
    }
}
