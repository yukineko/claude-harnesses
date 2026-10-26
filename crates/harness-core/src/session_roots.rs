//! The set of checkouts a Stop gate must judge.
//!
//! A gate that uses only the hook `cwd` judges the wrong tree when the session
//! worked in a linked git worktree while the hook ran in the primary tree: it
//! sees a clean tree and allows. The roots here are the hook cwd plus the git
//! toplevel of every file the session transcript records as written.
//!
//! Fail-closed (CLAUDE.md section 3): an unreadable transcript, a relative
//! written path, a written file whose directory is gone or unenterable (except
//! the removed-worktree case below), or any git failure other than the one
//! below is `Undetermined` naming the path.
//!
//! Removed linked worktrees: whenever a written file resolves to the toplevel
//! of a linked worktree, the pair (worktree toplevel, main-repository toplevel)
//! is appended to the durable store in [`crate::worktree_origin`]. When a
//! written file's directory no longer exists, the store is consulted: if the
//! longest recorded worktree containing the path no longer exists itself, and
//! the recorded repository still exists and git reports it as its own
//! toplevel, that repository becomes a root (Known). Every other outcome stays
//! `Undetermined`: no record (including a missing store), a recorded worktree
//! that still exists (a deleted subdirectory of a live checkout), a recorded
//! repository that is gone or no longer a toplevel, or a store that cannot be
//! read or has an unparsable line. Recording happens for every live linked
//! worktree the session wrote into, even when another write in the same
//! session is unresolvable (the verdict is then still `Undetermined`, naming
//! the first unresolvable write). Limitation: the mapping exists only if
//! something recorded it while the worktree existed: a resolve that saw a
//! write into it, or the repo's `.githooks/post-checkout` recorder, which
//! `git worktree add` runs inside the new worktree. A worktree with neither
//! has no record and stays `Undetermined`. A failure to record a pair never
//! changes the current resolve; it is reported on stderr.
//! The single exclusion: an absolute written path whose existing parent
//! directory git positively reports as outside every repository (exit 128 and
//! the full "not a git repository (or any of the parent directories)" message)
//! is skipped, not an error. It still counts toward `wrote_any`.
//! The git child runs with repository-discovery env vars (GIT_CEILING_DIRECTORIES,
//! GIT_DIR, ...) removed, so inherited env cannot make an in-repo path look
//! outside every repository.
//! Bash-derived candidates are heuristic (see
//! [`crate::attribution::SessionWrites::bash`]): a candidate outside any git
//! work tree is not a root and is not an error.

use crate::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoots {
    /// Canonical, deduplicated, hook cwd first.
    pub roots: Vec<PathBuf>,
    /// The transcript records at least one written file.
    pub wrote_any: bool,
}

/// Why a toplevel could not be resolved. `OutsideAnyRepo` is a positive
/// observation (git itself said the directory is in no repository); everything
/// else is a failure to determine.
enum TopErr {
    OutsideAnyRepo,
    Other(String),
}

impl std::fmt::Display for TopErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TopErr::OutsideAnyRepo => f.write_str("not inside any git repository"),
            TopErr::Other(s) => f.write_str(s),
        }
    }
}

/// Environment variables that steer git's repository discovery. They are
/// inherited from the hook process and must not influence a child that asks
/// "which repository contains this directory": e.g. `GIT_CEILING_DIRECTORIES`
/// makes an in-repo directory look outside every repository.
pub(crate) const GIT_DISCOVERY_ENV: [&str; 9] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Remove every [`GIT_DISCOVERY_ENV`] variable from `cmd`'s environment.
pub(crate) fn clear_git_discovery_env(cmd: &mut Command) -> &mut Command {
    for k in GIT_DISCOVERY_ENV {
        cmd.env_remove(k);
    }
    cmd
}

fn toplevel(dir: &Path) -> Result<PathBuf, TopErr> {
    let mut cmd = Command::new("git");
    clear_git_discovery_env(&mut cmd);
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| TopErr::Other(format!("cannot run git in {}: {e}", dir.display())))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Positively outside: exit 128 AND the full phrase. A corrupt gitfile
        // says "not a git repository" without the parenthetical, and stays Other.
        if out.status.code() == Some(128)
            && stderr.contains("not a git repository (or any of the parent directories)")
        {
            return Err(TopErr::OutsideAnyRepo);
        }
        return Err(TopErr::Other(format!(
            "git rev-parse --show-toplevel failed in {} (exit {:?}): {}",
            dir.display(),
            out.status.code(),
            stderr.trim()
        )));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s.is_empty() {
        return Err(TopErr::Other(format!(
            "git printed no toplevel for {}",
            dir.display()
        )));
    }
    std::fs::canonicalize(s)
        .map_err(|e| TopErr::Other(format!("cannot canonicalize toplevel {s}: {e}")))
}

/// If `toplevel` is a linked worktree, durably record which repository it
/// belongs to. Failures are reported, never fatal: the caller's root is Known.
fn record_if_linked(toplevel: &Path, store: &Path) {
    match crate::worktree_origin::linked_origin(toplevel) {
        Ok(Some(repo)) => {
            if let Err(e) = crate::worktree_origin::record(store, toplevel, &repo) {
                eprintln!(
                    "warning: session roots: could not record that worktree {} belongs to {} \
                     in {} ({e}); a write into it becomes undeterminable once it is removed",
                    toplevel.display(),
                    repo.display(),
                    store.display()
                );
            }
        }
        Ok(None) => {}
        Err(e) => eprintln!(
            "warning: session roots: could not tell whether {} is a linked worktree ({e}); \
             its origin is not recorded",
            toplevel.display()
        ),
    }
}

/// The repository a write into a since-removed linked worktree belongs to, or
/// why it cannot be determined (see the module docs).
fn removed_worktree_repo(written: &Path, store: &Path) -> Result<PathBuf, String> {
    use crate::worktree_origin::{lookup, Lookup};
    let (worktree, repo) = match lookup(store, written)? {
        Lookup::NoRecord => {
            return Err(format!(
                "no worktree-origin record in {} covers it",
                store.display()
            ))
        }
        Lookup::Found { worktree, repo } => (worktree, repo),
    };
    if worktree.exists() {
        return Err(format!(
            "the recorded worktree {} still exists, so this is not a removed worktree",
            worktree.display()
        ));
    }
    if !repo.is_dir() {
        return Err(format!(
            "the recorded worktree {} belonged to {}, which no longer exists",
            worktree.display(),
            repo.display()
        ));
    }
    match toplevel(&repo) {
        Ok(t) if t == repo => Ok(repo),
        Ok(t) => Err(format!(
            "the recorded repository {} is no longer a git toplevel (git reports {})",
            repo.display(),
            t.display()
        )),
        Err(e) => Err(format!("the recorded repository {}: {e}", repo.display())),
    }
}

/// Resolve the roots for a hook at `cwd` with the session transcript at
/// `transcript_path`. An empty `transcript_path` (manual CLI run) records no
/// writes: the only root is `cwd`.
pub fn resolve(cwd: &Path, transcript_path: &str) -> Determination<SessionRoots> {
    resolve_with_store(
        cwd,
        transcript_path,
        &crate::worktree_origin::default_store(),
    )
}

/// [`resolve`] with an explicit [`crate::worktree_origin`] store path.
pub fn resolve_with_store(
    cwd: &Path,
    transcript_path: &str,
    origin_store: &Path,
) -> Determination<SessionRoots> {
    let cwd_c = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut roots = vec![cwd_c];
    if transcript_path.is_empty() {
        return Determination::known(SessionRoots {
            roots,
            wrote_any: false,
        });
    }
    // A transcript file that does not exist records no writes: there is nothing
    // to resolve roots from, and every caller's per-root attribution is then
    // itself undetermined and judges the whole checkout (the restrictive side).
    // Any other read problem (permissions, unparsable content) is Undetermined
    // below and blocks.
    if matches!(
        std::fs::metadata(transcript_path),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound
    ) {
        return Determination::known(SessionRoots {
            roots,
            wrote_any: false,
        });
    }
    let writes = match crate::attribution::session_written_paths(transcript_path) {
        Determination::Known(w) => w,
        Determination::Undetermined(why) => {
            return Determination::undetermined(format!(
                "session roots: cannot read the session's writes from the transcript: {}",
                why.as_str()
            ))
        }
    };
    let wrote_any = !writes.edited.is_empty() || !writes.bash.is_empty();
    let mut checked_linked: Vec<PathBuf> = Vec::new();
    // The first unresolvable write decides the verdict (Undetermined), but the
    // walk continues: every still-live linked worktree the session wrote into
    // must be recorded now, or it becomes unattributable once removed.
    let mut first_unresolved: Option<String> = None;
    for p in &writes.edited {
        let parent = match p.parent() {
            Some(d) if d.is_dir() => d,
            _ => match removed_worktree_repo(p, origin_store) {
                Ok(repo) => {
                    if !roots.contains(&repo) {
                        roots.push(repo);
                    }
                    continue;
                }
                Err(why) => {
                    first_unresolved.get_or_insert_with(|| {
                        format!(
                            "session roots: the session wrote {} but its directory does not \
                             exist, so its git root cannot be determined ({why})",
                            p.display()
                        )
                    });
                    continue;
                }
            },
        };
        match toplevel(parent) {
            Ok(t) => {
                if !checked_linked.contains(&t) {
                    record_if_linked(&t, origin_store);
                    checked_linked.push(t.clone());
                }
                if !roots.contains(&t) {
                    roots.push(t);
                }
            }
            // Positively outside every repo (absolute path, existing dir, git
            // exit 128 with the full message): no checkout to judge, so excluded.
            Err(TopErr::OutsideAnyRepo) if p.is_absolute() => {}
            Err(e) => {
                first_unresolved.get_or_insert_with(|| {
                    format!(
                        "session roots: the session wrote {} but its git root cannot be \
                         determined: {e}",
                        p.display()
                    )
                });
            }
        }
    }
    if let Some(why) = first_unresolved {
        return Determination::undetermined(why);
    }
    for p in &writes.bash {
        let dir = if p.is_dir() {
            Some(p.as_path())
        } else {
            p.parent()
        };
        let Some(dir) = dir else { continue };
        if let Ok(t) = toplevel(dir) {
            if !roots.contains(&t) {
                roots.push(t);
            }
        }
    }
    Determination::known(SessionRoots { roots, wrote_any })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        wt: PathBuf,
        store: PathBuf,
        transcript: String,
    }

    /// repo + linked worktree `wt` with a written file `wt/src/a.rs`, a
    /// transcript recording that write, and a private origin store path.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        let wt = base.join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        );
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
        );
        let written = wt.join("src").join("a.rs");
        std::fs::create_dir_all(written.parent().unwrap()).unwrap();
        std::fs::write(&written, "x").unwrap();
        let line = serde_json::json!({
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_0", "name": "Write",
                "input": {"file_path": written.to_str().unwrap(), "content": "x"}
            }]}
        });
        let tpath = base.join("session.jsonl");
        std::fs::write(&tpath, format!("{line}\n")).unwrap();
        Fixture {
            store: base.join("store").join(crate::worktree_origin::FILE_NAME),
            transcript: tpath.to_string_lossy().into_owned(),
            _tmp: tmp,
            repo,
            wt,
        }
    }

    fn remove_wt(f: &Fixture) {
        git(
            &f.repo,
            &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
        );
        assert!(!f.wt.exists());
    }

    fn undetermined(d: Determination<SessionRoots>) -> String {
        match d {
            Determination::Undetermined(w) => w.as_str().to_string(),
            Determination::Known(sr) => panic!("expected Undetermined, got Known {sr:?}"),
        }
    }

    #[test]
    fn recorded_removed_worktree_resolves_to_its_repo() {
        let f = fixture();
        assert!(matches!(
            resolve_with_store(&f.repo, &f.transcript, &f.store),
            Determination::Known(_)
        ));
        remove_wt(&f);
        match resolve_with_store(&f.repo, &f.transcript, &f.store) {
            Determination::Known(sr) => {
                assert!(sr.roots.contains(&f.repo), "{sr:?}");
                assert!(sr.wrote_any);
            }
            Determination::Undetermined(w) => panic!("{}", w.as_str()),
        }
    }

    #[test]
    fn no_record_stays_undetermined() {
        let f = fixture();
        // Never resolved while the worktree existed: nothing recorded.
        remove_wt(&f);
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("no worktree-origin record"), "{why}");
    }

    #[test]
    fn corrupt_store_is_undetermined() {
        let f = fixture();
        assert!(matches!(
            resolve_with_store(&f.repo, &f.transcript, &f.store),
            Determination::Known(_)
        ));
        remove_wt(&f);
        crate::append::append_line(&f.store, "{garbage").unwrap();
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("unparsable line"), "{why}");
    }

    #[test]
    fn unreadable_store_is_undetermined() {
        let f = fixture();
        remove_wt(&f);
        std::fs::create_dir_all(&f.store).unwrap();
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(
            why.contains("cannot read the worktree-origin store"),
            "{why}"
        );
    }

    #[test]
    fn recorded_repo_that_no_longer_exists_is_undetermined() {
        let f = fixture();
        assert!(matches!(
            resolve_with_store(&f.repo, &f.transcript, &f.store),
            Determination::Known(_)
        ));
        remove_wt(&f);
        std::fs::remove_dir_all(&f.repo).unwrap();
        let cwd = f.store.parent().unwrap();
        let why = undetermined(resolve_with_store(cwd, &f.transcript, &f.store));
        assert!(why.contains("no longer exists"), "{why}");
    }

    #[test]
    fn recorded_repo_that_is_not_a_toplevel_is_undetermined() {
        let f = fixture();
        remove_wt(&f);
        let sub = f.repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        crate::worktree_origin::record(&f.store, &f.wt, &sub).unwrap();
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("no longer a git toplevel"), "{why}");
    }

    #[test]
    fn deleted_subdir_of_live_recorded_worktree_stays_undetermined() {
        let f = fixture();
        assert!(matches!(
            resolve_with_store(&f.repo, &f.transcript, &f.store),
            Determination::Known(_)
        ));
        std::fs::remove_dir_all(f.wt.join("src")).unwrap();
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("still exists"), "{why}");
    }

    #[test]
    fn main_tree_write_records_nothing() {
        let f = fixture();
        let written = f.repo.join("b.rs");
        std::fs::write(&written, "x").unwrap();
        let line = serde_json::json!({
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_1", "name": "Write",
                "input": {"file_path": written.to_str().unwrap(), "content": "x"}
            }]}
        });
        std::fs::write(&f.transcript, format!("{line}\n")).unwrap();
        assert!(matches!(
            resolve_with_store(&f.repo, &f.transcript, &f.store),
            Determination::Known(_)
        ));
        assert!(!f.store.exists());
    }
    #[test]
    fn live_linked_worktree_recorded_despite_unresolvable_write() {
        let f = fixture();
        let base = f.repo.parent().unwrap().to_path_buf();
        // Sorts before `wt/...` (the edited set is a sorted BTreeSet of strings),
        // directory absent, no store record covers it.
        let gone = base.join("aaa_gone").join("x.rs");
        let live = f.wt.join("src").join("a.rs");
        assert!(gone.to_string_lossy() < live.to_string_lossy());
        assert!(!gone.parent().unwrap().exists());
        let mk = |id: &str, p: &Path| {
            serde_json::json!({
                "message": {"role": "assistant", "content": [{
                    "type": "tool_use", "id": id, "name": "Write",
                    "input": {"file_path": p.to_str().unwrap(), "content": "x"}
                }]}
            })
        };
        std::fs::write(
            &f.transcript,
            format!("{}\n{}\n", mk("toolu_a", &live), mk("toolu_b", &gone)),
        )
        .unwrap();
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("aaa_gone"), "{why}");
        match crate::worktree_origin::lookup(&f.store, &live).unwrap() {
            crate::worktree_origin::Lookup::Found { worktree, repo } => {
                assert_eq!(worktree, f.wt);
                assert_eq!(repo, f.repo);
            }
            other => panic!("live linked worktree was not recorded: {other:?}"),
        }
    }
}
