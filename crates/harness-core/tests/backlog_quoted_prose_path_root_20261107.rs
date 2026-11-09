#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro (observed 2026-11-07 on main 31bf83ed, harness-core 0.2.92): a path
//! that appears ONLY inside the double-quoted prose of a skip command's
//! `--reason` argument (`donegate skip --reason "... <wt> ..."`) is collected
//! by `attribution::session_written_paths` as a Bash write candidate, so
//! `session_roots::resolve_with_store` makes that foreign worktree a session
//! root and Stop gates (donegate, tdd) evaluate another session's checkout.
//!
//! Contract pinned here (public API only):
//! 1. a path inside a quoted `--reason` value of `donegate skip` / `tdd skip`
//!    is NOT a Bash write candidate nor a root. RED at time of writing for
//!    `donegate skip` only: `tdd skip` / `reviewgate skip` are already on the
//!    read-only list (`read_only_stage` in attribution.rs), `donegate` is not,
//!    so the tdd sub-case passes and the donegate sub-case fails;
//! 2. controls (must stay GREEN): a real redirect write into <wt>, a Write
//!    tool call into <wt>, and an UNQUOTED bare path argument to an unknown
//!    command each still yield <wt> as a root (the restrictive side).
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
    /// The hook cwd (this session's checkout).
    repo: PathBuf,
    /// Another session's linked worktree (exists, never written here).
    foreign: PathBuf,
    store: PathBuf,
    tdir: PathBuf,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = base.join("repo");
    let foreign = base.join("run-X-t1");
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
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "x",
            foreign.to_str().unwrap(),
        ],
    );
    std::fs::write(foreign.join("f"), "x").unwrap();
    let store = base.join("origin-store.json");
    Fx {
        _tmp: tmp,
        repo,
        foreign,
        store,
        tdir,
    }
}

/// A transcript recording exactly one tool call `name` with `input`, in repo.
fn transcript(f: &Fx, file: &str, tool: &str, input: serde_json::Value) -> String {
    let user = serde_json::json!({"type":"user","message":{"role":"user","content":"go"}});
    let call = serde_json::json!({
        "type": "assistant",
        "cwd": f.repo.to_string_lossy(),
        "isSidechain": false,
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": "toolu_0", "name": tool, "input": input
        }]}
    });
    let path = f.tdir.join(format!("{file}.jsonl"));
    std::fs::write(&path, format!("{user}\n{call}\n")).unwrap();
    path.to_string_lossy().into_owned()
}

fn bash(f: &Fx, file: &str, command: &str) -> String {
    transcript(f, file, "Bash", serde_json::json!({ "command": command }))
}

/// (bash candidates, roots). Undetermined fails the test (vacuity guard).
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
fn path_inside_quoted_skip_reason_is_not_a_written_path_nor_root() {
    let f = fixture();
    let w = f.foreign.display();
    let mut bad = Vec::new();
    for (name, cmd) in [
        (
            "donegate",
            format!(
                "donegate skip --reason \"attribution bug: the failing root {w} is \
                 session bf349dd5's worktree, not ours\""
            ),
        ),
        (
            "tdd",
            format!("tdd skip --reason \"foreign root {w} belongs to another session\""),
        ),
    ] {
        let t = bash(&f, name, &cmd);
        let (cands, roots) = observe(&f, &t);
        if !under(&cands, &f.foreign).is_empty() || roots.contains(&f.foreign) {
            bad.push(format!(
                "{name}: command={cmd:?} bash candidates under foreign={:?} roots={roots:?}",
                under(&cands, &f.foreign)
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "a path named only inside the quoted --reason prose of a skip command was treated \
         as written:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_redirect_write_into_foreign_still_roots_it() {
    let f = fixture();
    let t = bash(&f, "redir", &format!("echo x > {}/f", f.foreign.display()));
    let (_, roots) = observe(&f, &t);
    assert!(roots.contains(&f.foreign), "roots={roots:?}");
}

#[test]
fn control_write_tool_into_foreign_still_roots_it() {
    let f = fixture();
    let t = transcript(
        &f,
        "write",
        "Write",
        serde_json::json!({
            "file_path": f.foreign.join("new.txt").to_string_lossy(),
            "content": "x"
        }),
    );
    let (_, roots) = observe(&f, &t);
    assert!(roots.contains(&f.foreign), "roots={roots:?}");
}

#[test]
fn control_unquoted_bare_path_to_unknown_command_still_roots_it() {
    let f = fixture();
    let t = bash(
        &f,
        "bare",
        &format!("some-unknown-tool {}/f", f.foreign.display()),
    );
    let (cands, roots) = observe(&f, &t);
    assert!(
        !under(&cands, &f.foreign).is_empty() && roots.contains(&f.foreign),
        "an unknown command may write its bare path argument: cands={cands:?} roots={roots:?}"
    );
}
