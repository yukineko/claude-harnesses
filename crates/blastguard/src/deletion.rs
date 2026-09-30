// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! Recursive `rm` judged by WHAT it destroys (backlog 3aa215e1, slice 1).
//!
//! # The principle (user ruling 2026-09-30, verbatim)
//!
//! > 削除していいがhookのせいで削除できないというのは基本的にまちがっている。
//! > 削除できるか、できないかの問題。削除してはいけないのはシステムの構成、
//! > 資産の破壊を意味するもの。作業や復帰できるものは削除していい
//!
//! *A deletion that is fine but cannot happen because of a hook is basically
//! wrong. The question is only whether it may be deleted. What must not be
//! deleted is whatever means destroying the system's configuration or assets.
//! Work products and anything recoverable may be deleted.* And for paths in a
//! project: 「復帰できるなら許可」 — allowed if it can be restored.
//!
//! So a recursive `rm` is no longer judged by its SHAPE (`-r` = "can delete a
//! whole tree") but by what is at the operand.
//!
//! # Scope
//!
//! Only `rm` with `-r`/`-R`/`--recursive` and LITERAL operands, reached from
//! [`crate::detect`]'s rm arm AFTER protected-path precedence (`.git`,
//! `.git/hooks`, `.claude`, `.githooks`, gate configs stay a flat Deny), AFTER
//! the worktree-storage `Allow` (backlog 873651b9) and AFTER the literal
//! config-file exemption. Wildcards, `find -delete`, `git clean`, `truncate`,
//! redirects and `~`/`$HOME` operands are not judged here and keep their
//! previous verdicts.
//!
//! # Verdicts
//!
//! `Allow`, no confirmation, when EVERY operand is strictly inside (never
//! equal to) one of:
//!
//!  1. a worktree storage root (unchanged, [`crate::scope`]);
//!  2. a temp root: `/tmp`, `/private/tmp`, `$TMPDIR`;
//!  3. a cache root: `$HOME/.cache`, `$HOME/Library/Caches`;
//!  4. build output in a git work tree: the operand is, or is inside, a
//!     directory named one of [`BUILD_OUTPUT_NAMES`] that `git check-ignore`
//!     reports ignored, with no tracked file at or below the operand and every
//!     `git status` entry there `!!` (ignored). Ignored-ness alone is NOT
//!     enough — `.env` is ignored and irreplaceable — so name AND ignored are
//!     both required;
//!  5. content fully recoverable from git: inside a git work tree (not its
//!     root), and `git status --ignored=matching --untracked-files=all` shows
//!     NOTHING at or below the operand (no untracked, no modified / staged /
//!     deleted, no ignored entry), no gitlink (submodule) and no index entry
//!     in an assume-unchanged / skip-worktree / other non-`H` state.
//!
//! `Deny` with a reason naming what would be lost (e.g. "3 untracked") when
//! git positively reports unrecoverable content at or below a project operand
//! — untracked, modified / staged / deleted, or ignored-but-not-build-output.
//! This replaces the confined `Ask` the rm arm used to give such a path.
//!
//! A worktree-storage, temp or cache root ITSELF is refused before any later
//! class is consulted. A git work-tree root is refused by classes 4/5 — but
//! one that lies strictly inside a worktree-storage / temp / cache root (a
//! scratch clone in `/tmp`) is an `Allow` by that earlier class.
//!
//! Everything else — the root of any class above, `$HOME` itself, anything
//! directly at `$HOME/<x>`, system directories, `/var` outside `$TMPDIR`, a
//! git work-tree root — is not an `Allow` from here and falls through to the
//! rm arm's pre-existing logic (the shape `Deny`, or the confined `Ask` for a
//! path inside the session's own safe roots).
//!
//! # 判定不能は拒否側 — cannot determine is never an `Allow`
//!
//! Every one of these withholds the `Allow` and leaves the pre-existing verdict
//! (a `Deny`, or at most the confined `Ask` that hardens to `Deny` headless):
//! a git probe that fails, times out or prints something unparseable; a path
//! that does not exist or cannot be resolved; a root that is not a real
//! directory right now; a symlink anywhere between the root and the operand
//! (the operand included — for the git classes the root is the work-tree
//! root); an operand spelled with a `..` or `.` component; a payload cwd that
//! is not canonical; any `cd` with a relative `rm` operand; any segment of the
//! command that is not a bare `rm` or `cd`; shell expansion, quoting or glob
//! characters; a lone `-` operand, `--`, or an option word after the first
//! operand. The whole-command conditions are checked in [`crate::detect`]
//! (`deletion_rm_eligible`); the per-operand ones here and in
//! [`crate::scope`].
//!
//! A tracked (not ignored) directory named like build output is ALSO not an
//! `Allow` from class 5, even when it is clean: the spec lists
//! `rm -rf target` with `target` tracked as a case that must not reach
//! `Allow`, so a build-output name that git does not report ignored withholds
//! both project classes. This is stricter than the recoverability principle
//! alone would require, deliberately.
//!
//! # Known gaps (NOT closed here)
//!
//!  * cross-call TOCTOU: a DIFFERENT tool call (a background job, a concurrent
//!    session) can change the tree between this judgement and the `rm`;
//!  * state changed by an EARLIER command: something an earlier tool call
//!    moved into a temp/cache root or committed into git is judged as it is
//!    now (`mv ~/Documents /tmp/x` in a previous call, then `rm -rf /tmp/x`
//!    here, is Allow). A SYMLINK placed there instead is not: see above;
//!  * git's own view: files hidden from `git status` by means this probe does
//!    not model (e.g. content-altering clean filters such as LFS, whose bytes
//!    live outside the blob) are judged by what git reports;
//!  * a path that does not exist is not an `Allow` from the git classes (the
//!    probe cannot observe it) even though deleting it destroys nothing.

use harness_core::verdict::Determination;

use crate::exclude;
use crate::reversible::TreeGitFacts;
use crate::scope::{Placement, SafeRoots};

/// Directory names that are well-known build / generated output (spec
/// 3aa215e1 class 4). A name here is necessary, not sufficient: git must also
/// report the directory ignored.
pub const BUILD_OUTPUT_NAMES: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    ".next",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".gradle",
    "coverage",
    ".turbo",
];

/// Absolute prefixes the git classes never judge: system configuration and
/// `/var` (outside the temp roots, which class 2 handles before this is
/// consulted). A path at or below one of these is not an `Allow` from
/// classes 4/5 even if a git work tree happens to contain it.
const NEVER_PROJECT_PREFIXES: &[&str] = &[
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/lib",
    "/lib32",
    "/lib64",
    "/opt",
    "/private/etc",
    "/private/var",
    "/proc",
    "/root",
    "/sbin",
    "/sys",
    "/usr",
    "/var",
    "/Library",
    "/System",
    "/Applications",
];

/// What one operand of a recursive `rm` is, for the deletion principle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperandJudgement {
    /// Strictly inside an allow class; the `&str` names it.
    Allow(&'static str),
    /// Git POSITIVELY reported content here that no copy holds. The string
    /// names what would be lost. Resolves to `Deny`.
    Loss(String),
    /// Not an `Allow` from this module — a root, outside every class, or
    /// undetermined. The rm arm keeps its pre-existing verdict. The string is
    /// the reason, kept for diagnostics.
    NotAllowed(String),
}

/// The aggregate over all operands of one `rm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmJudgement {
    /// Every operand is an `Allow`.
    Allow,
    /// At least one operand would lose unrecoverable content. `Deny`.
    Deny(String),
    /// Anything else: the rm arm's pre-existing logic decides.
    FallThrough,
}

/// Judge every operand; see the module doc for the table.
///
/// Any `Loss` wins (a `Deny` naming it); otherwise `Allow` only when EVERY
/// operand is an `Allow` and there is at least one; otherwise `FallThrough`.
/// An empty operand list is `FallThrough` — "nothing to judge" is not "judged
/// safe".
pub fn judge_rm(scope: &SafeRoots, operands: &[&str], cwd: Option<&str>) -> RmJudgement {
    if operands.is_empty() {
        return RmJudgement::FallThrough;
    }
    let judged: Vec<(&str, OperandJudgement)> = operands
        .iter()
        .map(|o| (*o, judge_operand(scope, o, cwd)))
        .collect();
    let losses: Vec<String> = judged
        .iter()
        .filter_map(|(o, j)| match j {
            OperandJudgement::Loss(what) => Some(format!("`{o}`: {what}")),
            OperandJudgement::Allow(_) | OperandJudgement::NotAllowed(_) => None,
        })
        .collect();
    if !losses.is_empty() {
        return RmJudgement::Deny(format!(
            "recursive rm would destroy content git cannot restore — {}. Commit, stash or move \
it first; blastguard allows deleting only what is recoverable or disposable.",
            losses.join("; ")
        ));
    }
    if judged
        .iter()
        .all(|(_, j)| matches!(j, OperandJudgement::Allow(_)))
    {
        RmJudgement::Allow
    } else {
        RmJudgement::FallThrough
    }
}

/// Judge one operand (literal path; relative ones against `cwd`).
pub fn judge_operand(scope: &SafeRoots, operand: &str, cwd: Option<&str>) -> OperandJudgement {
    if operand.starts_with('-') || exclude::touches_protected(operand) {
        return OperandJudgement::NotAllowed(format!("`{operand}` is not an ordinary operand"));
    }
    if let Some(why) = scope.deletion_refusal() {
        return OperandJudgement::NotAllowed(why.to_string());
    }
    // Class 1 — unchanged worktree storage classification. The storage root
    // ITSELF is refused outright, so no later class (a git work tree that
    // happens to contain the root, say) can turn it into an Allow: the root of
    // ANY class is not deletable from here.
    match scope.classify_worktree(operand, cwd) {
        Determination::Known(Placement::Inside { .. }) => {
            return OperandJudgement::Allow("worktree storage");
        }
        Determination::Known(Placement::IsRoot { root }) => {
            return OperandJudgement::NotAllowed(format!(
                "`{operand}` is the worktree storage root {root} itself"
            ));
        }
        Determination::Known(Placement::Outside { .. }) | Determination::Undetermined(_) => {}
    }
    // Classes 2/3.
    match scope.classify_disposable(operand, cwd) {
        Determination::Known(Placement::Inside { .. }) => {
            return OperandJudgement::Allow("temp or cache");
        }
        Determination::Known(Placement::IsRoot { root }) => {
            return OperandJudgement::NotAllowed(format!("`{operand}` is the root {root} itself"));
        }
        // Symlink below a temp/cache root, unresolvable, … — not class 2/3.
        // An operand that is not under any of them goes on to the git classes.
        // A symlink below a temp/cache root lands here too: the git classes
        // then re-check the path from the work-tree root with the same
        // no-symlink walk, so it cannot become an Allow by that route.
        Determination::Known(Placement::Outside { .. }) | Determination::Undetermined(_) => {}
    }
    judge_project_operand(scope, operand, cwd)
}

/// Classes 4/5: the git work-tree path.
fn judge_project_operand(scope: &SafeRoots, operand: &str, cwd: Option<&str>) -> OperandJudgement {
    let Some(probe) = scope.git_tree_probe() else {
        return OperandJudgement::NotAllowed("no git probe in this session model".to_string());
    };
    let lexical = match scope.lexical_absolute(operand, cwd) {
        Determination::Known(l) => l,
        Determination::Undetermined(_) => {
            return OperandJudgement::NotAllowed(format!("`{operand}` is not a placeable path"));
        }
    };
    let Some(real) = scope.resolve(&lexical) else {
        return OperandJudgement::NotAllowed(format!("`{operand}` could not be resolved"));
    };
    if let Some(why) = never_project(&real, scope.home_real()) {
        return OperandJudgement::NotAllowed(why);
    }
    let candidates = build_candidates(&real);
    let facts = match probe(&real, &candidates) {
        Determination::Known(f) => f,
        Determination::Undetermined(_) => {
            return OperandJudgement::NotAllowed(format!(
                "git could not tell what is at `{operand}`"
            ));
        }
    };
    let Some(top) = scope.resolve(&facts.toplevel) else {
        return OperandJudgement::NotAllowed(
            "git work-tree root could not be resolved".to_string(),
        );
    };
    let no_symlink = scope.no_symlink_below(&lexical, &top);
    decide_project(&real, &top, no_symlink, &candidates, &facts)
}

/// Why `real` is never judged by the git classes, or `None`.
fn never_project(real: &str, home: Option<&str>) -> Option<String> {
    if !real.starts_with('/') || real == "/" {
        return Some("not an absolute path below /".to_string());
    }
    if NEVER_PROJECT_PREFIXES
        .iter()
        .any(|p| real == *p || real.starts_with(&format!("{p}/")))
    {
        return Some(format!("`{real}` is in a system directory"));
    }
    // `$HOME` itself or directly at `$HOME/<x>` (home-level config). An
    // unknown home cannot be ruled out, so it withholds the class entirely.
    let Some(home) = home else {
        return Some("the home directory is unknown".to_string());
    };
    let parent = real.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let parent = if parent.is_empty() { "/" } else { parent };
    if real == home || parent == home {
        return Some(format!("`{real}` is the home directory or directly in it"));
    }
    if home.starts_with(&format!("{}/", real.trim_end_matches('/'))) {
        return Some(format!("`{real}` contains the home directory"));
    }
    None
}

/// Every prefix of `real` whose last component is a build-output name.
pub fn build_candidates(real: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut prefix = String::new();
    for comp in real.split('/').filter(|c| !c.is_empty()) {
        prefix.push('/');
        prefix.push_str(comp);
        if BUILD_OUTPUT_NAMES.contains(&comp) {
            out.push(prefix.clone());
        }
    }
    out
}

/// Pure decision for classes 4/5 — every row is unit-tested.
///
/// * `real` — the operand's canonical path; `top` — the canonical work-tree
///   root; `no_symlink` — [`SafeRoots::no_symlink_below`] from `top` to the
///   operand's spelling; `candidates` — [`build_candidates`] of `real`;
///   `facts` — the probe's observations.
///
/// Order: root/containment/symlink/index-state/submodule refusals
/// (`NotAllowed`), then the build-output name rule (a candidate below `top`
/// that git does not report ignored → `NotAllowed`; all ignored + nothing
/// tracked + only `!!` entries → `Allow`), then recoverability (no status
/// entry → `Allow`, otherwise `Loss` naming the counts).
pub fn decide_project(
    real: &str,
    top: &str,
    no_symlink: Determination<bool>,
    candidates: &[String],
    facts: &TreeGitFacts,
) -> OperandJudgement {
    let top_prefix = format!("{}/", top.trim_end_matches('/'));
    if real == top {
        return OperandJudgement::NotAllowed(format!("`{real}` is the git work-tree root itself"));
    }
    if !real.starts_with(&top_prefix) {
        return OperandJudgement::NotAllowed(format!(
            "`{real}` is not strictly inside the work tree {top}"
        ));
    }
    if top == "/" || NEVER_PROJECT_PREFIXES.contains(&top) {
        return OperandJudgement::NotAllowed(format!("work tree {top} is a system directory"));
    }
    match no_symlink {
        Determination::Known(true) => {}
        Determination::Known(false) | Determination::Undetermined(_) => {
            return OperandJudgement::NotAllowed(format!(
                "a symlink lies between {top} and `{real}` (or the path could not be walked)"
            ));
        }
    }
    if facts.hidden_index_state {
        return OperandJudgement::NotAllowed(
            "an index entry is assume-unchanged / skip-worktree / unmerged, so git status may \
not report its local changes"
                .to_string(),
        );
    }
    if facts.submodule {
        return OperandJudgement::NotAllowed("a submodule lies at or below it".to_string());
    }
    let inside: Vec<&String> = candidates
        .iter()
        .filter(|c| c.starts_with(&top_prefix))
        .collect();
    if !inside.is_empty() {
        let mut all_ignored = true;
        for c in &inside {
            match facts.build_dirs.iter().find(|(p, _)| p == *c) {
                Some((_, true)) => {}
                Some((_, false)) => {
                    return OperandJudgement::NotAllowed(format!(
                        "`{c}` is named like build output but git does not report it ignored \
(it is tracked or not excluded)"
                    ));
                }
                None => all_ignored = false,
            }
        }
        if !all_ignored {
            return OperandJudgement::NotAllowed(
                "git did not answer for every build-output directory".to_string(),
            );
        }
        if facts.tracked == 0 && facts.status.iter().all(|(xy, _)| xy == "!!") {
            return OperandJudgement::Allow("ignored build output");
        }
    }
    if facts.status.is_empty() {
        return OperandJudgement::Allow("fully recoverable from git");
    }
    OperandJudgement::Loss(describe_loss(&facts.status))
}

/// "2 untracked, 1 with uncommitted changes, 1 ignored (e.g. `a`, `b`)".
fn describe_loss(status: &[(String, String)]) -> String {
    let untracked = status.iter().filter(|(xy, _)| xy == "??").count();
    let ignored = status.iter().filter(|(xy, _)| xy == "!!").count();
    let changed = status.len() - untracked - ignored;
    let mut parts = Vec::new();
    if untracked > 0 {
        parts.push(format!("{untracked} untracked"));
    }
    if changed > 0 {
        parts.push(format!("{changed} with uncommitted changes"));
    }
    if ignored > 0 {
        parts.push(format!("{ignored} ignored (not build output)"));
    }
    let examples: Vec<String> = status
        .iter()
        .take(3)
        .map(|(_, p)| format!("`{p}`"))
        .collect();
    format!("{} (e.g. {})", parts.join(", "), examples.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(status: &[(&str, &str)], tracked: usize, build: &[(&str, bool)]) -> TreeGitFacts {
        TreeGitFacts {
            toplevel: "/w/p".to_string(),
            status: status
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            tracked,
            submodule: false,
            hidden_index_state: false,
            build_dirs: build.iter().map(|(a, b)| (a.to_string(), *b)).collect(),
        }
    }

    fn yes() -> Determination<bool> {
        Determination::known(true)
    }

    #[test]
    fn clean_tracked_subtree_is_recoverable_allow() {
        let f = facts(&[], 3, &[]);
        assert_eq!(
            decide_project("/w/p/src", "/w/p", yes(), &[], &f),
            OperandJudgement::Allow("fully recoverable from git")
        );
    }

    #[test]
    fn the_work_tree_root_itself_is_not_allowed() {
        let f = facts(&[], 3, &[]);
        assert!(matches!(
            decide_project("/w/p", "/w/p", yes(), &[], &f),
            OperandJudgement::NotAllowed(_)
        ));
        assert!(matches!(
            decide_project("/w/other", "/w/p", yes(), &[], &f),
            OperandJudgement::NotAllowed(_)
        ));
        assert!(matches!(
            decide_project("/w/px/a", "/w/p", yes(), &[], &f),
            OperandJudgement::NotAllowed(_)
        ));
    }

    #[test]
    fn untracked_modified_and_ignored_content_is_a_named_loss() {
        for (st, word) in [
            (vec![("??", "src/new.rs")], "1 untracked"),
            (vec![(" M", "src/a.rs")], "1 with uncommitted changes"),
            (vec![("A ", "src/b.rs")], "1 with uncommitted changes"),
            (vec![(" D", "src/c.rs")], "1 with uncommitted changes"),
            (vec![("!!", "secrets/")], "1 ignored (not build output)"),
        ] {
            let f = facts(&st, 2, &[]);
            let j = decide_project("/w/p/src", "/w/p", yes(), &[], &f);
            assert!(matches!(j, OperandJudgement::Loss(_)), "{st:?}: {j:?}");
            if let OperandJudgement::Loss(why) = j {
                assert!(why.contains(word), "{why} / {word}");
            }
        }
    }

    #[test]
    fn ignored_build_output_is_allow_but_ignored_non_build_is_loss() {
        let cands = vec!["/w/p/target".to_string()];
        let f = facts(&[("!!", "target/")], 0, &[("/w/p/target", true)]);
        assert_eq!(
            decide_project("/w/p/target", "/w/p", yes(), &cands, &f),
            OperandJudgement::Allow("ignored build output")
        );
        assert_eq!(
            decide_project("/w/p/target/debug", "/w/p", yes(), &cands, &f),
            OperandJudgement::Allow("ignored build output")
        );
        // `.env`-like: ignored, but no build-output name → Loss.
        let f = facts(&[("!!", "secrets/")], 0, &[]);
        assert!(matches!(
            decide_project("/w/p/secrets", "/w/p", yes(), &[], &f),
            OperandJudgement::Loss(_)
        ));
    }

    #[test]
    fn a_tracked_build_named_dir_is_not_allowed_even_when_clean() {
        let cands = vec!["/w/p/target".to_string()];
        let f = facts(&[], 2, &[("/w/p/target", false)]);
        assert!(matches!(
            decide_project("/w/p/target", "/w/p", yes(), &cands, &f),
            OperandJudgement::NotAllowed(_)
        ));
        // A candidate git did not answer for is not assumed ignored.
        let f = facts(&[("!!", "target/")], 0, &[]);
        assert!(matches!(
            decide_project("/w/p/target", "/w/p", yes(), &cands, &f),
            OperandJudgement::NotAllowed(_)
        ));
    }

    #[test]
    fn build_named_dir_with_tracked_files_falls_to_recoverability() {
        let cands = vec!["/w/p/dist".to_string()];
        // Ignored dir, but a force-added tracked file and ignored content:
        // not class 4, and class 5 sees the ignored bytes → Loss.
        let f = facts(&[("!!", "dist/x.js")], 1, &[("/w/p/dist", true)]);
        assert!(matches!(
            decide_project("/w/p/dist", "/w/p", yes(), &cands, &f),
            OperandJudgement::Loss(_)
        ));
    }

    #[test]
    fn symlinks_index_state_and_submodules_withhold_allow() {
        let f = facts(&[], 1, &[]);
        assert!(matches!(
            decide_project("/w/p/src", "/w/p", Determination::known(false), &[], &f),
            OperandJudgement::NotAllowed(_)
        ));
        assert!(matches!(
            decide_project(
                "/w/p/src",
                "/w/p",
                Determination::undetermined("no walk"),
                &[],
                &f
            ),
            OperandJudgement::NotAllowed(_)
        ));
        let mut h = facts(&[], 1, &[]);
        h.hidden_index_state = true;
        assert!(matches!(
            decide_project("/w/p/src", "/w/p", yes(), &[], &h),
            OperandJudgement::NotAllowed(_)
        ));
        let mut s = facts(&[], 1, &[]);
        s.submodule = true;
        assert!(matches!(
            decide_project("/w/p/src", "/w/p", yes(), &[], &s),
            OperandJudgement::NotAllowed(_)
        ));
    }

    #[test]
    fn system_and_home_level_paths_are_never_project_paths() {
        for p in [
            "/etc/x",
            "/usr/local/x",
            "/var/log/x",
            "/private/var/db",
            "/",
        ] {
            assert!(never_project(p, Some("/Users/u")).is_some(), "{p}");
        }
        assert!(never_project("/Users/u", Some("/Users/u")).is_some());
        assert!(never_project("/Users/u/.ssh", Some("/Users/u")).is_some());
        assert!(never_project("/Users", Some("/Users/u")).is_some());
        assert!(never_project("/Users/u/src/p/x", None).is_some());
        assert!(never_project("/Users/u/src/p/x", Some("/Users/u")).is_none());
    }

    #[test]
    fn build_candidates_are_every_build_named_prefix() {
        assert_eq!(
            build_candidates("/w/p/target/debug/build/x"),
            vec![
                "/w/p/target".to_string(),
                "/w/p/target/debug/build".to_string()
            ]
        );
        assert!(build_candidates("/w/p/src").is_empty());
    }

    #[test]
    fn describe_loss_counts_each_kind() {
        let st = vec![
            ("??".to_string(), "a".to_string()),
            ("??".to_string(), "b".to_string()),
            (" M".to_string(), "c".to_string()),
            ("!!".to_string(), "d/".to_string()),
        ];
        let d = describe_loss(&st);
        assert!(d.starts_with("2 untracked, 1 with uncommitted changes, 1 ignored"));
        assert!(d.contains("`a`, `b`, `c`"));
    }
}
