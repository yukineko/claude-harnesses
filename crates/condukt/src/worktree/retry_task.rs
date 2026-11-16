//! Retry-worktree task identity (backlog b2d9cb6b): a retry worktree is created
//! with a suffixed topic (`<task>-rN`); the identity recorded for its branch
//! must name the real task.

use super::*;

/// The task id a worktree `topic` stands for, given the ids of the run's tasks.
/// A retry worktree is created with a suffixed topic (`<task>-r1`) to avoid the
/// path collision with the first attempt; the identity recorded for its branch
/// must name the real task, not the topic.
///
/// * a topic that IS a task id of the run resolves to itself (so a task
///   literally named `foo-r1` is never re-attributed to `foo`);
/// * otherwise a single trailing `-r<digits>` is stripped and the remainder is
///   used only if it is a task id of the run;
/// * anything else (no such task) resolves to the topic unchanged, which the
///   teardown/merge gates keep refusing as an unknown task (fail-closed).
pub fn resolve_retry_topic(topic: &str, task_ids: &[&str]) -> String {
    if task_ids.contains(&topic) {
        return topic.to_string();
    }
    if let Some((base, n)) = topic.rsplit_once("-r") {
        if !base.is_empty()
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && task_ids.contains(&base)
        {
            return base.to_string();
        }
    }
    topic.to_string()
}

/// [`create_namespaced`] with an explicit `task` for the recorded
/// `<run>/<task>` identity (it differs from `topic` for a retry worktree).
pub fn create_namespaced_for_task(
    repo: &Path,
    worktree_base: &Path,
    run: Option<&str>,
    topic: &str,
    branch: &str,
    task: &str,
) -> Result<PathBuf> {
    match run {
        None => {
            eprintln!("{}", legacy_degradation_warning(topic, branch));
            create(repo, worktree_base, topic, branch)
        }
        Some(run) => {
            validate_run_ns(run)?;
            // Validate the *inputs* too, so a bad topic/branch is reported
            // against what the caller passed rather than against the spliced
            // form (create() re-validates the spliced result regardless).
            validate_topic(topic)?;
            validate_branch(branch)?;
            let ns_branch = namespaced_branch(run, branch);
            let path = create(
                repo,
                worktree_base,
                &namespaced_topic(run, topic),
                &ns_branch,
            )?;
            // The task identity every commit on this branch must carry as a
            // `Condukt-Task:` trailer (checked by a task-named `merge`). Without
            // it such a merge refuses, so a failed record rolls the worktree
            // back rather than returning a branch condukt cannot integrate.
            if let Err(e) = record_task_identity(repo, &ns_branch, &format!("{run}/{task}")) {
                let path_str = path.to_string_lossy().to_string();
                let _ = git_try(repo, &["worktree", "remove", "--force", "--", &path_str]);
                let _ = git_try(repo, &["branch", "-D", &ns_branch]);
                return Err(e);
            }
            Ok(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_retry_topic_maps_only_unambiguous_retry_suffixes() {
        let ids = ["e70b6a71", "foo", "foo-r1", "bar"];
        // exact task id wins, even when it looks like a retry of another task
        assert_eq!(resolve_retry_topic("foo-r1", &ids), "foo-r1");
        assert_eq!(resolve_retry_topic("foo", &ids), "foo");
        // unambiguous retry suffix resolves to the real task
        assert_eq!(resolve_retry_topic("e70b6a71-r1", &ids), "e70b6a71");
        assert_eq!(resolve_retry_topic("bar-r12", &ids), "bar");
        // base not a task of the run: unchanged (stays undeterminable)
        assert_eq!(resolve_retry_topic("nope-r1", &ids), "nope-r1");
        // not a pure numeric retry suffix: unchanged
        assert_eq!(resolve_retry_topic("bar-r", &ids), "bar-r");
        assert_eq!(resolve_retry_topic("bar-rx", &ids), "bar-rx");
        assert_eq!(resolve_retry_topic("-r1", &ids), "-r1");
    }
}
