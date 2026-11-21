#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Mechanism RED, user ruling 2026-11-21 (verbatim choice:
//! 「非破壊はAllow、他はDeny」), refined the same day (verbatim:
//! 「またaskされてたけど、これもallowに」): in a session that is a REGISTERED
//! `/flow` driver, blastguard must never answer `permissionDecision: "ask"`.
//! Statically-safe cases → allow; every other undetermined case → deny (the
//! reason recorded in the block log); system harm stays deny. (A later
//! message 「またpermissionがきている、これらはすべてAllowし、ださない」 offered
//! "every Ask → Allow"; the user then said 「あるいはさきほどの裁定にしたがう」 and
//! the coordinator chose this fail-closed reading — final spec.)
//!
//! Statically safe here = a write target that resolves statically into the
//! session's own worktree or scratchpad, a known read-only command, a direct
//! run of a script in the session scratchpad, a binary built under the
//! session's own worktree `target/`. Rows: `python3 <elsewhere>/tool.py` (a
//! program file outside scratchpad and worktree) → deny (logged); the
//! scratchpad script and the own-target binary → allow; a heredoc write into
//! the own worktree followed by an unmodelled `git commit` → allow (guard: it
//! is already Allow without a driver); `rm -rf /etc` → deny. (`B=/bin/ls;
//! "$B" x` from the brief was measured Allow at 3c792ee32, so it is not used.)
//!
//! The refinement names two shapes that raised a prompt in main on
//! 2026-11-21 (session c1b3e891, main 366eab917) and must be ALLOW (not deny,
//! not ask) in a driver session:
//!   * `python3 <session scratchpad>/probe.py <scratchpad>/a1.json` — reason
//!     then: "`python3` runs the program file `…/scratchpad/probe.py`, whose
//!     text is not in the command — blastguard cannot judge what it would do
//!     (Table B.5: Undetermined)"
//!   * `env -i HOME=<scratchpad>/h PATH=/usr/bin:/bin … target/debug/blastguard
//!     < <scratchpad>/p/01.json` (cwd = the session's own worktree) — reason
//!     then: "direct execution of the program file `target/debug/blastguard`,
//!     whose text is not in the command — blastguard cannot judge what it
//!     would do (Table B.5: Undetermined)"
//!
//! Driver registration is written exactly where and how `backlog driver
//! register --session-id S --project P` writes it (`crates/backlog/src/
//! driver.rs` at 366eab917): `$HOME/.backlog/drivers/<project_slug(P)>/
//! <fnv1a64(S) as %016x>.driver`, JSON `DriverInfo {session_id, pid, project,
//! registered_at, heartbeat_at}`, live while `now - heartbeat_at <= 1800`.
//! `project_slug(P)` = `%016x` of `fnv1a64(canonical P)`, and for a linked
//! worktree the canonical project is its MAIN working tree's canonicalized
//! path (`store::canonical_project_id`). The fixture registers the main tree
//! and runs the hook with cwd = the linked worktree, as `/flow` does.
//!
//! Written by an independent test author (not the implementer, CLAUDE.md
//! §2(a)). Nothing is executed by the hook.
//!
//! Controls (pass now and must keep passing): the same inputs with NO driver
//! registered, and with a driver registered for a DIFFERENT session id, stay
//! "ask" — the driver mode must be keyed to the session, not global.
//!
//! DOES NOT PROVE: subagent mode; a stale
//! (>1800 s) registration; the exact reason text; that the block log entry's
//! schema is right (only that the command text was recorded).

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/fixed_tmp.rs"]
mod fixed_tmp;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SESSION: &str = "flow-driver-never-asks-20261121";

struct Fx {
    base: PathBuf,
    tmp_root: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.tmp_root);
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
            .join("wt-own")
    }
    /// The session scratchpad, spelled the way Claude Code lays it out:
    /// `/private/tmp/claude-<uid>/<cwd with / → ->/<session id>/scratchpad`.
    /// A directory that is neither the scratchpad nor the own worktree.
    fn elsewhere(&self) -> PathBuf {
        self.base.join("elsewhere")
    }
    fn scratch(&self) -> PathBuf {
        self.tmp_root.join(SESSION).join("scratchpad")
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

    /// Register `session` as a live `/flow` driver of this fixture's project,
    /// byte-for-byte the layout `backlog driver register` writes.
    fn register_driver(&self, session: &str) {
        let project = self.repo().canonicalize().unwrap();
        let project = project.to_string_lossy().into_owned();
        let slug = format!("{:016x}", harness_core::hash::fnv1a64(project.as_bytes()));
        let file = format!(
            "{:016x}.driver",
            harness_core::hash::fnv1a64(session.as_bytes())
        );
        let dir = self.home().join(".backlog").join("drivers").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let info = serde_json::json!({
            "session_id": session,
            "pid": std::process::id(),
            "project": project,
            "registered_at": now,
            "heartbeat_at": now,
        });
        std::fs::write(dir.join(file), serde_json::to_string_pretty(&info).unwrap()).unwrap();
    }

    fn block_log_text(&self) -> String {
        let dir = self.home().join(".blastguard").join("blocks");
        let mut all = String::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                all.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
            }
        }
        all
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("flow-driver-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home")).unwrap();
    let uid = std::fs::metadata(base.join("home")).unwrap().uid();
    // The real layout is `/private/tmp/claude-<uid>/<cwd slug>/<session>/
    // scratchpad`. `claude-<uid>` is shared with live sessions, so only the
    // `<cwd slug>` dir is the fixture's own (unique: the slug spells this
    // run's base path) and only it is removed in Drop.
    let wt_spelling = base
        .join("src")
        .join(".harness-worktrees")
        .join("wt-own")
        .to_string_lossy()
        .replace('/', "-");
    let tmp_root = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("claude-{uid}"))
        .join(wt_spelling);
    let _ = std::fs::remove_dir_all(&tmp_root);
    for d in [
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base, tmp_root };
    let repo = fx.repo();
    std::fs::write(repo.join("README"), "x\n").unwrap();
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    // The session's own build output and scratchpad files the commands name.
    let bin = wt.join("target").join("debug").join("blastguard");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(fx.elsewhere()).unwrap();
    std::fs::write(fx.elsewhere().join("tool.py"), "print(1)\n").unwrap();
    let s = fx.scratch();
    std::fs::create_dir_all(s.join("p")).unwrap();
    std::fs::create_dir_all(s.join("h")).unwrap();
    std::fs::write(s.join("probe.py"), "import sys\nprint(sys.argv)\n").unwrap();
    std::fs::write(s.join("a1.json"), "[]\n").unwrap();
    std::fs::write(s.join("p").join("01.json"), "{}\n").unwrap();
    fx
}

/// Run the hook once as main-session `SESSION` (cwd = `CLAUDE_PROJECT_DIR`
/// = own linked worktree). Empty stdout is pass-through = allow.
fn run_main(fx: &Fx, command: &str) -> (String, String) {
    let cwd = fx.wt();
    let p = serde_json::json!({
        "session_id": SESSION,
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", fx.base.join("tmpdir"))
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

/// A generic undetermined, NOT statically safe case: a program file outside
/// both the session scratchpad and the own worktree (Table B.5). The brief's
/// `B=/bin/ls; "$B" x` was measured Allow at 3c792ee32 (the value resolves
/// statically), so it cannot stand for "currently Ask".
fn undetermined_cmd(fx: &Fx) -> String {
    format!("python3 {}/tool.py", fx.elsewhere().display())
}

/// A heredoc write into the session's own worktree (relative to cwd = the
/// worktree, so it resolves statically there) followed by a `git commit`
/// whose message names `rsync` and whose trailer holds `$TASK_ID` — the shape
/// of the "may create or rename an entry on this path and is not modelled"
/// Ask of 2026-11-21. Measured Allow in this fixture at 3c792ee32 (no driver),
/// so it is a guard row only (must stay allow in a driver session), not part
/// of the ask control. The body holds a shell `done` (a Rust test string).
const UNMODELLED_CREATOR_REDIRECT: &str = "cat > t.rs <<'EOF'\n#[test]\nfn t() {\n    let _c = \"for f in a b; do echo $f; done\";\n}\nEOF\ngit add t.rs\ngit commit -qm \"blastguard: RED tests for rsync protected destination\" --trailer \"Condukt-Task: $TASK_ID\"";

fn scratch_script_cmd(fx: &Fx) -> String {
    let s = fx.scratch().to_string_lossy().into_owned();
    format!("python3 {s}/probe.py {s}/a1.json")
}

fn own_target_bin_cmd(fx: &Fx) -> String {
    let s = fx.scratch().to_string_lossy().into_owned();
    format!(
        "env -i HOME={s}/h PATH=/usr/bin:/bin CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=cli target/debug/blastguard < {s}/p/01.json"
    )
}

#[test]
fn registered_driver_session_never_gets_ask() {
    let fx = fixture("drv");
    fx.register_driver(SESSION);
    let mut bad = Vec::new();

    // Generic undetermined (not statically safe): deny, and the deny is
    // recorded in the block log.
    let und = undetermined_cmd(&fx);
    let (d, r) = run_main(&fx, &und);
    if d != "deny" {
        bad.push(format!("{und:?} => {d} ({r}) — expected deny"));
    } else if !fx.block_log_text().contains("tool.py") {
        bad.push(format!(
            "{und:?} => deny but the command is not in the block log"
        ));
    }

    // Statically safe rows: must be allow.
    for c in [
        scratch_script_cmd(&fx),
        own_target_bin_cmd(&fx),
        UNMODELLED_CREATOR_REDIRECT.to_string(),
    ] {
        let (d, r) = run_main(&fx, &c);
        if d != "allow" {
            bad.push(format!("{c:?} => {d} ({r}) — expected allow"));
        }
    }

    // System harm stays deny.
    let (d, r) = run_main(&fx, "rm -rf /etc");
    if d != "deny" {
        bad.push(format!("\"rm -rf /etc\" => {d} ({r}) — expected deny"));
    }

    assert!(
        bad.is_empty(),
        "registered /flow driver session:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_no_driver_registered_stays_ask() {
    let fx = fixture("none");
    let cmds = [
        undetermined_cmd(&fx),
        scratch_script_cmd(&fx),
        own_target_bin_cmd(&fx),
    ];
    let bad: Vec<String> = cmds
        .iter()
        .filter_map(|c| {
            let (d, r) = run_main(&fx, c);
            (d != "ask").then(|| format!("{c:?} => {d} ({r})"))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "expected ask (control):\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_driver_for_other_session_stays_ask() {
    let fx = fixture("other");
    fx.register_driver("some-other-session");
    let (d, r) = run_main(&fx, &undetermined_cmd(&fx));
    assert_eq!(
        d, "ask",
        "another session's driver must not change this session: {r}"
    );
}
