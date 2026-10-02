// Reproduction test for backlog b0e1e4a2 (audit shard s10-small-b).
// RED while the item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn run_in(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_compass"))
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(cwd: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// b0e1e4a2: outcomes recorded in the main tree (gitignored
/// `.compass/outcomes.json`) must stay visible from a linked worktree, or the
/// worktree view must say "undetermined" -- it must not say "no outcomes
/// recorded yet; persevere" while three backward outcomes exist.
#[test]
#[ignore = "backlog b0e1e4a2: open defect, remove ignore when fixed"]
fn backlog_b0e1e4a2_worktree_sees_main_outcomes_or_says_undetermined() {
    let base: PathBuf = std::env::temp_dir().join(format!("compass-s10-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["config", "user.email", "t@t"]);
    git(&main, &["config", "user.name", "t"]);
    std::fs::write(
        main.join(".gitignore"),
        ".compass/outcomes.json\n.compass/carve-state.json\n",
    )
    .unwrap();
    std::fs::create_dir_all(main.join(".compass")).unwrap();
    std::fs::write(
        main.join(".compass/charter.md"),
        "## north_star\nship a thing\n\n## definition_of_done\n- a\n\n## measuring_stick\ncount of things\n",
    )
    .unwrap();
    std::fs::write(main.join("f"), "x").unwrap();
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "seed"]);
    let wt = base.join("wt");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
    );

    for i in 0..3 {
        let ev = format!("measured regression {i}");
        let (rc, out, err) = run_in(
            &main,
            &["outcome", "--verdict", "backward", "--evidence", &ev],
        );
        assert_eq!(rc, 0, "outcome record failed: {out} {err}");
    }
    // Control: from main, the streak is visible.
    let (_, main_out, _) = run_in(&main, &["pivot-check"]);
    assert!(
        main_out.contains(r#""streak":3"#),
        "control (main) failed: {main_out}"
    );

    let (_, wt_out, _) = run_in(&wt, &["pivot-check"]);
    eprintln!("main: {main_out}\nworktree: {wt_out}");
    assert!(
        wt_out.contains(r#""streak":3"#)
            || wt_out.to_lowercase().contains("undetermined")
            || wt_out.to_lowercase().contains("unknown"),
        "worktree pivot-check collapsed 3 outcomes to a confident answer: {wt_out}"
    );
}
