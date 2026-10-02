// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 1ae35d62: the branch-resolve preflight of `worktree merge`
//! (c49761cd / f14c18be) refuses an unresolvable branch via `git_try(rev-parse
//! --verify --quiet <branch>^{commit})`, and `git_try` maps a TIMED-OUT git to
//! `resolves == false`. That timeout arm had no RED/GREEN test.
//!
//! Fixture: a real repo with a real, resolvable branch `feat`, and a PATH shim
//! `git` that hangs past the production `GIT_TIMEOUT` (45s, a const) ONLY for
//! the `<branch>^{commit}` rev-parse, delegating every other call to real git.
//! Because `feat` really resolves, a timeout read as success would let the
//! merge proceed — so the test is RED exactly when the timeout arm fails open.
//! The control proves the same fixture merges when nothing hangs.
//!
//! Written by an independent auditor, not an implementer.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn real_git() -> PathBuf {
    for d in std::env::var("PATH").unwrap_or_default().split(':') {
        let p = PathBuf::from(d).join("git");
        if p.is_file() {
            return p;
        }
    }
    panic!("no git on PATH: fixture is void");
}

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    shim: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new(real_git())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(o.status.success(), "git {args:?}: {o:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

impl Fx {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "condukt-1ae35d62-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = base.join("repo");
        let home = base.join("home");
        let shim = base.join("shim");
        for d in [&repo, &home, &shim] {
            std::fs::create_dir_all(d).unwrap();
        }
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&repo, &["checkout", "-q", "-b", "feat"]);
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "feat"]);
        git(&repo, &["checkout", "-q", "main"]);

        let script = format!(
            "#!/bin/sh\n\
             hang=0; rp=0\n\
             for a in \"$@\"; do\n\
               case \"$a\" in\n\
                 rev-parse) rp=1 ;;\n\
                 *'^{{commit}}') hang=1 ;;\n\
               esac\n\
             done\n\
             if [ \"$rp$hang\" = 11 ] && [ -n \"$HANG_RESOLVE\" ]; then sleep 90; exit 0; fi\n\
             exec '{}' \"$@\"\n",
            real_git().display()
        );
        let g = shim.join("git");
        std::fs::write(&g, script).unwrap();
        std::fs::set_permissions(&g, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            base,
            repo,
            home,
            shim,
        }
    }

    fn merge(&self, hang: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        cmd.args(["worktree", "merge", "--branch", "feat"])
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wtb"))
            .env(
                "PATH",
                format!("{}:{}", self.shim.display(), "/usr/bin:/bin"),
            )
            .env_remove("CLAUDE_CODE_SESSION_ID");
        if hang {
            cmd.env("HANG_RESOLVE", "1");
        }
        cmd.output().expect("spawn condukt")
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn control_same_fixture_merges_when_resolve_does_not_hang() {
    let f = Fx::new("control");
    let o = f.merge(false);
    assert!(o.status.success(), "control merge failed: {}", text(&o));
    let main = git(&f.repo, &["rev-parse", "main"]);
    let feat = git(&f.repo, &["rev-parse", "feat"]);
    let mb = git(&f.repo, &["merge-base", "main", "feat"]);
    assert_eq!(
        mb, feat,
        "control: feat must be merged into main (main={main})"
    );
}

#[test]
fn timed_out_branch_resolve_refuses_the_merge() {
    let f = Fx::new("hang");
    let main_before = git(&f.repo, &["rev-parse", "main"]);
    let t0 = std::time::Instant::now();
    let o = f.merge(true);
    let took = t0.elapsed();
    let all = text(&o);
    assert!(
        took >= std::time::Duration::from_secs(40),
        "precondition: the shim must actually hang the resolve past GIT_TIMEOUT; took {took:?}: {all}"
    );
    assert!(
        !o.status.success(),
        "a timed-out branch resolve must refuse the merge (exit non-zero): {all}"
    );
    assert!(
        all.contains("timed out"),
        "the refusal must name the timeout, not some unrelated error: {all}"
    );
    let main_after = git(&f.repo, &["rev-parse", "main"]);
    assert_eq!(
        main_before, main_after,
        "main moved although the branch could not be resolved: {all}"
    );
    assert!(
        !all.contains("merge-conflict") && !all.to_lowercase().contains("held for review"),
        "a timed-out resolve must not be recorded as a conflict: {all}"
    );
}
