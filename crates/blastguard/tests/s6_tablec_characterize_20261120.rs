#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S6-1 characterization pins (user ruling 2026-11-20): today's verdicts for
//! the Table C cells marked `legacy` in `src/detect/ast/SUBSET.md` — Project,
//! Other, OtherWorktree, and Protected x GitDiscard / GitStage — per B.1
//! command row (and the B.4 git rows for GitDiscard / GitStage), per effect,
//! main mode and subagent mode.
//!
//! Written by an independent test author (not the S6 implementer; CLAUDE.md
//! §2(a)). These are CHARACTERIZATION pins: every expected value below was
//! MEASURED against the hook binary at the S6-1 base (main 5cfe8e73e,
//! blastguard 0.2.764, 2026-11-20), not derived from the spec. They pass
//! today by construction; their job is to make any later S6 step that moves
//! one of these cells fail loudly, naming the cell and the condition. No RED
//! is expected from this file.
//!
//! Conditional cells (ruling: "conditional cells = rule + measured
//! verdicts"): where the verdict depends on more than the class, each
//! condition is its own row — git recoverability (tracked+clean / tracked+
//! modified / untracked / absent), tee / cp / truncate / dd target absent vs
//! existing, chmod mode (`+x`, `644`, `000`, `-R`), and the reversible probe
//! for git discard (clean vs dirty work tree: `git_*_clean` vs
//! `git_*_dirty`). Where both conditions were measured and gave the same
//! verdict, the row is pinned once and run under both (stated on the table).
//!
//! A pin is NOT an endorsement: several measured cells look questionable
//! (e.g. `rm <file under $HOME>` Allow, `chmod 000` Allow outside Protected,
//! Protected `restore` vs `checkout` disagreeing in a CLEAN OtherWorktree).
//! Moving one is a ruling, not a test edit; see the S6-1 matrix for the rule
//! each cell comes from.
//!
//! Moved by ruling (user ruling 2026-11-20, backlog 606a93ab): a git
//! working-tree discard (`checkout -- <path>`, `checkout .`, `restore
//! <path>`, `reset --hard`, `clean -f[d]`) into OtherWorktree — a linked
//! worktree that does not hold the session cwd — is Ask (main) / Deny
//! (subagent) when that checkout has uncommitted changes (judged for the whole
//! checkout, so a clean path inside a dirty checkout is gated too); a CLEAN
//! OtherWorktree keeps its measured Allow. The dirty-fixture git tables
//! (`GIT_ROWS_DIRTY`, `PROTECTED_GIT_ROWS_DIRTY`) carry the new cells; the
//! clean-fixture tables are unchanged measurements. Previously one table was
//! shared by both fixtures (measured `-C <other worktree> reset --hard` Allow
//! on a dirty tree, at blastguard 0.2.764).
//!
//! Fixture (hermetic, `support/neutral_base.rs` location rule):
//!   * `<base>/home`                       — the hook's `HOME`
//!   * `<base>/src/proj`                   — git repo, main tree; the session
//!     cwd and `CLAUDE_PROJECT_DIR` (cell **Project**)
//!   * `<base>/src/.harness-worktrees/wt-other` — a linked worktree of that
//!     repo (`git worktree add`), a sibling checkout NOT containing the
//!     session cwd, inside the worktree storage root derived from the cwd
//!     (cell **OtherWorktree**)
//!   * `<base>/home/docs`                  — plain dir under the fixture
//!     `HOME`, no git, not a temp / cache / worktree root (cell **Other**)
//!
//! The hook runs with `env_clear()` plus `HOME`, `PATH=/usr/bin:/bin`,
//! `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT=cli`, `CLAUDE_PROJECT_DIR`,
//! `BLASTGUARD_APPROVALS_DIR`; no `TMPDIR`, no `CLAUDE_CODE_SESSION_ID`, git
//! variables scrubbed. Subagent mode = `agent_id` in the payload. Nothing is
//! executed; the hook only judges the text.
//!
//! Each root holds: `t.txt` (tracked, clean), `m.txt` (tracked; modified in
//! the dirty fixture), `src.txt`, `srcdir/s.txt`, `dir/a.txt` (tracked),
//! `.claude/settings.json` (tracked; modified in the dirty fixture); and in
//! the dirty fixture also `u.txt`, `udir/x` (untracked) and `edir/` (empty).
//! `new.txt`, `newdir`, `newlink`, `newhard` are absent. In **Other** every
//! file is a plain existing file (no git), so "tracked/untracked" in a row's
//! condition only describes the Project / OtherWorktree roots.
//!
//! Expected cell syntax: `"<main>/<subagent>"`, or `"-"` = row not applicable
//! to that cell (not run).
//!
//! DOES NOT PROVE: which rule produced a verdict (only the decision is
//! asserted; the rule attribution lives in the S6-1 matrix, read from src);
//! verdicts with `TMPDIR` set or a session id; relative spellings other than
//! the few Project twins; symlinked targets; that a cell not listed here
//! keeps its verdict; anything about Scratch / OwnWorktree / System /
//! Workflow / Persistence / Unresolved cells.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self) -> PathBuf {
        self.base
            .join("src")
            .join(".harness-worktrees")
            .join("wt-other")
    }
    fn other(&self) -> PathBuf {
        self.home().join("docs")
    }
    fn root(&self, cell: Cell) -> PathBuf {
        match cell {
            Cell::Project => self.repo(),
            Cell::Other => self.other(),
            Cell::OtherWorktree => self.wt(),
        }
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = git_env::pinned(dir);
        cmd.args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home());
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(
            out.status.success(),
            "fixture git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum Cell {
    Project,
    Other,
    OtherWorktree,
}

const CELLS: [Cell; 3] = [Cell::Project, Cell::Other, Cell::OtherWorktree];

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn seed_tracked(root: &Path) {
    write(&root.join("t.txt"), "a\n");
    write(&root.join("m.txt"), "a\n");
    write(&root.join("src.txt"), "a\n");
    write(&root.join("srcdir").join("s.txt"), "a\n");
    write(&root.join("dir").join("a.txt"), "a\n");
    write(&root.join(".claude").join("settings.json"), "{}\n");
}

fn dirty(root: &Path) {
    write(&root.join("m.txt"), "changed\n");
    write(&root.join(".claude").join("settings.json"), "{\"x\":1}\n");
    write(&root.join("u.txt"), "a\n");
    write(&root.join("udir").join("x"), "a\n");
    std::fs::create_dir_all(root.join("edir")).unwrap();
}

fn fixture(tag: &str, dirty_tree: bool) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "s6-tablec-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("src").join(".harness-worktrees")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    let fx = Fx { base };
    let repo = fx.repo();
    seed_tracked(&repo);
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "other"],
    );
    seed_tracked(&fx.other());
    if dirty_tree {
        dirty(&repo);
        dirty(&wt);
        dirty(&fx.other());
    }
    fx
}

/// Run the hook once; `(decision, reason)`. A non-zero exit panics; only then
/// is an empty stdout read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.repo();
    let mut p = serde_json::json!({
        "session_id": "s6-tablec-characterize-20261120",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git_env::scrub(&mut cmd);
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// One B.1 / B.4 row: `cmd` is a template, `{R}` = the cell root (absolute).
/// `exp` = expected `"main/subagent"` per cell, in `CELLS` order
/// (Project, Other, OtherWorktree); `"-"` = not applicable.
struct Row {
    effect: &'static str,
    cond: &'static str,
    cmd: &'static str,
    exp: [&'static str; 3],
}

/// Allow in both modes.
const A: &str = "allow/allow";
/// Ask (main) / Deny (subagent).
const AD: &str = "ask/deny";
/// Deny in both modes.
const D: &str = "deny/deny";
/// Row not applicable to the cell (not run).
const NA: &str = "-";

/// Run every applicable (row, cell), print each measurement, and fail once
/// with every mismatch named by table, cell, effect and condition.
fn check(table: &str, fx: &Fx, rows: &[Row]) {
    let mut bad = Vec::new();
    let mut n = 0usize;
    for row in rows {
        for (i, cell) in CELLS.iter().enumerate() {
            let exp = row.exp[i];
            if exp == "-" {
                continue;
            }
            let root = fx.root(*cell);
            let command = row.cmd.replace("{R}", root.to_str().unwrap());
            let (dm, rm) = run_as(fx, &command, false);
            let (ds, rs) = run_as(fx, &command, true);
            let got = format!("{dm}/{ds}");
            n += 1;
            println!(
                "MEASURE table={table} cell={cell:?} effect={} cond={:?} cmd={:?} => {got}\n    main: {rm}\n    sub:  {rs}",
                row.effect, row.cond, row.cmd
            );
            if got != exp {
                bad.push(format!(
                    "[{table}] cell={cell:?} effect={} condition={:?}: {command:?} expected {exp}, got {got}\n      main reason: {rm}\n      sub reason:  {rs}",
                    row.effect, row.cond
                ));
            }
        }
    }
    assert!(n > 0, "[{table}] no (row, cell) was run");
    assert!(
        bad.is_empty(),
        "[{table}] {} of {n} characterization pins moved:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

// ---------------------------------------------------------------------------
// B.1 rows x {Project, Other, OtherWorktree}, dirty fixture.
// ---------------------------------------------------------------------------

//                                                       exp: [Project, Other, OtherWorktree]
#[rustfmt::skip]
const B1_ROWS: &[Row] = &[
    // ---- Delete, recursive ----
    // Project: deletion::judge_rm recoverability (tracked clean -> Allow,
    // untracked -> Deny, absent -> FallThrough -> confined Ask). Other: shape
    // Deny. OtherWorktree: worktree-storage Allow (ruling 9 as corrected).
    Row { effect: "Delete rec", cond: "tracked clean dir", cmd: "rm -r {R}/dir", exp: [A, D, A] },
    Row { effect: "Delete rec", cond: "tracked clean dir, -f", cmd: "rm -rf {R}/dir", exp: [A, D, A] },
    Row { effect: "Delete rec", cond: "untracked dir", cmd: "rm -r {R}/udir", exp: [D, D, A] },
    Row { effect: "Delete rec", cond: "absent dir, -f", cmd: "rm -rf {R}/newdir", exp: [AD, D, A] },
    Row { effect: "Delete rec", cond: "mv source tracked file, dest absent", cmd: "mv {R}/t.txt {R}/new.txt", exp: [A, A, A] },
    Row { effect: "Delete rec", cond: "mv source untracked file, dest absent", cmd: "mv {R}/u.txt {R}/new.txt", exp: [A, A, A] },
    Row { effect: "Delete rec", cond: "mv source untracked dir, dest absent", cmd: "mv {R}/udir {R}/newdir", exp: [A, A, A] },
    Row { effect: "Delete rec", cond: "find -delete, untracked dir root", cmd: "find {R}/udir -delete", exp: [AD, D, D] },
    Row { effect: "Delete rec", cond: "find -delete, tracked dir root", cmd: "find {R}/dir -delete", exp: [AD, D, D] },
    Row { effect: "Delete rec", cond: "rsync --delete into untracked dir", cmd: "rsync -a --delete {R}/srcdir/ {R}/udir/", exp: [AD, D, D] },
    // ---- Delete, non-recursive ----
    Row { effect: "Delete non-rec", cond: "tracked clean file", cmd: "rm {R}/t.txt", exp: [A, A, A] },
    Row { effect: "Delete non-rec", cond: "tracked modified file", cmd: "rm {R}/m.txt", exp: [A, A, A] },
    Row { effect: "Delete non-rec", cond: "untracked file, -f", cmd: "rm -f {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Delete non-rec", cond: "absent file, -f", cmd: "rm -f {R}/new.txt", exp: [A, A, A] },
    Row { effect: "Delete non-rec", cond: "rmdir empty untracked dir", cmd: "rmdir {R}/edir", exp: [A, A, A] },
    Row { effect: "Delete non-rec", cond: "unlink untracked file", cmd: "unlink {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Delete non-rec", cond: "unlink tracked clean file", cmd: "unlink {R}/t.txt", exp: [A, A, A] },
    // ---- Truncate ----
    Row { effect: "Truncate", cond: "tee, target absent", cmd: "echo x | tee {R}/new.txt", exp: [A, A, A] },
    Row { effect: "Truncate", cond: "tee, target existing untracked", cmd: "echo x | tee {R}/u.txt", exp: [D, D, D] },
    Row { effect: "Truncate", cond: "tee, target existing tracked clean", cmd: "echo x | tee {R}/t.txt", exp: [D, D, D] },
    Row { effect: "Truncate", cond: "truncate, target existing untracked", cmd: "truncate -s 0 {R}/u.txt", exp: [AD, D, D] },
    Row { effect: "Truncate", cond: "truncate, target absent", cmd: "truncate -s 0 {R}/new.txt", exp: [AD, D, D] },
    Row { effect: "Truncate", cond: "dd of=, target absent", cmd: "dd if=/dev/zero of={R}/new.txt count=1", exp: [D, D, D] },
    Row { effect: "Truncate", cond: "dd of=, target existing untracked", cmd: "dd if=/dev/zero of={R}/u.txt count=1", exp: [D, D, D] },
    Row { effect: "Truncate", cond: "cp dest absent", cmd: "cp {R}/src.txt {R}/new.txt", exp: [A, A, A] },
    Row { effect: "Truncate", cond: "cp dest existing untracked", cmd: "cp {R}/src.txt {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Truncate", cond: "cp dest existing tracked clean", cmd: "cp {R}/src.txt {R}/t.txt", exp: [A, A, A] },
    Row { effect: "Truncate", cond: "mv dest existing untracked (source tracked)", cmd: "mv {R}/t.txt {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Truncate", cond: "install dest absent", cmd: "install {R}/src.txt {R}/new.txt", exp: [A, A, A] },
    Row { effect: "Truncate", cond: "install -m dest existing untracked", cmd: "install -m 644 {R}/src.txt {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Truncate", cond: "find -fprint, target absent", cmd: "find {R}/dir -fprint {R}/new.txt", exp: [A, A, A] },
    // ---- Append ----
    Row { effect: "Append", cond: "tee -a, target existing untracked", cmd: "echo x | tee -a {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Append", cond: "tee -a, target absent", cmd: "echo x | tee -a {R}/new.txt", exp: [A, A, A] },
    // ---- Rewrite ----
    Row { effect: "Rewrite", cond: "dd conv=notrunc, target existing untracked", cmd: "dd if=/dev/zero of={R}/u.txt conv=notrunc count=1", exp: [D, D, D] },
    Row { effect: "Rewrite", cond: "sed -i '' (BSD), literal s///, untracked", cmd: "sed -i '' 's/a/b/' {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Rewrite", cond: "sed -i (GNU), literal s///, tracked clean", cmd: "sed -i 's/a/b/' {R}/t.txt", exp: [A, A, A] },
    // ---- Metadata ----
    Row { effect: "Metadata", cond: "chmod +x, untracked file", cmd: "chmod +x {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Metadata", cond: "chmod +x, tracked clean file", cmd: "chmod +x {R}/t.txt", exp: [A, A, A] },
    Row { effect: "Metadata", cond: "chmod 644, untracked file", cmd: "chmod 644 {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Metadata", cond: "chmod 000 (removes read/exec), untracked file", cmd: "chmod 000 {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Metadata", cond: "chmod -R 755, untracked dir", cmd: "chmod -R 755 {R}/udir", exp: [AD, D, D] },
    Row { effect: "Metadata", cond: "chmod -R 000, untracked dir", cmd: "chmod -R 000 {R}/udir", exp: [AD, D, D] },
    Row { effect: "Metadata", cond: "chown, untracked file", cmd: "chown nobody {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Metadata", cond: "chown -R, untracked dir", cmd: "chown -R nobody {R}/udir", exp: [AD, D, D] },
    Row { effect: "Metadata", cond: "chgrp, untracked file", cmd: "chgrp staff {R}/u.txt", exp: [A, A, A] },
    // ---- Link ----
    Row { effect: "Link", cond: "ln -s, link name absent", cmd: "ln -s {R}/src.txt {R}/newlink", exp: [A, A, A] },
    Row { effect: "Link", cond: "ln -sf, link name existing untracked", cmd: "ln -sf {R}/src.txt {R}/u.txt", exp: [A, A, A] },
    Row { effect: "Link", cond: "ln (hard), link name absent", cmd: "ln {R}/src.txt {R}/newhard", exp: [A, A, A] },
    // ---- CreateTree ----
    Row { effect: "CreateTree", cond: "cp -r, dest absent", cmd: "cp -r {R}/srcdir {R}/newdir", exp: [A, AD, A] },
    Row { effect: "CreateTree", cond: "cp -R, dest existing untracked dir", cmd: "cp -R {R}/srcdir {R}/udir", exp: [A, AD, A] },
    Row { effect: "CreateTree", cond: "install -d, absent dir", cmd: "install -d {R}/newdir", exp: [A, A, A] },
];

#[test]
fn b1_rows_project_other_otherworktree() {
    let fx = fixture("b1", true);
    check("B1", &fx, B1_ROWS);
}

// ---------------------------------------------------------------------------
// Project relative spellings (cwd = repo main tree), dirty fixture. Other /
// OtherWorktree have no relative spelling from this cwd ("-").
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const PROJECT_RELATIVE_ROWS: &[Row] = &[
    Row { effect: "Delete rec", cond: "relative, tracked clean dir", cmd: "rm -rf dir", exp: [A, NA, NA] },
    Row { effect: "Delete rec", cond: "relative, untracked dir", cmd: "rm -rf udir", exp: [D, NA, NA] },
    Row { effect: "Delete non-rec", cond: "relative, tracked modified file", cmd: "rm m.txt", exp: [A, NA, NA] },
    Row { effect: "Truncate", cond: "relative, tee existing untracked", cmd: "echo x | tee u.txt", exp: [D, NA, NA] },
    Row { effect: "Truncate", cond: "relative, truncate existing untracked", cmd: "truncate -s 0 u.txt", exp: [AD, NA, NA] },
    Row { effect: "Metadata", cond: "relative, chmod -R 755 untracked dir", cmd: "chmod -R 755 udir", exp: [AD, NA, NA] },
    Row { effect: "CreateTree", cond: "relative, cp -r dest absent", cmd: "cp -r srcdir newdir", exp: [A, NA, NA] },
];

#[test]
fn project_relative_twins() {
    let fx = fixture("rel", true);
    check("ProjectRelative", &fx, PROJECT_RELATIVE_ROWS);
}

// ---------------------------------------------------------------------------
// B.4 git rows (GitDiscard / GitStage / git clean Delete) x {Project,
// OtherWorktree}; Other has no repository ("-"). Run in a CLEAN fixture (no
// modification, nothing untracked) and in a DIRTY one: the reversible probe
// condition.
// ---------------------------------------------------------------------------

/// The `-C` spelling, CLEAN fixture. Project: the `-C` global option defeats
/// the reversible probe ("the repository base is moved by a global option"),
/// so Project gives the same verdict clean and dirty. OtherWorktree (a linked
/// worktree that is not the session's own): the clean checkout keeps the
/// `managed_worktree_git` Allow for every discard.
#[rustfmt::skip]
const GIT_ROWS_CLEAN: &[Row] = &[
    Row { effect: "GitDiscard", cond: "checkout -- tracked clean file", cmd: "git -C {R} checkout -- t.txt", exp: [AD, NA, A] },
    Row { effect: "GitDiscard", cond: "checkout -- m.txt (clean)", cmd: "git -C {R} checkout -- m.txt", exp: [AD, NA, A] },
    Row { effect: "GitDiscard", cond: "checkout . (whole tree)", cmd: "git -C {R} checkout .", exp: [D, NA, A] },
    Row { effect: "GitDiscard", cond: "restore m.txt (clean)", cmd: "git -C {R} restore m.txt", exp: [D, NA, A] },
    Row { effect: "GitDiscard", cond: "restore --staged only", cmd: "git -C {R} restore --staged m.txt", exp: [A, NA, A] },
    Row { effect: "GitDiscard", cond: "reset --hard (work tree)", cmd: "git -C {R} reset --hard", exp: [D, NA, A] },
    Row { effect: "Delete rec", cond: "git clean -fd (work tree)", cmd: "git -C {R} clean -fd", exp: [AD, NA, A] },
    Row { effect: "Delete non-rec", cond: "git clean -f (work tree)", cmd: "git -C {R} clean -f", exp: [AD, NA, A] },
    Row { effect: "GitStage", cond: "add -A (work tree)", cmd: "git -C {R} add -A", exp: [AD, NA, AD] },
    Row { effect: "GitStage", cond: "add . (work tree)", cmd: "git -C {R} add .", exp: [AD, NA, AD] },
    Row { effect: "GitStage", cond: "add -u (work tree)", cmd: "git -C {R} add -u", exp: [AD, NA, AD] },
    Row { effect: "GitStage", cond: "add -A <pathspec m.txt>", cmd: "git -C {R} add -A m.txt", exp: [A, NA, A] },
];

/// The `-C` spelling, DIRTY fixture. Project: same as clean (see above).
/// OtherWorktree: user ruling 2026-11-20 (backlog 606a93ab) — a working-tree
/// discard into a linked worktree that is not the session's own and has
/// uncommitted changes is Ask (main) / Deny (subagent). Cleanliness is judged
/// for the whole checkout, so `checkout -- t.txt` (t.txt itself clean) is
/// gated too: restrictive, accepted by the ruling. `restore --staged` touches
/// only the index and the GitStage rows are not discards: unchanged.
#[rustfmt::skip]
const GIT_ROWS_DIRTY: &[Row] = &[
    Row { effect: "GitDiscard", cond: "checkout -- tracked clean file, checkout dirty", cmd: "git -C {R} checkout -- t.txt", exp: [AD, NA, AD] },
    Row { effect: "GitDiscard", cond: "checkout -- m.txt (modified)", cmd: "git -C {R} checkout -- m.txt", exp: [AD, NA, AD] },
    Row { effect: "GitDiscard", cond: "checkout . (whole tree)", cmd: "git -C {R} checkout .", exp: [D, NA, AD] },
    Row { effect: "GitDiscard", cond: "restore m.txt (modified)", cmd: "git -C {R} restore m.txt", exp: [D, NA, AD] },
    Row { effect: "GitDiscard", cond: "restore --staged only", cmd: "git -C {R} restore --staged m.txt", exp: [A, NA, A] },
    Row { effect: "GitDiscard", cond: "reset --hard (work tree)", cmd: "git -C {R} reset --hard", exp: [D, NA, AD] },
    Row { effect: "Delete rec", cond: "git clean -fd (work tree)", cmd: "git -C {R} clean -fd", exp: [AD, NA, AD] },
    Row { effect: "Delete non-rec", cond: "git clean -f (work tree)", cmd: "git -C {R} clean -f", exp: [AD, NA, AD] },
    Row { effect: "GitStage", cond: "add -A (work tree)", cmd: "git -C {R} add -A", exp: [AD, NA, AD] },
    Row { effect: "GitStage", cond: "add . (work tree)", cmd: "git -C {R} add .", exp: [AD, NA, AD] },
    Row { effect: "GitStage", cond: "add -u (work tree)", cmd: "git -C {R} add -u", exp: [AD, NA, AD] },
    Row { effect: "GitStage", cond: "add -A <pathspec m.txt>", cmd: "git -C {R} add -A m.txt", exp: [A, NA, A] },
];

#[test]
fn git_rows_clean_tree() {
    let fx = fixture("git-clean", false);
    check("GitClean", &fx, GIT_ROWS_CLEAN);
}

#[test]
fn git_rows_dirty_tree() {
    let fx = fixture("git-dirty", true);
    check("GitDirty", &fx, GIT_ROWS_DIRTY);
}

/// Project cwd spelling (no `-C`): the work tree is the session cwd. Here
/// the reversible probe decides `checkout -- <path>`: clean -> Allow, the
/// same path modified -> Deny (`checkout_pathspec_verdict`).
#[rustfmt::skip]
const GIT_CWD_ROWS_CLEAN: &[Row] = &[
    Row { effect: "GitDiscard", cond: "cwd form, checkout -- m.txt, m.txt clean", cmd: "git checkout -- m.txt", exp: [A, NA, NA] },
    Row { effect: "GitDiscard", cond: "cwd form, reset --hard, tree clean", cmd: "git reset --hard", exp: [D, NA, NA] },
    Row { effect: "GitStage", cond: "cwd form, add -A, tree clean", cmd: "git add -A", exp: [AD, NA, NA] },
];

#[rustfmt::skip]
const GIT_CWD_ROWS_DIRTY: &[Row] = &[
    Row { effect: "GitDiscard", cond: "cwd form, checkout -- m.txt, m.txt modified", cmd: "git checkout -- m.txt", exp: [D, NA, NA] },
    Row { effect: "GitDiscard", cond: "cwd form, reset --hard, tree dirty", cmd: "git reset --hard", exp: [D, NA, NA] },
    Row { effect: "GitStage", cond: "cwd form, add -A, tree dirty", cmd: "git add -A", exp: [AD, NA, NA] },
];

#[test]
fn git_cwd_rows_clean_tree() {
    let fx = fixture("gitcwd-clean", false);
    check("GitCwdClean", &fx, GIT_CWD_ROWS_CLEAN);
}

#[test]
fn git_cwd_rows_dirty_tree() {
    let fx = fixture("gitcwd-dirty", true);
    check("GitCwdDirty", &fx, GIT_CWD_ROWS_DIRTY);
}

// ---------------------------------------------------------------------------
// Protected x GitDiscard / GitStage: `.claude/settings.json` (a
// `PROTECTED_GLOBS` entry, tracked) in the Project root and in the sibling
// worktree (Table C: Protected is lifted inside a linked worktree checkout —
// the OtherWorktree column measures what that gives today).
// ---------------------------------------------------------------------------

/// CLEAN fixture. Note the OtherWorktree column is NOT uniform: `checkout` is
/// Deny (`protected_disarm_deny`) while `restore` of the same file is Allow
/// (`managed_worktree_git` is consulted before any protected check in the
/// `restore` arm, and the clean checkout keeps that Allow). Pinned as
/// measured, not endorsed.
#[rustfmt::skip]
const PROTECTED_GIT_ROWS_CLEAN: &[Row] = &[
    Row { effect: "GitDiscard", cond: "Protected, checkout -- .claude/settings.json", cmd: "git -C {R} checkout -- .claude/settings.json", exp: [D, NA, D] },
    Row { effect: "GitDiscard", cond: "Protected, restore .claude/settings.json", cmd: "git -C {R} restore .claude/settings.json", exp: [D, NA, A] },
    Row { effect: "GitDiscard", cond: "Protected, checkout HEAD -- .claude/settings.json", cmd: "git -C {R} checkout HEAD -- .claude/settings.json", exp: [D, NA, D] },
    Row { effect: "GitStage", cond: "Protected, add -A .claude/settings.json", cmd: "git -C {R} add -A .claude/settings.json", exp: [A, NA, A] },
    Row { effect: "GitStage", cond: "Protected, add -u .claude", cmd: "git -C {R} add -u .claude", exp: [A, NA, A] },
];

/// DIRTY fixture: as clean, except OtherWorktree `restore` — user ruling
/// 2026-11-20 (backlog 606a93ab): a discard into a linked worktree that is
/// not the session's own and has uncommitted changes is Ask (main) / Deny
/// (subagent). `checkout` stays Deny (`protected_disarm_deny`).
#[rustfmt::skip]
const PROTECTED_GIT_ROWS_DIRTY: &[Row] = &[
    Row { effect: "GitDiscard", cond: "Protected, checkout -- .claude/settings.json", cmd: "git -C {R} checkout -- .claude/settings.json", exp: [D, NA, D] },
    Row { effect: "GitDiscard", cond: "Protected, restore .claude/settings.json (modified)", cmd: "git -C {R} restore .claude/settings.json", exp: [D, NA, AD] },
    Row { effect: "GitDiscard", cond: "Protected, checkout HEAD -- .claude/settings.json", cmd: "git -C {R} checkout HEAD -- .claude/settings.json", exp: [D, NA, D] },
    Row { effect: "GitStage", cond: "Protected, add -A .claude/settings.json", cmd: "git -C {R} add -A .claude/settings.json", exp: [A, NA, A] },
    Row { effect: "GitStage", cond: "Protected, add -u .claude", cmd: "git -C {R} add -u .claude", exp: [A, NA, A] },
];

#[test]
fn protected_git_rows_clean_tree() {
    let fx = fixture("prot-clean", false);
    check("ProtectedGitClean", &fx, PROTECTED_GIT_ROWS_CLEAN);
}

#[test]
fn protected_git_rows_dirty_tree() {
    let fx = fixture("prot-dirty", true);
    check("ProtectedGitDirty", &fx, PROTECTED_GIT_ROWS_DIRTY);
}
