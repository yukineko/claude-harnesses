//! The set of checkouts a Stop gate must judge.
//!
//! A gate that uses only the hook `cwd` judges the wrong tree when the session
//! worked in a linked git worktree while the hook ran in the primary tree: it
//! sees a clean tree and allows. The roots here are the hook cwd plus the git
//! toplevel of every file the session transcript records as written.
//!
//! Fail-closed (CLAUDE.md section 3): an unreadable transcript, a relative
//! written path, a written file whose directory is gone or unenterable (except
//! the removed-worktree and nearest-existing-ancestor cases below), or any git
//! failure other than the one below is `Undetermined` naming the path.
//!
//! Removed linked worktrees: whenever a written file resolves to the toplevel
//! of a linked worktree, the pair (worktree toplevel, main-repository toplevel)
//! is appended to the durable store in [`crate::worktree_origin`]. When a
//! written file's directory no longer exists, the store is consulted: if the
//! longest recorded worktree containing the path no longer exists itself, and
//! the recorded repository still exists and git reports it as its own
//! toplevel, that repository becomes a root (Known). When NO record covers an
//! absolute written path (including a missing store), the walk goes up to the
//! nearest existing ancestor directory (only `NotFound` continues the walk; any
//! other stat error, a non-directory or a dangling-symlink ancestor stops it as
//! `Undetermined`) and asks git there: positively outside every repository
//! (the exclusion rule below) means the gone path is outside every live repo
//! and is excluded (Known); inside a repository makes that repository's
//! toplevel a root (Known); any other git failure (unenterable ancestor,
//! corrupt gitfile, git missing) stays `Undetermined`. A relative path with no
//! record stays `Undetermined`. With a record, every other outcome stays
//! `Undetermined`: a recorded worktree that still exists (a deleted
//! subdirectory of a live recorded checkout), a recorded repository that is
//! gone or no longer a toplevel, or a store that cannot be read or has an
//! unparsable line. Recording happens for every live linked
//! worktree the session wrote into, even when another write in the same
//! session is unresolvable (the verdict is then still `Undetermined`, naming
//! the first unresolvable write). Limitation: the mapping exists only if
//! something recorded it while the worktree existed: a resolve that saw a
//! write into it, or the repo's `.githooks/post-checkout` recorder, which
//! `git worktree add` runs inside the new worktree. A worktree with neither
//! has no record and is judged by its nearest existing ancestor as above. A failure to record a pair never
//! changes the current resolve; it is reported on stderr.
//! The exclusion rule: an absolute written path whose existing parent
//! directory (or, for a gone directory with no record, nearest existing
//! ancestor) git positively reports as outside every repository (exit 128 and
//! the full "not a git repository (or any of the parent directories)" message)
//! is skipped, not an error. It still counts toward `wrote_any`.
//! The git child runs with repository-discovery env vars (GIT_CEILING_DIRECTORIES,
//! GIT_DIR, ...) removed, so inherited env cannot make an in-repo path look
//! outside every repository.
//! A write in this session's own Claude Code scratchpad (see
//! `in_own_scratchpad`) is not a root and not an error, even when git cannot
//! judge its directory.
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

/// The nearest ancestor directory of `written` that exists, for a write whose
/// own directory is gone. Only `NotFound` lets the walk continue upward: any
/// other stat error (e.g. an ancestor git or we cannot search), an ancestor
/// that exists but is not a directory, or a dangling symlink ancestor is an
/// error, because the gone path's real location is then unknown.
fn nearest_existing_ancestor(written: &Path) -> Result<PathBuf, String> {
    for a in written.ancestors().skip(1) {
        if a.as_os_str().is_empty() {
            break;
        }
        match std::fs::symlink_metadata(a) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot stat ancestor {}: {e}", a.display())),
            Ok(m) if m.file_type().is_symlink() => {
                return match std::fs::metadata(a) {
                    Ok(t) if t.is_dir() => Ok(a.to_path_buf()),
                    Ok(_) => Err(format!(
                        "ancestor {} is a symlink to a non-directory",
                        a.display()
                    )),
                    Err(e) => Err(format!(
                        "ancestor {} is a symlink that cannot be followed: {e}",
                        a.display()
                    )),
                }
            }
            Ok(m) if m.is_dir() => return Ok(a.to_path_buf()),
            Ok(_) => {
                return Err(format!(
                    "ancestor {} exists but is not a directory",
                    a.display()
                ))
            }
        }
    }
    Err("no existing ancestor directory".to_string())
}

/// The repository a write into a since-removed linked worktree belongs to
/// (`Ok(Some)`), `Ok(None)` when no worktree-origin record covers it, or why it
/// cannot be determined (see the module docs).
fn removed_worktree_repo(written: &Path, store: &Path) -> Result<Option<PathBuf>, String> {
    use crate::worktree_origin::{lookup, Lookup};
    let (worktree, repo) = match lookup(store, written)? {
        Lookup::NoRecord => return Ok(None),
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
        Ok(t) if t == repo => Ok(Some(repo)),
        Ok(t) => Err(format!(
            "the recorded repository {} is no longer a git toplevel (git reports {})",
            repo.display(),
            t.display()
        )),
        Err(e) => Err(format!("the recorded repository {}: {e}", repo.display())),
    }
}

/// Whether `dir` lies inside THIS session's Claude Code scratchpad:
/// canonically `…/claude-<digits>/<slug>/<SID>/scratchpad[/…]`, where `<SID>`
/// is the transcript's file stem. Claude Code tells the model to put temporary
/// files there, and nothing a gate checks lives in it. Another session's
/// scratchpad, or any other directory outside git, does not match and stays
/// undetermined. A directory that cannot be canonicalized does not match.
fn in_own_scratchpad(dir: &Path, transcript_path: &str) -> bool {
    let Some(sid) = Path::new(transcript_path)
        .file_stem()
        .and_then(|s| s.to_str())
    else {
        return false;
    };
    let Ok(canon) = std::fs::canonicalize(dir) else {
        return false;
    };
    let comps: Vec<&std::ffi::OsStr> = canon.iter().collect();
    comps.windows(4).any(|w| {
        let uid_ok = w[0]
            .to_str()
            .and_then(|s| s.strip_prefix("claude-"))
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
        uid_ok && w[2] == sid && w[3] == "scratchpad"
    })
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
            _ => {
                let no_record = || {
                    format!(
                        "no worktree-origin record in {} covers it",
                        origin_store.display()
                    )
                };
                let why = match removed_worktree_repo(p, origin_store) {
                    Ok(Some(repo)) => {
                        if !roots.contains(&repo) {
                            roots.push(repo);
                        }
                        continue;
                    }
                    // No record: judge the nearest existing ancestor (absolute
                    // paths only). Outside every repo -> excluded; inside a
                    // repo -> that repo is a root; anything else stays
                    // undetermined.
                    Ok(None) if p.is_absolute() => match nearest_existing_ancestor(p) {
                        Ok(anc) => match toplevel(&anc) {
                            Ok(t) => {
                                if !checked_linked.contains(&t) {
                                    record_if_linked(&t, origin_store);
                                    checked_linked.push(t.clone());
                                }
                                if !roots.contains(&t) {
                                    roots.push(t);
                                }
                                continue;
                            }
                            Err(TopErr::OutsideAnyRepo) => continue,
                            Err(e) => format!(
                                "{}, and its nearest existing ancestor {} cannot be judged: {e}",
                                no_record(),
                                anc.display()
                            ),
                        },
                        Err(e) => format!("{}, and {e}", no_record()),
                    },
                    Ok(None) => no_record(),
                    Err(why) => why,
                };
                first_unresolved.get_or_insert_with(|| {
                    format!(
                        "session roots: the session wrote {} but its directory does not \
                         exist, so its git root cannot be determined ({why})",
                        p.display()
                    )
                });
                continue;
            }
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
            Err(_) if in_own_scratchpad(parent, transcript_path) => {}
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

    // user ruling 2026-10-27: no record + directory gone + nearest existing
    // ancestor positively outside every repo is excluded, no longer Undetermined.
    #[test]
    fn no_record_gone_dir_under_non_repo_base_is_excluded() {
        let f = fixture();
        // Never resolved while the worktree existed: nothing recorded.
        remove_wt(&f);
        match resolve_with_store(&f.repo, &f.transcript, &f.store) {
            Determination::Known(sr) => {
                assert_eq!(sr.roots, vec![std::fs::canonicalize(&f.repo).unwrap()]);
                assert!(sr.wrote_any);
            }
            Determination::Undetermined(w) => panic!("{}", w.as_str()),
        }
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
    // user ruling 2026-10-27: the unresolvable write is now one under a mode-000
    // directory (a gone dir under a non-repo base is excluded, not an error).
    #[cfg(unix)]
    #[test]
    fn live_linked_worktree_recorded_despite_unresolvable_write() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let base = f.repo.parent().unwrap().to_path_buf();
        // Sorts before `wt/...` (the edited set is a sorted BTreeSet of strings);
        // its ancestor `aaa_locked` cannot be entered, no store record covers it.
        let locked = base.join("aaa_locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let gone = locked.join("gone").join("x.rs");
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
        let got = resolve_with_store(&f.repo, &f.transcript, &f.store);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let why = undetermined(got);
        assert!(why.contains("aaa_locked"), "{why}");
        match crate::worktree_origin::lookup(&f.store, &live).unwrap() {
            crate::worktree_origin::Lookup::Found { worktree, repo } => {
                assert_eq!(worktree, f.wt);
                assert_eq!(repo, f.repo);
            }
            other => panic!("live linked worktree was not recorded: {other:?}"),
        }
    }
}
