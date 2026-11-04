#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression (observed 2026-11-04): ONE Bash command whose every stage that
//! names a linked worktree is read-only (`cd <wt> && git status --short`) made
//! that worktree a session root — and so a donegate Stop root — only because
//! the same command also contained a read-only command substitution
//! (`c=$(readlink …)`). The bash parser gives up on `$(…)` and keeps the WHOLE
//! command as "possibly writing" (documented over-attribution), so every
//! existing path the command names becomes a written candidate. donegate then
//! ran its checks in another session's worktree and blocked this session for
//! that session's failing tests.
//!
//! Contract pinned here (observed through the public API only:
//! `attribution::session_written_paths(..).bash` and
//! `session_roots::resolve_with_store`):
//! 1. a read-only `$(…)` plus provably read-only statements naming <wt> must
//!    NOT make <wt> a written candidate nor a root (RED at time of writing);
//! 2. a `$(…)` command where a statement writes into <wt> still yields <wt>;
//! 3. a `$(…)` whose inner command writes into <wt> still yields <wt>
//!    (a fix must not make substitutions blind).
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use harness_core::attribution::session_written_paths;
use harness_core::session_roots::resolve_with_store;
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be runnable for this test");
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

struct Fx {
    _tmp: tempfile::TempDir,
    /// Main checkout; the hook cwd.
    repo: PathBuf,
    /// Linked worktree A (holds "another session's" uncommitted change).
    wt_a: PathBuf,
    /// Linked worktree B.
    wt_b: PathBuf,
    store: PathBuf,
    tdir: PathBuf,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = base.join("repo");
    let wt_a = base.join("wt-A");
    let wt_b = base.join("wt-B");
    let tdir = base.join("t");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&tdir).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "a", wt_a.to_str().unwrap()],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "b", wt_b.to_str().unwrap()],
    );
    // Another session's uncommitted change in wt-A, and files the control
    // commands name (candidates must exist to be seen at all).
    std::fs::write(wt_a.join("peer.rs"), "x").unwrap();
    std::fs::write(wt_a.join("f.txt"), "x").unwrap();
    std::fs::write(wt_a.join("f"), "x").unwrap();
    let store = base.join("origin-store.json");
    Fx {
        _tmp: tmp,
        repo,
        wt_a,
        wt_b,
        store,
        tdir,
    }
}

/// A transcript recording exactly one Bash call `command`, run in `cwd`.
fn transcript(f: &Fx, name: &str, command: &str) -> String {
    let user = serde_json::json!({"type":"user","message":{"role":"user","content":"go"}});
    let call = serde_json::json!({
        "type": "assistant",
        "cwd": f.repo.to_string_lossy(),
        "isSidechain": false,
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": "toolu_0", "name": "Bash",
            "input": {"command": command}
        }]}
    });
    let path = f.tdir.join(format!("{name}.jsonl"));
    std::fs::write(&path, format!("{user}\n{call}\n")).unwrap();
    path.to_string_lossy().into_owned()
}

/// (bash candidates, roots) for the transcript. Undetermined fails the test:
/// it would make the assertions vacuous.
fn observe(f: &Fx, t: &str) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let bash = match session_written_paths(t) {
        Determination::Known(w) => w.bash.into_iter().collect::<Vec<_>>(),
        Determination::Undetermined(why) => {
            panic!("session_written_paths Undetermined: {}", why.as_str())
        }
    };
    let roots = match resolve_with_store(&f.repo, t, &f.store) {
        Determination::Known(sr) => sr.roots,
        Determination::Undetermined(why) => {
            panic!("resolve_with_store Undetermined: {}", why.as_str())
        }
    };
    (bash, roots)
}

fn under(paths: &[PathBuf], dir: &Path) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|p| p.starts_with(dir))
        .cloned()
        .collect()
}

#[test]
#[ignore = "RED repro: $() makes bash attribution keep read-only statements (see backlog); run with --ignored"]
fn read_only_substitution_and_read_only_statements_do_not_root_a_worktree() {
    let f = fixture();
    let a = f.wt_a.display();
    let b = f.wt_b.display();
    // Shape of the observed command, with every wt-naming stage read-only.
    let cmd = format!(
        "for p in /proc/[0-9]*; do c=$(readlink $p/cwd 2>/dev/null); echo $c; done; \
         ls -la {a}/peer.rs; cd {a} && git status --short; \
         cd {b} && git status --short && git log --oneline main..HEAD"
    );
    let t = transcript(&f, "ro", &cmd);
    let (bash, roots) = observe(&f, &t);

    let bad_a = under(&bash, &f.wt_a);
    let bad_b = under(&bash, &f.wt_b);
    assert!(
        bad_a.is_empty()
            && bad_b.is_empty()
            && !roots.contains(&f.wt_a)
            && !roots.contains(&f.wt_b),
        "a command whose only `$(…)` is read-only and whose every statement naming the \
         worktrees is read-only attributed them as written:\n  bash candidates under wt-A: \
         {bad_a:?}\n  bash candidates under wt-B: {bad_b:?}\n  roots: {roots:?}"
    );
}

#[test]
#[ignore = "RED repro: $() makes bash attribution keep read-only statements (see backlog); run with --ignored"]
fn minimal_read_only_substitution_does_not_root_a_worktree() {
    let f = fixture();
    let a = f.wt_a.display();
    let cmd = format!("c=$(readlink /x); cd {a} && git status --short");
    let t = transcript(&f, "ro-min", &cmd);
    let (bash, roots) = observe(&f, &t);
    let bad = under(&bash, &f.wt_a);
    assert!(
        bad.is_empty() && !roots.contains(&f.wt_a),
        "`{cmd}` attributed wt-A as written: bash candidates {bad:?}, roots {roots:?}"
    );
}

#[test]
fn control_substitution_with_a_writing_statement_still_roots_the_worktree() {
    let f = fixture();
    let a = f.wt_a.display();
    let cmd = format!("x=$(date); echo hi > {a}/f.txt");
    let t = transcript(&f, "w-stmt", &cmd);
    let (bash, roots) = observe(&f, &t);
    assert!(
        !under(&bash, &f.wt_a).is_empty() && roots.contains(&f.wt_a),
        "`{cmd}` writes into wt-A but was not attributed: bash {bash:?}, roots {roots:?}"
    );
}

#[test]
fn control_writing_substitution_still_roots_the_worktree() {
    let f = fixture();
    let a = f.wt_a.display();
    let cmd = format!("x=$(touch {a}/f)");
    let t = transcript(&f, "w-subst", &cmd);
    let (bash, roots) = observe(&f, &t);
    assert!(
        !under(&bash, &f.wt_a).is_empty() && roots.contains(&f.wt_a),
        "`{cmd}` writes into wt-A inside `$(…)` but was not attributed: bash {bash:?}, \
         roots {roots:?}"
    );
}
