#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: `rsync --delete` / `--delete-*` is a Table B `Delete` row (user ruling
//! 2026-11-18): "rsync --delete / --delete-* は Table B の Delete 行。宛先は rm
//! と同じく Table C で判定する（scratch と自分の worktree は Allow、それ以外は
//! rm と同じ結果）".
//!
//! Written by an independent test author (not the implementer; CLAUDE.md
//! §2(a)) against origin/main a19a34553 (blastguard 0.2.702).
//!
//! Observed 2026-11-18 at a19a34553 (probe of the built hook binary, same env
//! as this harness), main / subagent:
//!   rsync -a --delete src/ ~/Documents/            allow / allow
//!   rsync -a --delete-after src/ ~/Documents/      allow / allow
//!   rsync -a --delete-excluded src/ ~/Documents/   allow / allow
//!   rm -rf ~/Documents/x                           deny / deny
//! The rsync arm (`detect/system_paths.rs` `rsync_mutated`) judges the
//! destination only against system directories, so a home subdirectory that
//! `rm` refuses is Allow for `rsync --delete`.
//!
//! Not duplicated here: the home directory itself and `~/.ssh` as the
//! destination (backlog 7f9f2457, RED 3cc3c61e5).
//!
//! Content rule: every target is a non-existent sentinel path; the hook only
//! JUDGES the command text, nothing is executed.
//!
//! Harness: same as `s3_home_tilde_branches_20261118.rs` — the hook binary
//! runs with `env_clear`, `HOME=<fixture>/home`, `CLAUDECODE=1`,
//! `CLAUDE_CODE_ENTRYPOINT=cli`, `CLAUDE_PROJECT_DIR` = the payload cwd,
//! `BLASTGUARD_APPROVALS_DIR=<fixture>/store`, no `TMPDIR` unless a test sets
//! one, in main mode and subagent mode (`agent_id` in the payload).
//!
//! DOES NOT PROVE (whole file): which rule produced a verdict; remote
//! destinations (`host:/path`); `--del` / `--delete-before` /
//! `--delete-during` / `--delete-delay` spellings; a `$TMPDIR/...` SPELLED
//! destination (Table A resolves only HOME, so `$TMPDIR` is Unresolved —
//! observed: rsync ask/deny, `rm -rf $TMPDIR/x` deny/deny — not pinned here);
//! OtherWorktree / Project destinations.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    /// A directory handed to the hook as `TMPDIR` (a Scratch root, ruling 8).
    fn tmpdir(&self) -> PathBuf {
        let t = self.base.join("tmpd");
        std::fs::create_dir_all(&t).unwrap();
        t
    }
    /// A linked-worktree-shaped checkout under the hermetic
    /// `$HOME/.condukt/worktrees` storage root, used as the session cwd.
    fn own_worktree(&self) -> PathBuf {
        let wt = self
            .home
            .join(".condukt")
            .join("worktrees")
            .join("bg-rsync-tableb-wt");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(
            wt.join(".git"),
            "gitdir: /nonexistent/bg-rsync-tableb/.git/worktrees/bg-rsync-tableb-wt\n",
        )
        .unwrap();
        wt
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "rsync-tableb-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base, home }
}

/// Run the hook once; `(decision, reason)`. Non-zero exit panics before an
/// empty stdout can be read as Allow; non-JSON stdout panics.
fn run_as(
    fx: &Fx,
    cwd: &Path,
    command: &str,
    subagent: bool,
    tmpdir: Option<&Path>,
) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "rsync-delete-table-b-20261118",
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
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(t) = tmpdir {
        cmd.env("TMPDIR", t);
    }
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

fn allow_at(fx: &Fx, cwd: &Path, command: &str, tmpdir: Option<&Path>) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent, tmpdir);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

/// The ruling's "rm と同じ結果": in each mode the rsync verdict equals the
/// verdict of the `rm` reference, and neither is Allow.
fn same_as_rm_and_not_allow(fx: &Fx, cwd: &Path, rsync: &str, rm: &str) {
    for subagent in [false, true] {
        let (dr, rr) = run_as(fx, cwd, rm, subagent, None);
        assert!(
            dr == "ask" || dr == "deny",
            "subagent={subagent}: reference {rm:?} expected Ask or Deny, got {dr} ({rr})"
        );
        let (ds, rs) = run_as(fx, cwd, rsync, subagent, None);
        assert!(
            ds == "ask" || ds == "deny",
            "subagent={subagent}: {rsync:?} expected Ask or Deny (like {rm:?} = {dr}), got {ds} ({rs})"
        );
        assert_eq!(
            ds, dr,
            "subagent={subagent}: {rsync:?} ({ds}: {rs}) differs from {rm:?} ({dr}: {rr})"
        );
    }
}

// ---------------------------------------------------------------------------
// Other (resolved, outside every root): same verdict as rm.
// ---------------------------------------------------------------------------

/// `~/Documents/` is a home subdirectory that is not a home asset; rsync
/// `--delete` deletes its extraneous contents, so it is judged like
/// `rm -rf ~/Documents/x`.
/// DOES NOT PROVE: that the verdict comes from the Table B row (a stricter
/// unrelated rule would also satisfy it).
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: rsync --delete Table B Delete row (ruling 2026-11-18) - ~/Documents/ destination is Allow, rm -rf ~/Documents/x is Deny"]
fn rsync_delete_into_home_subdir_same_as_rm() {
    let fx = fixture("docs");
    same_as_rm_and_not_allow(
        &fx,
        &fx.proj(),
        "rsync -a --delete src/ ~/Documents/",
        "rm -rf ~/Documents/x",
    );
}

/// `--delete-after` is a `--delete-*` flag: same row.
/// DOES NOT PROVE: the other `--delete-*` spellings.
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: rsync --delete-after Table B Delete row (ruling 2026-11-18) - ~/Documents/ destination is Allow, rm -rf ~/Documents/x is Deny"]
fn rsync_delete_after_into_home_subdir_same_as_rm() {
    let fx = fixture("docs-after");
    same_as_rm_and_not_allow(
        &fx,
        &fx.proj(),
        "rsync -a --delete-after src/ ~/Documents/",
        "rm -rf ~/Documents/x",
    );
}

/// `--delete-excluded` is a `--delete-*` flag: same row.
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: rsync --delete-excluded Table B Delete row (ruling 2026-11-18) - ~/Documents/ destination is Allow, rm -rf ~/Documents/x is Deny"]
fn rsync_delete_excluded_into_home_subdir_same_as_rm() {
    let fx = fixture("docs-excl");
    same_as_rm_and_not_allow(
        &fx,
        &fx.proj(),
        "rsync -a --delete-excluded src/ ~/Documents/",
        "rm -rf ~/Documents/x",
    );
}

// ---------------------------------------------------------------------------
// Scratch: Allow (Table C Scratch x Delete, ruling 5).
// ---------------------------------------------------------------------------

/// Destination under `/tmp`: Allow for all three flags.
/// Observed GREEN at a19a34553 (pins that the new row must keep it).
/// DOES NOT PROVE: `/private/tmp`, `/var/tmp`.
#[test]
fn rsync_delete_into_tmp_allows() {
    let fx = fixture("tmp");
    for flag in ["--delete", "--delete-after", "--delete-excluded"] {
        allow_at(
            &fx,
            &fx.proj(),
            &format!("rsync -a {flag} src/ /tmp/bg-sentinel-rsync-tableb/"),
            None,
        );
    }
}

/// Destination under the hook's `$TMPDIR`, spelled absolutely (Table A
/// resolves only HOME, so the absolute spelling is the one Table C places).
/// Observed GREEN at a19a34553.
/// DOES NOT PROVE: the `$TMPDIR/...` spelling (see file doc).
#[test]
fn rsync_delete_into_tmpdir_absolute_allows() {
    let fx = fixture("tmpdir");
    let t = fx.tmpdir();
    let dest = t.join("bg-sentinel-rsync-tableb");
    for flag in ["--delete", "--delete-after", "--delete-excluded"] {
        allow_at(
            &fx,
            &fx.proj(),
            &format!("rsync -a {flag} src/ {}/", dest.display()),
            Some(&t),
        );
    }
}

// ---------------------------------------------------------------------------
// OwnWorktree: Allow (Table C OwnWorktree x Delete, rulings 5, 9).
// ---------------------------------------------------------------------------

/// Destination inside the checkout containing the session cwd, relative and
/// absolute spellings. Observed GREEN at a19a34553.
/// DOES NOT PROVE: a destination that IS the worktree root.
#[test]
fn rsync_delete_into_own_worktree_allows() {
    let fx = fixture("wt");
    let wt = fx.own_worktree();
    let abs = wt.join("sub").join("bg-sentinel-out");
    for flag in ["--delete", "--delete-after", "--delete-excluded"] {
        allow_at(
            &fx,
            &wt,
            &format!("rsync -a {flag} src/ sub/bg-sentinel-out/"),
            None,
        );
        allow_at(
            &fx,
            &wt,
            &format!("rsync -a {flag} src/ {}/", abs.display()),
            None,
        );
    }
}

// ---------------------------------------------------------------------------
// Control: no delete flag.
// ---------------------------------------------------------------------------

/// Control: `rsync` without a delete flag into scratch stays Allow.
/// DOES NOT PROVE: the no-delete verdict for other destinations.
#[test]
fn control_rsync_without_delete_into_tmp_allows() {
    let fx = fixture("ctl");
    allow_at(
        &fx,
        &fx.proj(),
        "rsync -a src/ /tmp/bg-sentinel-rsync-tableb/",
        None,
    );
}
