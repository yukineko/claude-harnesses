//! RED repro: production `git::changed_files` honours an inherited GIT_DIR and
//! so judges the WRONG repo. Repo A is clean; repo B (unrelated) is dirty. The
//! verdict for A must not depend on GIT_DIR pointing at B.
//!
//! The env is set by re-exec'ing this test binary as a child (never
//! `set_var`), so the parallel runner is not raced.

use crate::git::changed_files;
use harness_core::git_env::scrub;
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_FLAG: &str = "GITENV_CHILD_REPO";
const MARK: &str = "SCAN_VERDICT=";

fn git(dir: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    scrub(&mut c);
    let o = c
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        o.status.success(),
        "fixture git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn fixture_repo(root: &Path, name: &str, dirty: bool) -> PathBuf {
    let p = root.join(name);
    std::fs::create_dir_all(&p).unwrap();
    git(&p, &["init", "-q"]);
    std::fs::write(p.join("tracked.txt"), "x\n").unwrap();
    git(&p, &["add", "tracked.txt"]);
    git(&p, &["commit", "-q", "-m", "init"]);
    if dirty {
        std::fs::write(p.join("b_dirty_untracked.txt"), "y\n").unwrap();
        std::fs::write(p.join("tracked.txt"), "changed\n").unwrap();
    }
    p
}

/// Inner test: only acts in the re-exec'd child.
#[test]
fn gitenv_child_scan() {
    let Ok(repo) = std::env::var(CHILD_FLAG) else {
        return;
    };
    println!("{MARK}{:?}", changed_files(Path::new(&repo)));
}

fn run_child(repo_a: &Path, envs: &[(&str, &Path)]) -> String {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        "--exact",
        "git_env_red_tests::gitenv_child_scan",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD_FLAG, repo_a);
    scrub(&mut c);
    for (k, v) in envs {
        c.env(k, v);
    }
    let o = c.output().expect("spawn child");
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    out.lines()
        .find_map(|l| l.find(MARK).map(|i| l[i + MARK.len()..].to_string()))
        .unwrap_or_else(|| {
            panic!(
                "child produced no verdict: {out}\n{}",
                String::from_utf8_lossy(&o.stderr)
            )
        })
}

#[test]
fn changed_files_ignores_inherited_git_dir() {
    if std::env::var(CHILD_FLAG).is_ok() {
        return;
    }
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "donegate-gitenv-red-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let a = fixture_repo(&root, "repo_a_clean", false);
    let b = fixture_repo(&root, "repo_b_dirty", true);

    // A directory that is not a repo at all (no .git here; temp root has none above).
    let nr = root.join("not_a_repo");
    std::fs::create_dir_all(&nr).unwrap();

    let bgit = b.join(".git");
    let bidx = bgit.join("index");
    let mut bad = Vec::new();
    for (label, target, expect) in [
        ("clean repo A", &a, "Files([])"),
        ("non-repo dir", &nr, "NotRepo"),
    ] {
        let control = run_child(target, &[]);
        assert_eq!(control, expect, "control (scrubbed) verdict for {label}");
        let cases: [(&str, Vec<(&str, &Path)>); 3] = [
            ("GIT_DIR", vec![("GIT_DIR", bgit.as_path())]),
            (
                "GIT_DIR+GIT_INDEX_FILE",
                vec![
                    ("GIT_DIR", bgit.as_path()),
                    ("GIT_INDEX_FILE", bidx.as_path()),
                ],
            ),
            (
                "GIT_DIR+GIT_WORK_TREE",
                vec![("GIT_DIR", bgit.as_path()), ("GIT_WORK_TREE", b.as_path())],
            ),
        ];
        for (name, envs) in &cases {
            let got = run_child(target, envs);
            if got != control {
                bad.push(format!(
                    "{label} under {name}: got {got}, control {control}"
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        bad.is_empty(),
        "donegate git::changed_files judged the wrong repo (inherited git env followed): {bad:#?}"
    );
}
