//! S6-2 RED tests for [`super::classify`]: the real Table C `PathClass`
//! (SUBSET.md "Table C — PathClass × Effect → verdict"), first match wins in
//! the order System, Protected, Workflow, Persistence, Scratch, OwnWorktree,
//! OtherWorktree, Project, Other; Unresolved when nothing could be placed.
//!
//! Written by an agent that did not write (and will not write) `classify`
//! (CLAUDE.md §2(a)). Each `#[ignore = "RED S6-2 ..."]` test was observed
//! failing against the S1 stub (which answers `Unresolved` for everything)
//! before the implementation; the un-ignored ones are controls the stub's
//! restrictive answer already satisfies (they pin that the real `classify`
//! keeps them restrictive).
//!
//! Hermetic: every path lives in a fresh fixture under a neutral parent
//! (`scope::test_fixture_roots::neutral_fixture_parent`: outside every fixed
//! temp root and every git work tree), except the Scratch fixtures, which by
//! definition live under the canonical fixed temp root (`/private/tmp` on
//! macOS), each in its own `tempfile` directory. The location model is built
//! with an explicit `HOME` (the fixture's), no `TMPDIR`, and the same
//! tail-re-attaching resolver the hook binary uses (`main.rs::real_path`).

use super::*;
use crate::scope::test_fixture_roots::{
    canonical_fixed_temp_root, neutral_fixture_candidates, neutral_fixture_parent,
};
use std::path::{Path, PathBuf};

const STUB: &str = "RED S6-2 2026-11-20: classify is the S1 stub (always Unresolved)";

/// `main.rs::real_path`: canonicalise the longest existing prefix and
/// re-attach the missing tail by name.
fn real_path(path: &str) -> Option<String> {
    let mut cur = PathBuf::from(path);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out.to_str().map(str::to_string);
        }
        let name = cur.file_name()?.to_os_string();
        if !cur.pop() {
            return None;
        }
        tail.push(name);
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn lit(p: &str) -> Value {
    Value::Literal(p.to_string())
}

/// `<base>/home` (HOME), `<base>/src/proj` (a main checkout: `.git` DIR),
/// `<base>/src/.harness-worktrees/{wt-a,wt-b}` (linked checkouts: `.git`
/// FILE starting `gitdir:`), `<base>/other`.
struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let parent = neutral_fixture_parent(&neutral_fixture_candidates());
        let dir = tempfile::Builder::new()
            .prefix("bg-s6-2-classify-")
            .tempdir_in(parent)
            .unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let mk = |rel: &str| std::fs::create_dir_all(base.join(rel)).unwrap();
        let touch = |rel: &str| std::fs::write(base.join(rel), b"x").unwrap();
        mk("home");
        touch("home/notes.txt");
        mk("home/.ssh");
        touch("home/.ssh/id_ed25519");
        mk("src/proj/.git");
        mk("src/proj/src");
        touch("src/proj/src/a.rs");
        mk("src/proj/.githooks");
        touch("src/proj/.githooks/pre-commit");
        mk("src/proj/.github/workflows");
        touch("src/proj/.github/workflows/ci.yml");
        for wt in ["wt-a", "wt-b"] {
            mk(&format!("src/.harness-worktrees/{wt}/src"));
            std::fs::write(
                base.join(format!("src/.harness-worktrees/{wt}/.git")),
                format!("gitdir: {}/src/proj/.git/worktrees/{wt}\n", s(&base)),
            )
            .unwrap();
            touch(&format!("src/.harness-worktrees/{wt}/src/lib.rs"));
            mk(&format!("src/.harness-worktrees/{wt}/.githooks"));
            touch(&format!("src/.harness-worktrees/{wt}/.githooks/pre-commit"));
        }
        mk("other/Library/LaunchAgents");
        Fx { _dir: dir, base }
    }

    fn p(&self, rel: &str) -> String {
        s(&self.base.join(rel))
    }

    fn home(&self) -> String {
        self.p("home")
    }

    fn proj(&self) -> String {
        self.p("src/proj")
    }

    fn wt(&self, name: &str) -> String {
        self.p(&format!("src/.harness-worktrees/{name}"))
    }

    /// Location model for a session whose payload cwd and
    /// `CLAUDE_PROJECT_DIR` are `cwd`.
    fn scope_at(&self, cwd: &str) -> SafeRoots {
        SafeRoots::new(
            Some(cwd),
            Some(cwd),
            Some(&self.home()),
            None,
            Some(real_path),
        )
    }
}

fn classify_at(
    scope: &SafeRoots,
    hook_cwd: Option<&str>,
    effect: Effect,
    target: &Value,
) -> PathClass {
    let ctx = PathCtx {
        scope,
        hook_cwd,
        legacy: None,
    };
    let cwd = match hook_cwd {
        Some(c) => lit(c),
        None => Value::Undetermined("no hook cwd in this test".into()),
    };
    classify(effect, target, &cwd, &ctx)
}

/// Classify `target` (absolute) in a session at `cwd`, hook cwd = `cwd`.
fn class_in(fx: &Fx, cwd: &str, effect: Effect, target: &str) -> PathClass {
    let scope = fx.scope_at(cwd);
    classify_at(&scope, Some(cwd), effect, &lit(target))
}

const DEL_R: Effect = Effect::Delete { recursive: true };
const TRUNC: Effect = Effect::Truncate;

/// A fresh directory strictly inside the canonical fixed temp root
/// (`/private/tmp/bg-s6-2-scratch-XXXX` on macOS).
fn scratch_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("bg-s6-2-scratch-")
        .tempdir_in(canonical_fixed_temp_root())
        .unwrap();
    let p = std::fs::canonicalize(dir.path()).unwrap();
    (dir, p)
}

// ---------------------------------------------------------------- fixture preconditions

/// Controls on the FIXTURE, not on `classify`: the existing predicates the
/// Table C classes are built from answer what each test below assumes. If one
/// of these fails, the RED tests below would be pinning the wrong fixture.
#[test]
fn fixture_predicates_answer_as_assumed() {
    use crate::scope::Placement;
    use harness_core::verdict::Determination;
    let fx = Fx::new();
    let wt_a = fx.wt("wt-a");
    let own = fx.scope_at(&wt_a);
    let in_a = format!("{wt_a}/src/lib.rs");
    assert!(
        matches!(own.classify_worktree_checkout_value(&in_a), Determination::Known(Placement::Inside { ref root, .. }) if *root == wt_a),
        "wt-a file must be Inside the wt-a linked checkout"
    );
    let in_b = format!("{}/src/lib.rs", fx.wt("wt-b"));
    assert!(
        matches!(own.classify_worktree_checkout_value(&in_b), Determination::Known(Placement::Inside { ref root, .. }) if *root == fx.wt("wt-b")),
        "wt-b file must be Inside the wt-b linked checkout"
    );
    assert!(
        matches!(
            own.classify_worktree(&in_b, None),
            Determination::Known(Placement::Inside { .. })
        ),
        "wt-b file must be Inside a worktree storage root"
    );
    let main = fx.scope_at(&fx.proj());
    assert!(matches!(
        main.classify(&fx.p("src/proj/src/a.rs"), None),
        Determination::Known(Placement::Inside { .. })
    ));
    assert!(matches!(
        main.classify(&fx.p("home/notes.txt"), None),
        Determination::Known(Placement::Outside { .. })
    ));
    assert!(matches!(
        main.classify_disposable_value(&fx.p("home/notes.txt")),
        Determination::Known(Placement::Outside { .. })
    ));
    assert!(!matches!(
        main.classify_worktree(&fx.p("home/notes.txt"), None),
        Determination::Known(Placement::Inside { .. })
    ));
    let (_keep, sdir) = scratch_dir();
    let f = sdir.join("f.txt");
    std::fs::write(&f, b"x").unwrap();
    assert!(matches!(
        main.classify_disposable_value(&s(&f)),
        Determination::Known(Placement::Inside { .. })
    ));
    assert!(matches!(
        main.classify_disposable_value(&s(&canonical_fixed_temp_root())),
        Determination::Known(Placement::IsRoot { .. })
    ));
    assert!(crate::exclude::is_protected_path(
        &fx.p("src/proj/.githooks/pre-commit")
    ));
    assert!(!crate::exclude::is_protected_path(
        &fx.p("src/proj/.github/workflows/ci.yml")
    ));
    assert!(crate::exclude::is_workflow_path(
        &fx.p("src/proj/.github/workflows/ci.yml")
    ));
}

// ---------------------------------------------------------------- System

#[test]
fn system_etc_file_is_system() {
    let fx = Fx::new();
    let c = class_in(&fx, &fx.proj(), TRUNC, "/etc/hosts");
    assert_eq!(c, PathClass::System, "{STUB}");
}

#[test]
fn system_root_is_system() {
    let fx = Fx::new();
    let c = class_in(&fx, &fx.proj(), DEL_R, "/");
    assert_eq!(c, PathClass::System, "{STUB}");
}

#[test]
fn system_home_itself_is_system() {
    let fx = Fx::new();
    let home = fx.home();
    let c = class_in(&fx, &fx.proj(), DEL_R, &home);
    assert_eq!(c, PathClass::System, "{STUB}");
}

#[test]
fn system_home_ssh_asset_is_system() {
    let fx = Fx::new();
    let t = fx.p("home/.ssh/id_ed25519");
    let c = class_in(&fx, &fx.proj(), TRUNC, &t);
    assert_eq!(c, PathClass::System, "{STUB}");
}

/// A spelling under Scratch whose symlink lands in `/etc` is System (System
/// is judged on the symlink-resolved spelling too, and precedes Scratch).
#[test]
fn system_scratch_symlink_into_etc_is_system() {
    let fx = Fx::new();
    let (_keep, sdir) = scratch_dir();
    let link = sdir.join("etc-link");
    std::os::unix::fs::symlink("/etc", &link).unwrap();
    let t = format!("{}/hosts", s(&link));
    let c = class_in(&fx, &fx.proj(), TRUNC, &t);
    assert_eq!(c, PathClass::System, "{STUB}; target {t}");
}

/// judge.rs `PathClass::System` doc (judge.rs:124-125): "A system path
/// strictly inside a safe root (`Ctx::confined_root`, a project under `/opt`)
/// is not `System`." A session whose project is `/opt/<x>/proj` owns
/// `/opt/<x>/proj/src/a.rs` (safe root `Inside`), so it is Project. Nothing
/// is created under `/opt`: the resolver re-attaches the missing tail to the
/// real `/opt`, so the root and the target are placed without writing there.
#[test]
fn system_path_strictly_inside_safe_root_is_not_system() {
    use crate::scope::Placement;
    use harness_core::verdict::Determination;
    let fx = Fx::new();
    let proj = format!("/opt/bg-s6-2-zqxv-{}/proj", std::process::id());
    assert!(!Path::new(&proj).exists(), "fixture: {proj} must not exist");
    let scope = fx.scope_at(&proj);
    let t = format!("{proj}/src/a.rs");
    assert!(
        crate::scope::is_inside_system_dir(&t),
        "fixture: {t} is a system path"
    );
    assert!(
        matches!(scope.classify(&t, None), Determination::Known(Placement::Inside { ref root, .. }) if *root == proj),
        "fixture: {t} must be strictly Inside the safe root {proj}"
    );
    let c = classify_at(&scope, Some(&proj), TRUNC, &lit(&t));
    assert_ne!(c, PathClass::System, "{STUB}");
    assert_eq!(c, PathClass::Project, "{STUB}");
}

// ---------------------------------------------------------------- Protected

/// `.githooks/**` is a protected path (`exclude.rs` PROTECTED_GLOBS:
/// `".githooks"`, `".githooks/**"`, `"**/.githooks"`, `"**/.githooks/**"`),
/// and Protected precedes Workflow / Project in Table C, so a `.githooks`
/// file in the main project tree is Protected (SUBSET.md Table C row
/// "**Protected** (`exclude::is_protected_path`, ...)" and deny-target row
/// "Q4 `.githooks`").
#[test]
fn protected_githooks_file_in_main_tree_is_protected() {
    let fx = Fx::new();
    let t = fx.p("src/proj/.githooks/pre-commit");
    let c = class_in(&fx, &fx.proj(), TRUNC, &t);
    assert_eq!(c, PathClass::Protected, "{STUB}");
}

/// Relative spelling, placed against the hook cwd.
#[test]
fn protected_relative_githooks_is_protected() {
    let fx = Fx::new();
    let scope = fx.scope_at(&fx.proj());
    let c = classify_at(
        &scope,
        Some(&fx.proj()),
        TRUNC,
        &lit(".githooks/pre-commit"),
    );
    assert_eq!(c, PathClass::Protected, "{STUB}");
}

/// Protected is lifted strictly inside a linked worktree checkout
/// (judge.rs `PathClass::Protected` doc: "Lifted (falls through to the next
/// class) strictly inside a linked worktree checkout:
/// [`SafeRoots::classify_worktree_checkout`] `Inside`"; SUBSET.md Table C
/// Protected row "lifted inside `classify_worktree_checkout` Inside"), so a
/// `.githooks` file in the session's own linked checkout falls through to
/// OwnWorktree.
#[test]
fn githooks_inside_own_linked_checkout_is_not_protected() {
    use crate::scope::Placement;
    use harness_core::verdict::Determination;
    let fx = Fx::new();
    let wt = fx.wt("wt-a");
    let t = format!("{wt}/.githooks/pre-commit");
    let scope = fx.scope_at(&wt);
    assert!(
        matches!(scope.classify_worktree_checkout_value(&t), Determination::Known(Placement::Inside { ref root, .. }) if *root == wt),
        "fixture: {t} must be checkout-Inside wt-a"
    );
    assert!(
        crate::exclude::is_protected_path(&t),
        "fixture: {t} is protected-named"
    );
    let c = classify_at(&scope, Some(&wt), TRUNC, &lit(&t));
    assert_ne!(c, PathClass::Protected, "{STUB}");
    assert_eq!(c, PathClass::OwnWorktree, "{STUB}");
}

// ---------------------------------------------------------------- Workflow

#[test]
fn workflow_file_is_workflow() {
    let fx = Fx::new();
    let t = fx.p("src/proj/.github/workflows/ci.yml");
    let c = class_in(&fx, &fx.proj(), TRUNC, &t);
    assert_eq!(c, PathClass::Workflow, "{STUB}");
}

// ---------------------------------------------------------------- Persistence

/// A NEW file in a `Library/LaunchAgents` directory under a Truncate effect
/// (`launch_agents_new_file_ask`, new file only).
#[test]
fn persistence_new_launch_agent_is_persistence() {
    let fx = Fx::new();
    let t = fx.p("other/Library/LaunchAgents/com.example.zqxv.plist");
    assert!(!Path::new(&t).exists());
    let c = class_in(&fx, &fx.proj(), TRUNC, &t);
    assert_eq!(c, PathClass::Persistence, "{STUB}");
}

// ---------------------------------------------------------------- Scratch

#[test]
fn scratch_file_inside_tmp_is_scratch() {
    let fx = Fx::new();
    let (_keep, sdir) = scratch_dir();
    let f = sdir.join("f.txt");
    std::fs::write(&f, b"x").unwrap();
    let c = class_in(&fx, &fx.proj(), DEL_R, &s(&f));
    assert_eq!(c, PathClass::Scratch, "{STUB}");
}

/// The scratch ROOT itself is not Scratch (`IsRoot` falls through to
/// Project / Other: SUBSET.md Table C notes, "`IsRoot` of a Scratch ...
/// root (`rm -rf /tmp`) is NOT Scratch"; user ruling 2026-11-20 "/tmp root
/// itself is not Scratch").
#[test]
fn scratch_root_tmp_itself_is_not_scratch() {
    let fx = Fx::new();
    for t in ["/tmp", &s(&canonical_fixed_temp_root())] {
        let c = class_in(&fx, &fx.proj(), DEL_R, t);
        assert!(
            matches!(c, PathClass::Project | PathClass::Other),
            "{STUB}; target {t} classified {c:?}"
        );
    }
}

/// Control: a `..` spelling that leaves scratch is never Scratch; per the
/// `PathClass::Unresolved` doc ("a `..` component") it is Unresolved.
#[test]
fn scratch_dotdot_escape_is_not_scratch() {
    let fx = Fx::new();
    let (_keep, sdir) = scratch_dir();
    let depth = sdir.components().count();
    let up = "../".repeat(depth);
    let t = format!(
        "{}/{}{}",
        s(&sdir),
        up,
        fx.p("home/notes.txt").trim_start_matches('/')
    );
    let c = class_in(&fx, &fx.proj(), DEL_R, &t);
    assert_ne!(c, PathClass::Scratch, "target {t}");
    assert!(
        matches!(c, PathClass::Unresolved(_)),
        "target {t} classified {c:?}"
    );
}

// ---------------------------------------------------------------- OwnWorktree

#[test]
fn own_worktree_file_is_own_worktree() {
    let fx = Fx::new();
    let wt = fx.wt("wt-a");
    let t = format!("{wt}/src/lib.rs");
    let c = class_in(&fx, &wt, DEL_R, &t);
    assert_eq!(c, PathClass::OwnWorktree, "{STUB}");
}

#[test]
fn own_worktree_relative_file_is_own_worktree() {
    let fx = Fx::new();
    let wt = fx.wt("wt-a");
    let scope = fx.scope_at(&wt);
    let c = classify_at(&scope, Some(&wt), DEL_R, &lit("src/lib.rs"));
    assert_eq!(c, PathClass::OwnWorktree, "{STUB}");
}

/// No session cwd and no hook cwd: the checkout cannot be "the one that
/// contains the session cwd", so never OwnWorktree; the storage root is
/// still derived from `CLAUDE_PROJECT_DIR`, so the target is OtherWorktree.
#[test]
fn no_cwd_is_never_own_worktree() {
    let fx = Fx::new();
    let wt = fx.wt("wt-a");
    let scope = SafeRoots::new(None, Some(&wt), Some(&fx.home()), None, Some(real_path));
    let c = classify_at(&scope, None, DEL_R, &lit(&format!("{wt}/src/lib.rs")));
    assert_ne!(c, PathClass::OwnWorktree);
    assert_eq!(c, PathClass::OtherWorktree, "{STUB}");
}

/// OwnWorktree is defined by [`SafeRoots::session_cwd`], not by
/// `PathCtx::hook_cwd` (SUBSET.md Table C notes: "the session cwd
/// (`SafeRoots::session_cwd`) is `R` or inside `R`"; judge.rs
/// `PathClass::OwnWorktree` doc). With the session cwd in wt-a and no hook
/// cwd, an absolute target in wt-a is still OwnWorktree.
#[test]
fn own_worktree_by_session_cwd_without_hook_cwd() {
    let fx = Fx::new();
    let wt = fx.wt("wt-a");
    let scope = fx.scope_at(&wt);
    assert_eq!(
        scope.session_cwd(),
        Some(wt.as_str()),
        "fixture: session cwd"
    );
    let c = classify_at(&scope, None, DEL_R, &lit(&format!("{wt}/src/lib.rs")));
    assert_eq!(c, PathClass::OwnWorktree, "{STUB}");
}

// ---------------------------------------------------------------- OtherWorktree

#[test]
fn sibling_checkout_is_other_worktree() {
    let fx = Fx::new();
    let t = format!("{}/src/lib.rs", fx.wt("wt-b"));
    let c = class_in(&fx, &fx.wt("wt-a"), DEL_R, &t);
    assert_eq!(c, PathClass::OtherWorktree, "{STUB}");
}

// ---------------------------------------------------------------- Project

#[test]
fn main_tree_file_is_project() {
    let fx = Fx::new();
    let t = fx.p("src/proj/src/a.rs");
    let c = class_in(&fx, &fx.proj(), DEL_R, &t);
    assert_eq!(c, PathClass::Project, "{STUB}");
}

#[test]
fn relative_main_tree_file_is_project() {
    let fx = Fx::new();
    let scope = fx.scope_at(&fx.proj());
    let c = classify_at(&scope, Some(&fx.proj()), DEL_R, &lit("src/a.rs"));
    assert_eq!(c, PathClass::Project, "{STUB}");
}

// ---------------------------------------------------------------- Other

#[test]
fn home_plain_file_is_other() {
    let fx = Fx::new();
    let t = fx.p("home/notes.txt");
    let c = class_in(&fx, &fx.proj(), TRUNC, &t);
    assert_eq!(c, PathClass::Other, "{STUB}");
}

// ---------------------------------------------------------------- Unresolved (controls)

#[test]
fn undetermined_target_is_unresolved() {
    let fx = Fx::new();
    let scope = fx.scope_at(&fx.proj());
    let c = classify_at(
        &scope,
        Some(&fx.proj()),
        DEL_R,
        &Value::Undetermined("unbound $X".into()),
    );
    assert!(matches!(c, PathClass::Unresolved(_)), "{c:?}");
}

#[test]
fn relative_target_with_undetermined_cwd_is_unresolved() {
    let fx = Fx::new();
    let scope = fx.scope_at(&fx.proj());
    let ctx = PathCtx {
        scope: &scope,
        hook_cwd: Some(&fx.proj()),
        legacy: None,
    };
    let cwd = Value::Undetermined("cd $D".into());
    let c = classify(DEL_R, &lit("src/a.rs"), &cwd, &ctx);
    assert!(matches!(c, PathClass::Unresolved(_)), "{c:?}");
}

#[test]
fn no_location_model_is_unresolved() {
    let scope = SafeRoots::none();
    let c = classify_at(
        &scope,
        Some("/nonexistent/zqxv"),
        DEL_R,
        &lit("/nonexistent/zqxv/f"),
    );
    assert!(
        matches!(c, PathClass::Unresolved(_)),
        "no resolver / no roots: nothing can be placed; got {c:?}"
    );
}
