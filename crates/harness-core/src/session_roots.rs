//! The set of checkouts a Stop gate must judge.
//!
//! A gate that uses only the hook `cwd` judges the wrong tree when the session
//! worked in a linked git worktree while the hook ran in the primary tree: it
//! sees a clean tree and allows. The roots here are the hook cwd plus the git
//! toplevel of every file the session transcript records as written.
//!
//! Fail-closed (CLAUDE.md section 3): an unreadable transcript, a relative
//! written path, a written file whose directory is gone or unenterable (except
//! the removed-worktree, live-recorded-worktree and nearest-existing-ancestor
//! cases below), or any git
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
//! record stays `Undetermined`. When the longest recorded worktree containing
//! the path still exists (a deleted subdirectory of a live recorded checkout),
//! the nearest existing ancestor is found by the same walk; it must
//! canonicalize to a path inside the recorded worktree (a symlink escaping it
//! is `Undetermined`), and git's toplevel there becomes a root (Known). Unlike
//! the no-record case, git reporting that ancestor outside every repository is
//! `Undetermined`, not excluded, as is any other walk, canonicalize or git
//! failure. With a record, every other outcome stays `Undetermined`: a
//! recorded repository that is gone or no longer a toplevel, or a store that
//! cannot be read or has an unparsable line. Recording happens for every live linked
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
    // All 13 inherited repo-selecting vars (crate::git_env), not only the 9
    // discovery vars of GIT_DISCOVERY_ENV.
    crate::git_env::scrub(&mut cmd);
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

/// What the worktree-origin store says about a write whose directory is gone.
enum Recorded {
    /// No record covers the written path.
    NoRecord,
    /// The recorded worktree is gone; the write belongs to this repository.
    Removed(PathBuf),
    /// The recorded worktree (canonical) still exists: the write was into a
    /// since-deleted subdirectory of a live checkout.
    Live(PathBuf),
}

/// Consult the worktree-origin store for `written`, or say why it cannot be
/// determined (see the module docs).
fn removed_worktree_repo(written: &Path, store: &Path) -> Result<Recorded, String> {
    use crate::worktree_origin::{lookup, Lookup};
    let (worktree, repo) = match lookup(store, written)? {
        Lookup::NoRecord => return Ok(Recorded::NoRecord),
        Lookup::Found { worktree, repo } => (worktree, repo),
    };
    if worktree.exists() {
        return Ok(Recorded::Live(worktree));
    }
    if !repo.is_dir() {
        return Err(format!(
            "the recorded worktree {} belonged to {}, which no longer exists",
            worktree.display(),
            repo.display()
        ));
    }
    match toplevel(&repo) {
        Ok(t) if t == repo => Ok(Recorded::Removed(repo)),
        Ok(t) => Err(format!(
            "the recorded repository {} is no longer a git toplevel (git reports {})",
            repo.display(),
            t.display()
        )),
        Err(e) => Err(format!("the recorded repository {}: {e}", repo.display())),
    }
}

/// The git toplevel for a write into a deleted subdirectory of the live
/// recorded (canonical) `worktree`: the nearest existing ancestor must
/// canonicalize to a path inside `worktree` (a symlink escaping it is not
/// followed), and git at that ancestor must name a toplevel. Every other
/// outcome, including git reporting the ancestor outside every repository, is
/// an error: the record says the path belongs to a checkout.
fn live_worktree_toplevel(written: &Path, worktree: &Path) -> Result<PathBuf, String> {
    let anc = nearest_existing_ancestor(written).map_err(|e| {
        format!(
            "it lies in the live recorded worktree {}, and {e}",
            worktree.display()
        )
    })?;
    let canon = std::fs::canonicalize(&anc).map_err(|e| {
        format!(
            "cannot canonicalize its nearest existing ancestor {}: {e}",
            anc.display()
        )
    })?;
    if !canon.starts_with(worktree) {
        return Err(format!(
            "its nearest existing ancestor {} resolves to {}, outside the live recorded \
             worktree {}",
            anc.display(),
            canon.display(),
            worktree.display()
        ));
    }
    toplevel(&canon).map_err(|e| {
        format!(
            "it lies in the live recorded worktree {}, and its nearest existing ancestor {} \
             cannot be judged: {e}",
            worktree.display(),
            anc.display()
        )
    })
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
                    Ok(Recorded::Removed(repo)) => {
                        if !roots.contains(&repo) {
                            roots.push(repo);
                        }
                        continue;
                    }
                    // A deleted subdirectory of a live recorded worktree: git
                    // at the nearest existing ancestor inside it names the root.
                    Ok(Recorded::Live(wt)) => match live_worktree_toplevel(p, &wt) {
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
                        Err(why) => why,
                    },
                    // No record: judge the nearest existing ancestor (absolute
                    // paths only). Outside every repo -> excluded; inside a
                    // repo -> that repo is a root; anything else stays
                    // undetermined.
                    Ok(Recorded::NoRecord) if p.is_absolute() => match nearest_existing_ancestor(p)
                    {
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
                    Ok(Recorded::NoRecord) => no_record(),
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
        let o = crate::git_env::scrub(&mut Command::new("git"))
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

    // user ruling 2026-10-29: 直す — a deleted subdirectory of a still-live
    // recorded worktree is no longer Undetermined. The nearest existing ancestor
    // (inside the recorded worktree) is judged by git, and its toplevel is a root.
    #[test]
    fn deleted_subdir_of_live_recorded_worktree_resolves_to_the_worktree() {
        let f = fixture();
        assert!(matches!(
            resolve_with_store(&f.repo, &f.transcript, &f.store),
            Determination::Known(_)
        ));
        std::fs::remove_dir_all(f.wt.join("src")).unwrap();
        assert!(f.wt.is_dir());
        match resolve_with_store(&f.repo, &f.transcript, &f.store) {
            Determination::Known(sr) => {
                assert!(
                    sr.roots.contains(&std::fs::canonicalize(&f.wt).unwrap()),
                    "{sr:?}"
                );
                assert!(
                    sr.roots.contains(&std::fs::canonicalize(&f.repo).unwrap()),
                    "{sr:?}"
                );
                assert!(sr.wrote_any);
            }
            Determination::Undetermined(w) => panic!("{}", w.as_str()),
        }
    }

    /// Resolve once with everything present so the worktree-origin record for
    /// `wt` is stored.
    fn record_wt(f: &Fixture) {
        assert!(matches!(
            resolve_with_store(&f.repo, &f.transcript, &f.store),
            Determination::Known(_)
        ));
        assert!(matches!(
            crate::worktree_origin::lookup(&f.store, &f.wt.join("src").join("a.rs")).unwrap(),
            crate::worktree_origin::Lookup::Found { .. }
        ));
    }

    // Guard for the 2026-10-29 ruling: only NotFound continues the ancestor walk;
    // a stat that fails with EACCES leaves the gone path's location unknown.
    #[cfg(unix)]
    #[test]
    fn deleted_subdir_of_live_recorded_worktree_unstatable_is_undetermined() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        record_wt(&f);
        std::fs::remove_dir_all(f.wt.join("src")).unwrap();
        std::fs::set_permissions(&f.wt, std::fs::Permissions::from_mode(0o000)).unwrap();
        let pre = std::fs::symlink_metadata(f.wt.join("src"));
        if !matches!(pre, Err(ref e) if e.kind() == std::io::ErrorKind::PermissionDenied) {
            std::fs::set_permissions(&f.wt, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skip: mode 000 does not deny stat here (root?): {pre:?}");
            return;
        }
        let got = resolve_with_store(&f.repo, &f.transcript, &f.store);
        std::fs::set_permissions(&f.wt, std::fs::Permissions::from_mode(0o755)).unwrap();
        let why = undetermined(got);
        assert!(why.contains("a.rs"), "{why}");
    }

    // Guard for the 2026-10-29 ruling: a git failure at the ancestor (corrupt
    // gitfile) is not a root and not excluded.
    #[test]
    fn deleted_subdir_of_live_recorded_worktree_with_broken_gitfile_is_undetermined() {
        let f = fixture();
        record_wt(&f);
        std::fs::remove_dir_all(f.wt.join("src")).unwrap();
        std::fs::write(f.wt.join(".git"), "gitdir: /nonexistent/x\n").unwrap();
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("a.rs"), "{why}");
    }

    // Guard for the 2026-10-29 ruling: unlike the no-record branch, an ancestor
    // of a recorded worktree that git places outside every repo is NOT excluded.
    #[test]
    fn deleted_subdir_of_live_recorded_worktree_outside_any_repo_is_undetermined() {
        let f = fixture();
        record_wt(&f);
        std::fs::remove_dir_all(f.wt.join("src")).unwrap();
        std::fs::remove_file(f.wt.join(".git")).unwrap();
        let o = crate::git_env::scrub(&mut Command::new("git"))
            .arg("-C")
            .arg(&f.wt)
            .args(["rev-parse", "--show-toplevel"])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .env_remove("GIT_DISCOVERY_ACROSS_FILESYSTEM")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(128), "{o:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr)
                .contains("not a git repository (or any of the parent directories)"),
            "{o:?}"
        );
        assert!(f.wt.is_dir());
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("a.rs"), "{why}");
    }

    // Guard for the 2026-10-29 ruling: an ancestor reached through a symlink that
    // points outside the recorded worktree must not resolve to the symlink's
    // target repo (`other`).
    #[cfg(unix)]
    #[test]
    fn deleted_subdir_via_symlink_escaping_live_recorded_worktree_is_undetermined() {
        let f = fixture();
        record_wt(&f);
        let base = f.repo.parent().unwrap().to_path_buf();
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "-q"]);
        git(
            &other,
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
        std::fs::remove_dir_all(f.wt.join("src")).unwrap();
        let link = f.wt.join("link");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        let written = link.join("gone").join("a.rs");
        assert!(!written.parent().unwrap().exists());
        // Precondition: the record covers the written path, and git at the
        // ancestor would name `other` — so only an escape check makes this
        // Undetermined.
        assert!(matches!(
            crate::worktree_origin::lookup(&f.store, &written).unwrap(),
            crate::worktree_origin::Lookup::Found { .. }
        ));
        assert_eq!(
            toplevel(&link).unwrap_or_else(|e| panic!("{e}")),
            std::fs::canonicalize(&other).unwrap()
        );
        let line = serde_json::json!({
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_2", "name": "Write",
                "input": {"file_path": written.to_str().unwrap(), "content": "x"}
            }]}
        });
        std::fs::write(&f.transcript, format!("{line}\n")).unwrap();
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("gone"), "{why}");
    }

    /// Replace the transcript with a single Write of `p`.
    fn write_only(f: &Fixture, p: &Path) {
        let line = serde_json::json!({
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_k", "name": "Write",
                "input": {"file_path": p.to_str().unwrap(), "content": "x"}
            }]}
        });
        std::fs::write(&f.transcript, format!("{line}\n")).unwrap();
    }

    // Kill test for mutant A (a failed ancestor walk inside a live recorded
    // worktree falls back to the worktree root). The worktree root stays
    // readable, so git at the root would succeed; only the walk's EACCES on an
    // ancestor below it keeps this Undetermined.
    #[cfg(unix)]
    #[test]
    fn live_recorded_worktree_walk_failure_below_root_is_undetermined() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        record_wt(&f);
        let src = f.wt.join("src");
        let written = src.join("sub").join("x.rs");
        write_only(&f, &written);
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o000)).unwrap();
        let pre = std::fs::symlink_metadata(src.join("sub"));
        if !matches!(pre, Err(ref e) if e.kind() == std::io::ErrorKind::PermissionDenied) {
            std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skip: mode 000 does not deny stat here (root?): {pre:?}");
            return;
        }
        // Precondition: the record covers the write, and the root itself is
        // still judgeable by git (so a fallback to it would be Known).
        assert!(matches!(
            crate::worktree_origin::lookup(&f.store, &written).unwrap(),
            crate::worktree_origin::Lookup::Found { .. }
        ));
        let root_ok = toplevel(&f.wt).is_ok();
        let got = resolve_with_store(&f.repo, &f.transcript, &f.store);
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(root_ok);
        let why = undetermined(got);
        assert!(why.contains("x.rs"), "{why}");
        assert!(why.contains("cannot stat ancestor"), "{why}");
    }

    // Kill test for mutant B (a canonicalize failure on the found ancestor is
    // swallowed). Reachable without a race: an ancestor reached through short
    // relative symlinks stats fine (the kernel only ever holds the remaining
    // path) while its canonical path exceeds PATH_MAX, so realpath fails with
    // ENAMETOOLONG.
    #[cfg(unix)]
    #[test]
    fn live_recorded_worktree_uncanonicalizable_ancestor_is_undetermined() {
        let f = fixture();
        record_wt(&f);
        let mut access = f.wt.clone();
        // One long component per hop: the kernel's buffer after a hop holds
        // only the link text plus the short remaining tail. Stop once realpath
        // fails, well under the symlink-hop limit (32 on macOS, 40 on Linux).
        let mut k = 0;
        while std::fs::canonicalize(&access).is_ok() {
            assert!(k < 30, "canonical path never exceeded PATH_MAX");
            let long = "a".repeat(250);
            std::fs::create_dir(access.join(&long)).unwrap();
            let s = format!("s{k}");
            std::os::unix::fs::symlink(&long, access.join(&s)).unwrap();
            access = access.join(s);
            k += 1;
        }
        let anc = access.join("d");
        std::fs::create_dir(&anc).unwrap();
        let written = anc.join("gone").join("x.rs");
        // Preconditions: the walk finds `anc` (a real directory), yet it cannot
        // be canonicalized.
        assert!(std::fs::symlink_metadata(&anc).unwrap().is_dir());
        assert!(!written.parent().unwrap().exists());
        assert_eq!(nearest_existing_ancestor(&written).unwrap(), anc);
        let ce = std::fs::canonicalize(&anc).unwrap_err();
        assert!(ce.to_string().contains("too long"), "{ce}");
        assert!(matches!(
            crate::worktree_origin::lookup(&f.store, &written).unwrap(),
            crate::worktree_origin::Lookup::Found { .. }
        ));
        write_only(&f, &written);
        let why = undetermined(resolve_with_store(&f.repo, &f.transcript, &f.store));
        assert!(why.contains("x.rs"), "{why}");
        assert!(why.contains("cannot canonicalize"), "{why}");
    }

    // Kill test for mutant E (the Live arm skips record_if_linked). A linked
    // worktree nested inside the recorded one is not itself recorded; a write
    // into a deleted subdirectory of it resolves through the Live arm, which
    // must record the nested toplevel, as the other arms do.
    #[test]
    fn live_recorded_worktree_arm_records_the_resolved_toplevel() {
        let f = fixture();
        record_wt(&f);
        let nested = f.wt.join("nested");
        git(
            &f.repo,
            &["worktree", "add", "-q", "-b", "n", nested.to_str().unwrap()],
        );
        let written = nested.join("gone").join("x.rs");
        assert!(!written.parent().unwrap().exists());
        write_only(&f, &written);
        // Precondition: before resolving, only the outer worktree covers it.
        match crate::worktree_origin::lookup(&f.store, &written).unwrap() {
            crate::worktree_origin::Lookup::Found { worktree, .. } => {
                assert_eq!(worktree, f.wt)
            }
            other => panic!("expected the outer record: {other:?}"),
        }
        match resolve_with_store(&f.repo, &f.transcript, &f.store) {
            Determination::Known(sr) => {
                assert!(
                    sr.roots.contains(&std::fs::canonicalize(&nested).unwrap()),
                    "{sr:?}"
                );
            }
            Determination::Undetermined(w) => panic!("{}", w.as_str()),
        }
        match crate::worktree_origin::lookup(&f.store, &written).unwrap() {
            crate::worktree_origin::Lookup::Found { worktree, repo } => {
                assert_eq!(worktree, nested);
                assert_eq!(repo, f.repo);
            }
            other => panic!("nested worktree was not recorded: {other:?}"),
        }
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
