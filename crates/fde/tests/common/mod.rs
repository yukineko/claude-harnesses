// このファイルは integration test 用の共有 fixture なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
// 各 test バイナリが使う helper は一部だけなので dead_code も許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]
//! Shared fixture for the fde contract tests.
//!
//! Written by an independent test author (CLAUDE.md §2(a)). Everything here
//! drives the REAL built binary (`CARGO_BIN_EXE_fde`) against:
//!   - a REAL temp git repo (the target), with an initial commit;
//!   - a separate temp state dir (`FDE_STATE_DIR`), outside the target;
//!   - a separate temp dir holding the spec TOML, outside the target.
//!
//! Child processes (git and fde) run with `GIT_DIR` / `GIT_WORK_TREE` /
//! `GIT_INDEX_FILE` removed, so a test run from inside a git hook or a
//! worktree cannot leak the outer repository into the fixture.
//!
//! The CLI surface assumed is the contract in `crates/fde/src/main.rs` plus
//! the shared additions (`fde begin [TARGET] --spec FILE`,
//! `fde ratify [TARGET] --spec FILE`, the specforge-shaped spec TOML).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// Env vars that must never leak from the outer process into a fixture child.
const GIT_LEAK_VARS: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"];

/// Path of the built fde binary.
pub fn fde_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fde")
}

/// Run `git args` in `dir`, panicking (with output) on a non-zero exit.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    c.current_dir(dir).args(args);
    for v in GIT_LEAK_VARS {
        c.env_remove(v);
    }
    let out = c.output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A temp git repo with one initial commit. `root` is canonical
/// (symlink-resolved; on macOS the temp dir lives behind `/var` -> `/private/var`).
pub struct TempRepo {
    _dir: TempDir,
    pub root: PathBuf,
}

impl TempRepo {
    /// `git init` + a committed `README.md`.
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("fde-target-")
            .tempdir()
            .expect("tempdir");
        let root = dir.path().canonicalize().expect("canonicalize repo");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "fde-test"]);
        git(&root, &["config", "user.email", "fde-test@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "fixture\n").expect("write README");
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-q", "-m", "initial"]);
        TempRepo { _dir: dir, root }
    }

    /// Create (recursively) the subdir `rel` under the root and return its path.
    pub fn subdir(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(&p).expect("create nested subdir");
        p
    }

    /// `git status --porcelain --ignored` of the repo.
    pub fn porcelain_ignored(&self) -> String {
        git(&self.root, &["status", "--porcelain", "--ignored"])
    }
}

impl Default for TempRepo {
    fn default() -> Self {
        Self::new()
    }
}

/// A plain temp dir (for the state dir or the spec), canonicalized.
pub struct TempPlace {
    _dir: TempDir,
    pub path: PathBuf,
}

impl TempPlace {
    pub fn new(prefix: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("tempdir");
        let path = dir.path().canonicalize().expect("canonicalize tempdir");
        TempPlace { _dir: dir, path }
    }
}

/// Render a specforge-shaped spec TOML with the given `(id, statement)` requirements.
pub fn spec_toml(spec_id: &str, reqs: &[(&str, &str)]) -> String {
    let mut s = format!("[spec]\nid = \"{spec_id}\"\ntitle = \"fixture spec {spec_id}\"\n");
    for (id, statement) in reqs {
        s.push_str(&format!(
            "\n[[requirement]]\nid = \"{id}\"\nstatement = \"{statement}\"\nacceptance = [\"observable for {id}\"]\n"
        ));
    }
    s
}

/// Write a spec TOML file named `name` into `dir` and return its path.
pub fn write_spec(dir: &Path, name: &str, spec_id: &str, reqs: &[(&str, &str)]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, spec_toml(spec_id, reqs)).expect("write spec");
    p
}

/// Run `fde args` with cwd `cwd` and `FDE_STATE_DIR=state`, git leak vars removed.
pub fn run_fde(cwd: &Path, state: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(fde_bin());
    c.current_dir(cwd).args(args).env("FDE_STATE_DIR", state);
    for v in GIT_LEAK_VARS {
        c.env_remove(v);
    }
    c.output().expect("spawn fde")
}

/// Exit code of `out` (`None` = killed by a signal).
pub fn code(out: &Output) -> Option<i32> {
    out.status.code()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One-line rendering of an fde run for assertion messages.
pub fn show(out: &Output) -> String {
    format!(
        "exit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        stdout(out),
        stderr(out)
    )
}

/// A full fde engagement fixture: target repo + state dir + spec dir, all distinct.
pub struct Engagement {
    pub repo: TempRepo,
    pub state: TempPlace,
    pub spec_dir: TempPlace,
    pub spec: PathBuf,
}

impl Engagement {
    /// Fresh target with a spec carrying `reqs`, NOT yet registered.
    pub fn new(reqs: &[(&str, &str)]) -> Self {
        let repo = TempRepo::new();
        let state = TempPlace::new("fde-state-");
        let spec_dir = TempPlace::new("fde-spec-");
        let spec = write_spec(&spec_dir.path, "spec.toml", "fixture", reqs);
        Engagement {
            repo,
            state,
            spec_dir,
            spec,
        }
    }

    pub fn root(&self) -> &Path {
        &self.repo.root
    }

    pub fn spec_str(&self) -> &str {
        self.spec.to_str().expect("utf-8 spec path")
    }

    /// Run fde with cwd = target root.
    pub fn fde(&self, args: &[&str]) -> Output {
        run_fde(&self.repo.root, &self.state.path, args)
    }

    /// Run fde with cwd = `cwd`.
    pub fn fde_in(&self, cwd: &Path, args: &[&str]) -> Output {
        run_fde(cwd, &self.state.path, args)
    }

    /// `fde begin --spec SPEC` from the root; asserts exit 0.
    pub fn begin(&self) {
        let out = self.fde(&["begin", "--spec", self.spec_str()]);
        assert_eq!(
            code(&out),
            Some(0),
            "fde begin must register: {}",
            show(&out)
        );
    }

    /// `fde ratify --spec SPEC` from the root; asserts exit 0.
    pub fn ratify(&self) {
        let out = self.fde(&["ratify", "--spec", self.spec_str()]);
        assert_eq!(
            code(&out),
            Some(0),
            "fde ratify must record: {}",
            show(&out)
        );
    }

    /// begin + ratify, then confirm `fde status` reports ratified (exit 0).
    pub fn ratified(reqs: &[(&str, &str)]) -> Self {
        let e = Self::new(reqs);
        e.begin();
        e.ratify();
        let st = e.fde(&["status"]);
        assert_eq!(code(&st), Some(0), "target must be ratified: {}", show(&st));
        e
    }
}

/// Recursive snapshot `relative path -> bytes` of every regular file under `dir`.
pub fn snapshot_files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut m = BTreeMap::new();
    fn walk(base: &Path, d: &Path, m: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let rd = fs::read_dir(d).expect("read_dir in snapshot");
        for ent in rd {
            let ent = ent.expect("dir entry in snapshot");
            let p = ent.path();
            let ft = ent.file_type().expect("file type in snapshot");
            if ft.is_dir() {
                walk(base, &p, m);
            } else if ft.is_file() {
                let rel = p.strip_prefix(base).expect("under base").to_path_buf();
                m.insert(rel, fs::read(&p).expect("read file in snapshot"));
            }
        }
    }
    // A state dir that was never created has no files; anything else must be readable.
    if dir.exists() {
        walk(dir, dir, &mut m);
    }
    m
}

/// Files that are new or changed in `after` relative to `before` (absolute paths under `dir`).
pub fn changed_files(
    dir: &Path,
    before: &BTreeMap<PathBuf, Vec<u8>>,
    after: &BTreeMap<PathBuf, Vec<u8>>,
) -> Vec<PathBuf> {
    after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, _)| dir.join(k))
        .collect()
}
