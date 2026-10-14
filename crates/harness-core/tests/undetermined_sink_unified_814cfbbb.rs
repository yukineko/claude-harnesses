// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 814cfbbb: the default `undetermined.jsonl` sink must be keyed on the
//! MAIN worktree root (the identity overwatch's other stores use via
//! `projkey::main_worktree_root`), not on `projkey::repo_root`, which for a cwd
//! inside a LINKED worktree is the linked worktree itself.
//!
//! Contract under test (production context: `CARGO` unset, sink env unset,
//! `HOME` sandboxed):
//!   (A) cwd = main checkout, cwd = linked worktree, cwd = subdir of the linked
//!       worktree all resolve to the SAME `SinkState::Active(path)`, and that
//!       path is `$HOME/.overwatch/<project_key(main)>/overwatch/undetermined.jsonl`.
//!   (B) fail-closed: when the main worktree root cannot be determined (a `.git`
//!       FILE without a `gitdir:` line), `sink_state()` is `Unresolvable`, never
//!       an `Active` per-checkout path posing as the unified one; and `record()`
//!       in that state returns (no hang / unbounded recursion) and writes no
//!       `undetermined.jsonl` anywhere under the sandboxed HOME.
//!   (C) a plain non-repo directory still yields `Active` (no regression).
//!
//! One `#[test]` in its own binary because it mutates process-global state
//! (`CARGO`, `HOME`, the sink env var, and the cwd). Every sub-check is run and
//! its failures collected, so a RED run reports all broken clauses at once.

use harness_core::projkey;
use harness_core::undetermined::{self, SinkState};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// Restores every piece of process-global state this test touches.
struct EnvGuard {
    cwd: PathBuf,
    home: Option<std::ffi::OsString>,
    cargo: Option<std::ffi::OsString>,
    sink: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn capture() -> Self {
        EnvGuard {
            cwd: std::env::current_dir().unwrap(),
            home: std::env::var_os("HOME"),
            cargo: std::env::var_os("CARGO"),
            sink: std::env::var_os(undetermined::SINK_ENV),
        }
    }
}

fn restore(key: &str, v: &Option<std::ffi::OsString>) {
    match v {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.cwd);
        restore("HOME", &self.home);
        restore("CARGO", &self.cargo);
        restore(undetermined::SINK_ENV, &self.sink);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .args(["-c", "user.email=t@example.invalid", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr)
    );
}

fn sink_at(cwd: &Path) -> SinkState {
    std::env::set_current_dir(cwd).unwrap();
    undetermined::sink_state()
}

fn find_named(root: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_named(&p, name, out);
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

fn has_git_ancestor(p: &Path) -> bool {
    p.ancestors().any(|a| a.join(".git").exists())
}

#[test]
fn backlog_814cfbbb_default_sink_is_unified_across_worktrees_and_fails_closed() {
    let _guard = EnvGuard::capture();
    let mut failures: Vec<String> = Vec::new();

    // Canonicalize up front so macOS /var -> /private/var cannot make two
    // spellings of one directory look like two projects.
    let td = tempfile::tempdir().unwrap();
    let base = td.path().canonicalize().unwrap();
    let home = base.join("home");
    let main = base.join("repos").join("main");
    let wt = base.join("repos").join("wt");
    let wt_sub = wt.join("deep").join("sub");
    let bogus = base.join("bogus");
    let plain = base.join("plain");
    for d in [&home, &main, &bogus, &plain] {
        std::fs::create_dir_all(d).unwrap();
    }

    // Real repo + linked worktree, built BEFORE HOME is swapped so git's own
    // config lookup is not perturbed.
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    std::fs::create_dir_all(&wt_sub).unwrap();
    assert!(
        std::fs::metadata(wt.join(".git")).unwrap().is_file(),
        "fixture: a linked worktree's .git must be a file"
    );

    // A `.git` FILE with no `gitdir:` line: main_worktree_root is documented to
    // return Undetermined for this.
    std::fs::write(bogus.join(".git"), "this is not a gitdir pointer\n").unwrap();
    assert!(
        matches!(
            projkey::main_worktree_root(&bogus),
            harness_core::verdict::Determination::Undetermined(_)
        ),
        "fixture: main_worktree_root must be Undetermined for a .git file without gitdir:"
    );

    // Production context, sandboxed HOME.
    std::env::remove_var("CARGO");
    std::env::remove_var(undetermined::SINK_ENV);
    std::env::set_var("HOME", &home);

    // ---------------- (A) one sink per project ----------------
    let expected = home
        .join(".overwatch")
        .join(projkey::project_key(&main))
        .join("overwatch")
        .join("undetermined.jsonl");
    let at_main = sink_at(&main);
    let at_wt = sink_at(&wt);
    let at_sub = sink_at(&wt_sub);
    for (label, got) in [
        ("main checkout", &at_main),
        ("linked worktree", &at_wt),
        ("subdir of linked worktree", &at_sub),
    ] {
        match got {
            SinkState::Active(p) if *p == expected => {}
            other => failures.push(format!(
                "(A) cwd = {label}: expected Active({}) keyed on the MAIN worktree root, got {other:?}",
                expected.display()
            )),
        }
    }
    if !(at_main == at_wt && at_wt == at_sub) {
        failures.push(format!(
            "(A) the same project resolves to different sinks: main={at_main:?} wt={at_wt:?} wt_sub={at_sub:?}"
        ));
    }

    // ---------------- (B) fail-closed when main root is undeterminable --------
    let at_bogus = sink_at(&bogus);
    if !matches!(at_bogus, SinkState::Unresolvable(_)) {
        failures.push(format!(
            "(B) cwd whose main worktree root is Undetermined must yield SinkState::Unresolvable, got {at_bogus:?}"
        ));
    }
    // record() in that state must return (an implementation that resolves the
    // main root inside sink_state may re-enter record via Determination::
    // undetermined) and must not write anywhere under HOME.
    //
    // Accounting contract: every event record() receives while the sink is
    // Unresolvable — the outer event AND each nested undetermined event minted
    // while resolving the sink (main_worktree_root's own give-up re-enters
    // record) — is counted in dropped_writes(), and none is counted as written.
    // So one outer call must raise dropped_writes() by >= 2 (1 outer + >= 1
    // nested) and leave written() unchanged. The counters are process-global;
    // they are sampled inside the thread, immediately around the single call,
    // while the main thread is blocked on the channel.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let d0 = undetermined::dropped_writes();
        let w0 = undetermined::written();
        undetermined::record("probe-814cfbbb", std::panic::Location::caller());
        let d1 = undetermined::dropped_writes();
        let w1 = undetermined::written();
        let _ = tx.send((d0, d1, w0, w1));
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Err(_) => {
            failures.push("(B) record() did not return within 20s in the unresolvable state".into())
        }
        Ok((d0, d1, w0, w1)) => {
            if d1 <= d0 {
                failures.push(format!(
                    "(B) record() in the unresolvable state lost the event without counting it: \
                     dropped_writes() {d0} -> {d1} (must strictly increase)"
                ));
            }
            if d1 < d0 + 2 {
                failures.push(format!(
                    "(B) record() in the unresolvable state must count the outer event AND the \
                     nested give-up(s) from resolving the sink: dropped_writes() {d0} -> {d1} \
                     (expected an increase of >= 2)"
                ));
            }
            if w1 != w0 {
                failures.push(format!(
                    "(B) record() in the unresolvable state counted a write: written() {w0} -> {w1}"
                ));
            }
        }
    }
    let mut written = Vec::new();
    find_named(&home, "undetermined.jsonl", &mut written);
    if !written.is_empty() {
        failures.push(format!(
            "(B) record() in the unresolvable state wrote undetermined.jsonl under the sandboxed HOME: {written:?}"
        ));
    }

    // ---------------- (C) non-repo cwd still records ----------------
    assert!(
        !has_git_ancestor(&plain),
        "fixture: {} must have no .git ancestor for (C) to mean anything",
        plain.display()
    );
    match sink_at(&plain) {
        SinkState::Active(p) => {
            if !p.starts_with(&home) || !p.ends_with("overwatch/undetermined.jsonl") {
                failures.push(format!(
                    "(C) non-repo cwd: Active path {} is not $HOME/.overwatch/<key>/overwatch/undetermined.jsonl",
                    p.display()
                ));
            }
        }
        other => failures.push(format!(
            "(C) a plain non-repo cwd must still yield Active, got {other:?}"
        )),
    }

    assert!(
        failures.is_empty(),
        "backlog 814cfbbb contract violated ({} clause(s)):\n  - {}",
        failures.len(),
        failures.join("\n  - ")
    );
}
