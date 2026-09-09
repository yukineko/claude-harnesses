//! Which of a working tree's changed files a gate may **stop looking at**.
//!
//! A working tree carries no session identity. `git diff --name-only` in a
//! checkout two sessions share answers for BOTH of them, so a gate that reads
//! it presents a peer's edits as yours and blocks you on them (backlog
//! `1e44bfd9`, observed 2026-07-21 session c6a1fdbf and 2026-07-23 session
//! 143f3d21). That is not merely noisy: the shortest way out of a block you did
//! not earn is to create a skip marker, and under parallel sessions that is the
//! shared one-shot hatch CLAUDE.md 第5節 forbids. The gate's own pressure pushes
//! the operator toward disabling the gate.
//!
//! The session's **transcript** is the artefact that does carry identity. The
//! obvious rule — keep the intersection of "changed" and "what my transcript
//! records me editing" — is WRONG, and this module exists to not implement it.
//!
//! # The measurement
//!
//! [`crate::transcript::files_edited_by_session`] collects paths from `Edit` /
//! `Write` / `MultiEdit` / `NotebookEdit` tool_use blocks. Run over this
//! repository's own session `e9ebfcb6` on 2026-09-09 — a 12.6 MB transcript,
//! 90 recorded paths — against the working tree that same session produced:
//!
//! ```text
//! working tree changed: 10 path(s)
//!   IN footprint  crates/donegate/src/main.rs
//!   ...
//!   MISSING       crates/harness-core/src/lib.rs
//!   MISSING       crates/harness-core/tests/gate_giveup_concession.rs
//! ```
//!
//! Both MISSING files were written by that very session. `lib.rs` was edited
//! with `sed -i`, which is a `Bash` tool_use and leaves no edit block at all;
//! the test file was authored by a subagent, whose blocks live in its own
//! sidechain transcript. So the footprint is a strict UNDER-approximation:
//!
//! ```text
//! f in my footprint      ==> f IS mine     (sound)
//! f not in my footprint  ==> UNKNOWN       (NOT "f is not mine")
//! ```
//!
//! Intersecting would read the second line as the third, and drop shell-driven
//! and subagent-authored edits from a gate that blocks on hard-coded secrets
//! and missing tests — a fail-open introduced by the fix itself, which is the
//! class 第3節 exists to forbid.
//!
//! # The rule
//!
//! An exclusion must be a **positive observation**, never the absence of one. A
//! file leaves the working set only when some *other* session's footprint
//! claims it and this session's does not. A file no transcript claims is
//! unattributed, and unattributed stays in.
//!
//! [`crate::transcript::files_edited_by_session_and_subagents`] narrows the gap
//! for subagents (that half is observable); nothing can close the `sed -i` half,
//! which is why the rule above is structural rather than a stopgap.
//!
//! # Residual hole — stated because a limit nobody is told about is a lie
//!
//! One case is still permissive and this module cannot fix it: a file **I**
//! edited through the shell and a **peer** edited through `Edit` is absent from
//! my footprint and present in theirs, so it is excluded from my gate even
//! though I wrote to it. Both halves of that coincidence are needed, and under
//! CLAUDE.md 第8節 (one worktree per session) it cannot arise at all — but 第8節
//! is exactly the invariant already broken whenever this module matters, so the
//! hole is real rather than theoretical. Closing it would need shell-driven
//! writes to be observable, which would mean interpreting arbitrary `Bash`
//! command lines; a heuristic there would fail permissively in a *new* way. The
//! honest state is: narrowed, bounded, and disclosed — never silently widened.
//!
//! Lives here rather than in one gate because two gates need it —
//! `reviewgate` (Stop hook, which receives `transcript_path` directly) and
//! `precommit-audit` (a pre-commit hook, which receives only
//! `CLAUDE_CODE_SESSION_ID` and must locate the transcript itself). Two copies
//! of this rule would drift, and the direction they drift in is permissive.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::verdict::Determination;

/// What a gate should do with each file a working-tree scan returned.
#[derive(Debug)]
#[must_use]
pub enum Attribution {
    /// This session's footprint was read.
    ///
    /// `keep` is what the gate must still act on: files this session is known
    /// to have edited **plus** files no transcript claims at all. `excluded` is
    /// the narrow set positively attributed to a different session, kept as a
    /// list so the exclusion can be NAMED rather than silently applied — an
    /// exclusion nobody is told about is the same defect as an inclusion nobody
    /// is told about.
    Narrowed {
        keep: Vec<String>,
        excluded: Vec<String>,
    },
    /// This session's own footprint could not be read. **Do not narrow.**
    /// Report the reason and keep the full set.
    Undetermined { why: String },
}

impl Attribution {
    /// The file list the caller should act on: `keep` when attribution
    /// succeeded, and — deliberately — the FULL set when it did not.
    pub fn files<'a>(&'a self, full: &'a [String]) -> &'a [String] {
        match self {
            Attribution::Narrowed { keep, .. } => keep,
            Attribution::Undetermined { .. } => full,
        }
    }
}

/// Resolve `p` as far as the filesystem allows, so an absolute path recorded by
/// the transcript and a repo-relative path from `git` compare equal.
///
/// `canonicalize` also resolves the symlinked temp dirs some hosts use, which
/// is why it is applied to BOTH sides rather than just joining the root. A
/// deleted file cannot be canonicalised; falling back to the unresolved path is
/// correct there — worst case the two spellings differ, the file matches
/// neither footprint, and it stays in `keep`, which is the restrictive
/// direction.
pub fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn resolved(paths: &BTreeSet<String>) -> BTreeSet<PathBuf> {
    paths.iter().map(|p| resolve(Path::new(p))).collect()
}

/// Split `changed` (repo-relative paths under `root`) using already-computed
/// footprints. Pure apart from `canonicalize`, so the rule itself is testable
/// without a transcript on disk.
///
/// `peers` is deliberately a plain set rather than a `Determination`: see
/// [`crate::transcript::peer_edit_footprint_in`] for why a failure to read peer
/// transcripts is already the restrictive answer.
pub fn attribute_with_footprints(
    root: &Path,
    mine: Determination<BTreeSet<String>>,
    peers: &BTreeSet<String>,
    changed: &[String],
) -> Attribution {
    let mine = match mine {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return Attribution::Undetermined {
                why: why.as_str().to_string(),
            }
        }
    };
    let mine = resolved(&mine);
    let peers = resolved(peers);

    let mut keep = Vec::new();
    let mut excluded = Vec::new();
    for c in changed {
        let abs = resolve(&root.join(c));
        // Order matters: my own evidence wins over a peer's. An overlap means
        // we both touched the file, and a file I touched is never someone
        // else's problem to review.
        if mine.contains(&abs) {
            keep.push(c.clone());
        } else if peers.contains(&abs) {
            excluded.push(c.clone());
        } else {
            // Unattributed. NOT mine-by-absence: see the module docstring's
            // measurement — `sed -i` edits appear in no footprint at all.
            keep.push(c.clone());
        }
    }
    Attribution::Narrowed { keep, excluded }
}

/// Split `changed` by reading the transcript at `transcript_path` — the Stop
/// hook's entry point, where the payload names the file directly.
///
/// Peers are looked for in the transcript's own projects tree
/// (`<transcript>/../..`), so the Stop hook needs no extra configuration to
/// find them — but only when that grandparent really is a `projects` directory.
/// Handed a transcript from anywhere else, the peer set would become whatever
/// `.jsonl` files happen to sit two levels up, and a WRONG peer set produces
/// FALSE EXCLUSIONS: files dropped from the gate on the strength of a stranger's
/// footprint. That failure is permissive, so the shape is checked rather than
/// assumed, and a mismatch yields no peers at all.
pub fn attribute_from_transcript(
    root: &Path,
    transcript_path: &str,
    changed: &[String],
) -> Attribution {
    let mine = crate::transcript::files_edited_by_session_and_subagents(transcript_path);
    // Compute peers only if they can still matter. On the `Undetermined` arm
    // the result is discarded, and building it first meant walking the whole
    // transcript store — measured at 93 MB — to throw the answer away on every
    // Stop hook and every `git commit`.
    if matches!(mine, Determination::Undetermined(_)) {
        return attribute_with_footprints(root, mine, &BTreeSet::new(), changed);
    }
    let path = Path::new(transcript_path);
    let my_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let peers = match path.parent().and_then(Path::parent) {
        Some(projects) if projects.file_name().is_some_and(|n| n == "projects") => {
            crate::transcript::peer_edit_footprint_in(projects, &my_id)
        }
        _ => BTreeSet::new(),
    };
    attribute_with_footprints(root, mine, &peers, changed)
}

/// Split `changed` for a caller that knows only a **session id** — a pre-commit
/// hook, which gets no Stop payload and so must locate the transcript itself.
///
/// Every failure on the way (no session id, no `$HOME`, no transcript file) is
/// `Undetermined`, never an empty footprint: an empty footprint plus a peer set
/// would exclude files this session actually wrote.
pub fn attribute_from_session(root: &Path, session_id: &str, changed: &[String]) -> Attribution {
    match crate::transcript::locate_session_transcript(session_id) {
        Determination::Known(p) => attribute_from_transcript(root, &p.to_string_lossy(), changed),
        Determination::Undetermined(why) => Attribution::Undetermined {
            why: why.as_str().to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|s| (*s).to_string()).collect()
    }

    fn known(paths: &[&str]) -> Determination<BTreeSet<String>> {
        Determination::Known(set(paths))
    }

    #[test]
    fn an_unreadable_transcript_does_not_narrow_anything() {
        let root = Path::new("/nonexistent-root");
        let changed = vec!["a.rs".to_string(), "b.rs".to_string()];
        let a = attribute_with_footprints(
            root,
            Determination::undetermined("transcript unreadable"),
            &set(&[]),
            &changed,
        );
        match &a {
            Attribution::Undetermined { why } => assert!(why.contains("unreadable"), "{why}"),
            other => panic!("must not narrow on an unreadable transcript, got {other:?}"),
        }
        assert_eq!(
            a.files(&changed),
            changed.as_slice(),
            "判定不能 must keep the FULL set: narrowing here would turn `cannot tell whose \
             these are` into `none of these are mine`, silently switching the gate off"
        );
    }

    #[test]
    fn a_file_no_transcript_claims_is_kept_not_excluded() {
        let dir = tempdir("unattributed");
        let changed = vec!["a.rs".to_string(), "b.rs".to_string()];
        let a = attribute_with_footprints(&dir, known(&[]), &set(&[]), &changed);
        match a {
            Attribution::Narrowed { keep, excluded } => {
                assert_eq!(
                    keep, changed,
                    "an empty footprint on both sides means UNATTRIBUTED, not `nobody's`. \
                     Measured: a `sed -i` edit and a subagent-authored file appear in no \
                     footprint at all, so excluding here drops real edits from a gate that \
                     blocks on secrets and missing tests"
                );
                assert!(
                    excluded.is_empty(),
                    "an exclusion requires positive evidence of a peer, and there is none here"
                );
            }
            other => panic!("a READ transcript must narrow, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_peers_own_files_are_excluded() {
        let dir = tempdir("split");
        for f in ["mine.rs", "theirs.rs", "nobodys.rs", "both.rs"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let abs = |f: &str| resolve(&dir.join(f)).to_string_lossy().into_owned();
        let mine = abs("mine.rs");
        let both_m = abs("both.rs");
        let theirs = abs("theirs.rs");
        let both_p = abs("both.rs");
        let changed = vec![
            "mine.rs".to_string(),
            "theirs.rs".to_string(),
            "nobodys.rs".to_string(),
            "both.rs".to_string(),
        ];
        let a = attribute_with_footprints(
            &dir,
            known(&[&mine, &both_m]),
            &set(&[&theirs, &both_p]),
            &changed,
        );
        match a {
            Attribution::Narrowed { keep, excluded } => {
                assert_eq!(
                    excluded,
                    vec!["theirs.rs".to_string()],
                    "only a file a PEER claims and I do not may be excluded"
                );
                assert_eq!(
                    keep,
                    vec![
                        "mine.rs".to_string(),
                        "nobodys.rs".to_string(),
                        "both.rs".to_string()
                    ],
                    "`nobodys.rs` is unattributed and `both.rs` is an overlap; both stay in"
                );
            }
            other => panic!("expected a narrowed split, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_session_id_is_undetermined_not_an_empty_footprint() {
        let changed = vec!["a.rs".to_string()];
        let a = attribute_from_session(Path::new("/tmp"), "", &changed);
        assert!(
            matches!(a, Attribution::Undetermined { .. }),
            "no session id means the footprint was NOT observed; an empty footprint alongside \
             a non-empty peer set would exclude files this session actually wrote"
        );
        assert_eq!(a.files(&changed), changed.as_slice());
    }

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hc-attr-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
