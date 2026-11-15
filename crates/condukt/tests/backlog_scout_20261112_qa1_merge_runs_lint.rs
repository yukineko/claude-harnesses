// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (scout 2026-11-12 QA1): `condukt worktree merge` runs
//! `cargo fmt` / `cargo clippy` on every merge.
//!
//! User ruling 2026-11-12: the per-merge lint gate of backlog 0a68823d is
//! retracted; lint moves to the push tier. At HEAD
//! `lint_gate::ensure_tip_lint_clean` (called from crates/condukt/src/worktree.rs:1166
//! in `merge` and :1604 in `resolve_merge`) still spawns `cargo fmt --check` and
//! `cargo clippy -D warnings` for every branch that changes a Cargo package.
//!
//! Fixture: a temp Cargo repo, a task branch made with `condukt worktree create`
//! (so `conduktBase` and the task identity are recorded) whose trailer-carrying
//! commit changes `src/lib.rs`, and a stub `cargo` FIRST on `PATH` that appends
//! its argv to a log file and exits 0. For `cargo metadata` the stub also prints
//! a minimal metadata document naming the fixture package (without it the
//! HEAD gate refuses the merge as `merge-lint-undetermined`, so the control
//! could not show the merge succeeding); this is fixture plumbing, not part of
//! the property.
//!
//! Control: the merge succeeds and `main` advances. RED: the argv log has no
//! `clippy` / `fmt` invocation.
//!
//! RED observed at rev 656b1c38 (written without a fix, by a disinterested
//! author — CLAUDE.md §2(a)). The real binary runs against a per-test `$HOME`,
//! state dir and worktree base inside a throwaway git repo with
//! `GIT_CONFIG_GLOBAL=/dev/null`; nothing under the real `~/.condukt`,
//! `~/.budgetguard` or `~/.overwatch` is touched, and no real cargo is run.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    stub_dir: PathBuf,
    log: PathBuf,
}

const MANIFEST: &str =
    "[package]\nname = \"qafx\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n";

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        let stub_dir = base.join("stub-bin");
        let log = base.join("cargo-argv.log");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&stub_dir).unwrap();

        // Stub cargo: log argv; for `metadata`, print a minimal document for
        // the manifest it was given; exit 0 always.
        let stub = format!(
            "#!/bin/sh\n\
             echo \"$*\" >> '{log}'\n\
             if [ \"$1\" = metadata ]; then\n\
               mp=''\n\
               prev=''\n\
               for a in \"$@\"; do\n\
                 if [ \"$prev\" = --manifest-path ]; then mp=\"$a\"; fi\n\
                 prev=\"$a\"\n\
               done\n\
               root=$(dirname \"$mp\")\n\
               printf '{{\"workspace_root\":\"%s\",\"packages\":[{{\"name\":\"qafx\",\"manifest_path\":\"%s\"}}]}}\\n' \"$root\" \"$mp\"\n\
             fi\n\
             exit 0\n",
            log = log.display()
        );
        let stub_path = stub_dir.join("cargo");
        std::fs::write(&stub_path, stub).unwrap();
        std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("Cargo.toml"), MANIFEST).unwrap();
        std::fs::write(repo.join(".gitignore"), "/target\nCargo.lock\n").unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    42\n}\n",
        )
        .unwrap();
        run_git(&repo, &["add", "Cargo.toml", ".gitignore", "src/lib.rs"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);
        Self {
            _tmp: tmp,
            base,
            repo,
            home,
            stub_dir,
            log,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        let ambient = std::env::var_os("PATH").unwrap_or_default();
        let mut path = vec![self.stub_dir.clone()];
        path.extend(std::env::split_paths(&ambient));
        let path = std::env::join_paths(path).unwrap();
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("CARGO_HOME", self.home.join("no-cargo-home"))
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wts"))
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CLAUDE_CODE_SESSION_ID", "sess-scout-qa1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    /// `condukt worktree create`, change `src/lib.rs`, commit with the recorded
    /// task identity as trailer. Returns the branch.
    fn task_branch(&self) -> String {
        let out = self.condukt(&[
            "worktree", "create", "--run", "run-qa1", "--topic", "t1", "--branch", "b-t1",
        ]);
        assert!(out.status.success(), "worktree create: {out:?}");
        let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
        let branch = run_git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let identity = run_git(
            &self.repo,
            &[
                "config",
                "--local",
                "--get",
                &format!("branch.{branch}.conduktTask"),
            ],
        );
        std::fs::write(
            wt.join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    42\n}\n\npub fn more() -> u32 {\n    1\n}\n",
        )
        .unwrap();
        run_git(&wt, &["add", "src/lib.rs"]);
        run_git(
            &wt,
            &[
                "commit",
                "-q",
                "-m",
                &format!("worker change\n\nCondukt-Task: {identity}"),
            ],
        );
        branch
    }
}

#[test]
fn worktree_merge_runs_no_cargo_fmt_or_clippy() {
    let f = Fixture::new();
    let branch = f.task_branch();
    let before = run_git(&f.repo, &["rev-parse", "main"]);

    let out = f.condukt(&["worktree", "merge", "--branch", &branch]);
    let after = run_git(&f.repo, &["rev-parse", "main"]);
    let log = std::fs::read_to_string(&f.log).unwrap_or_default();
    eprintln!(
        "observation: exit={:?}\nstdout={}\nstderr={}\nmain {before} -> {after}\nstub cargo argv \
         log:\n{log}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // CONTROL: the merge itself succeeds and the default branch advances.
    assert!(
        out.status.success() && after != before,
        "CONTROL: the merge must succeed and advance main; exit={:?} main {before} -> {after}",
        out.status.code()
    );

    // RED: no cargo fmt / cargo clippy invocation on the merge path.
    let lint_lines: Vec<&str> = log
        .lines()
        .filter(|l| l.split_whitespace().any(|w| w == "clippy" || w == "fmt"))
        .collect();
    assert!(
        lint_lines.is_empty(),
        "`condukt worktree merge` invoked cargo fmt/clippy (the per-merge lint gate was \
         retracted by the 2026-11-12 ruling; lint belongs to the push tier): {lint_lines:?}"
    );
}
