// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial probes (2026-11-27) of the 39772e9f fix in
//! `harness_core::attribution` (`cd_step` / `call_named_paths`), which follows
//! a literal `cd`/`pushd` when joining relative Bash write paths.
//!
//! Fixture: same shape as `backlog_20261117_subagent_cd_relative_root.rs`
//! (parent root + subagent worktree `wt`, each with a red check that drops a
//! marker). Transcripts are synthetic: no recorded command is executed; each
//! probe states where bash WOULD land the write and asserts that the gate
//! judges that root.
//!
//! Classes:
//!  - `hole_*` (fail-open): the static cwd is moved to a `cd` target the shell
//!    did NOT stay in (subshell, background job, skipped/failed `cd`,
//!    `pushd +N`, CDPATH), so a write bash lands in the hook root is attributed
//!    elsewhere and SubagentStop exits 0 without running checks.
//!  - `fp_*` (false positive): text that is not a directory change, or a plain
//!    `cd` followed by a redirect/heredoc, is read as an unresolvable `cd`;
//!    `own_written_paths` becomes Undetermined and the gate blocks although
//!    nothing was written in the hook root.
//!  - `control_*`: behaviour the fix gets right.
//!
//! What these probes do NOT prove: that Claude Code records such commands with
//! these cwds (the recorded-cwd shape is the one observed in the RED file's
//! docs), nor what the right fix is. The cross-call probe models the observed
//! subagent behaviour (shell cwd reset between calls, line cwd = parent root);
//! it does not prove what a main-session transcript records after a `cd`.
//!
//! Written by a disinterested verifier (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const AGENT_ID: &str = "a20261127cdhole0001";
const SESSION: &str = "s-cdhole-20261127";

struct Fixture {
    home: PathBuf,
    /// The parent session's worktree root (the hook cwd).
    parent: PathBuf,
    /// The subagent's own worktree (same layout as `parent`).
    wt: PathBuf,
}

struct Outcome {
    code: Option<i32>,
    decision: String,
    stdout: String,
    stderr: String,
}

fn git(dir: &Path, args: &[&str]) {
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

fn line(v: serde_json::Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

fn user_line() -> String {
    line(serde_json::json!({
        "type": "user", "message": {"role": "user", "content": "go"}
    }))
}

/// One Bash tool_use transcript line, recording `cwd` like Claude Code does.
fn bash_use(id: &str, command: &str, cwd: &Path, sidechain: bool) -> String {
    line(serde_json::json!({
        "parentUuid": "p", "isSidechain": sidechain, "cwd": cwd.to_string_lossy(),
        "message": {"model": "claude-opus-5", "id": format!("m-{id}"), "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": "Bash",
                "input": {"command": command}}]}
    }))
}

/// A git repo with `src/lib.rs` and a red check that drops `marker` in `home`.
fn make_repo(home: &Path, name: &str, marker: &str) -> PathBuf {
    let root = home.join(name);
    std::fs::create_dir_all(root.join("src")).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@t.com"]);
    git(&root, &["config", "user.name", "t"]);
    std::fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(
        root.join("donegate.toml"),
        format!(
            "max_attempts = 50\n\n[[check]]\nname = \"marker-check\"\n\
             cmd = \"touch {}; echo 'marker check is red'; false\"\n",
            home.join(marker).display()
        ),
    )
    .unwrap();
    git(&root, &["add", "src/lib.rs", "donegate.toml"]);
    git(
        &root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
    );
    root
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-cdhole-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let parent = make_repo(&home, "parent", "ran-parent");
        let wt = make_repo(&home, "wt", "ran-wt");
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!(
                "trusted = [\"{}\", \"{}\"]\n",
                parent.display(),
                wt.display()
            ),
        )
        .unwrap();
        Fixture { home, parent, wt }
    }

    fn ran(&self, marker: &str) -> bool {
        self.home.join(marker).exists()
    }

    fn wt_s(&self) -> String {
        self.wt.display().to_string()
    }

    fn project_dir(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn parent_transcript(&self, body: &str) -> PathBuf {
        let p = self.project_dir().join("sess.jsonl");
        std::fs::write(&p, format!("{}{body}", user_line())).unwrap();
        p
    }

    /// A sidechain whose Bash calls are `cmds` (one call each), every line
    /// recorded with `cwd` = the parent root (the observed sidechain shape).
    fn sidechain_calls(&self, cmds: &[&str]) -> PathBuf {
        let dir = self.project_dir().join("sess").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = user_line();
        for (i, c) in cmds.iter().enumerate() {
            body.push_str(&bash_use(&format!("t-sub{i}"), c, &self.parent, true));
        }
        let p = dir.join(format!("agent-{AGENT_ID}.jsonl"));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn run(&self, sub: &str, cwd: &Path, payload: serde_json::Value) -> Outcome {
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg(sub)
                .current_dir(cwd)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", SESSION)
                .env_remove("DONEGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
                .env_remove("HARNESS_WORKTREE_ORIGIN_DIR")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("donegate spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
        let decision = v
            .as_ref()
            .and_then(|v| v.get("decision"))
            .and_then(|d| d.as_str())
            .map(String::from)
            .unwrap_or_default();
        Outcome {
            code: out.status.code(),
            decision,
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn subagent_stop(&self, cwd: &Path, agent_t: &Path) -> Outcome {
        let parent_t = self.parent_transcript("");
        self.run(
            "subagent-gate",
            cwd,
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": parent_t.to_string_lossy(),
                "agent_transcript_path": agent_t.to_string_lossy(),
                "agent_id": AGENT_ID,
                "cwd": cwd.to_string_lossy(),
                "hook_event_name": "SubagentStop",
                "stop_hook_active": false,
            }),
        )
    }

    fn parent_stop(&self, parent_t: &Path) -> Outcome {
        self.run(
            "gate",
            &self.parent,
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": parent_t.to_string_lossy(),
                "cwd": self.parent.to_string_lossy(),
                "hook_event_name": "Stop",
                "stop_hook_active": false,
            }),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn dump(o: &Outcome) -> String {
    format!(
        "code={:?} stdout={:?} stderr={:?}",
        o.code, o.stdout, o.stderr
    )
}

/// Bash lands the write in the PARENT root (the hook cwd): its checks run.
fn assert_parent_judged(f: &Fixture, cmds: &[&str]) {
    let agent_t = f.sidechain_calls(cmds);
    let o = f.subagent_stop(&f.parent.clone(), &agent_t);
    assert!(
        f.ran("ran-parent"),
        "bash lands `src/lib.rs` of {cmds:?} in the parent root (hook cwd), yet its \
         checks did not run: {}",
        dump(&o)
    );
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// The subagent wrote nothing in the parent root (hook cwd): no block.
fn assert_parent_not_judged(f: &Fixture, cmd: &str) {
    let agent_t = f.sidechain_calls(&[cmd]);
    let o = f.subagent_stop(&f.parent.clone(), &agent_t);
    assert!(!f.ran("ran-parent"), "{}", dump(&o));
    assert_ne!(
        o.decision,
        "block",
        "{cmd:?} writes nothing in the parent root, yet SubagentStop blocks: {}",
        dump(&o)
    );
}

// ---- controls -------------------------------------------------------------

/// The RED shape with `;` instead of `&&` (the `cd` succeeds) is followed.
#[test]
fn control_semicolon_cd_to_wt_is_followed() {
    let f = Fixture::new();
    let cmd = format!("cd {}; sed -i s/a/b/ src/lib.rs", f.wt_s());
    assert_parent_not_judged(&f, &cmd);
}

/// A write after `cd` back into the parent root judges the parent.
#[test]
fn control_cd_back_into_parent_is_judged() {
    let f = Fixture::new();
    let cmd = format!(
        "cd {} && cd {} && sed -i s/a/b/ src/lib.rs",
        f.wt_s(),
        f.parent.display()
    );
    assert_parent_judged(&f, &[&cmd]);
}

/// `cd ../parent` from the wt (a relative `..` climb) lands in the parent.
#[test]
fn control_cd_dotdot_climb_into_parent_is_judged() {
    let f = Fixture::new();
    let cmd = format!(
        "cd {} && cd ../parent && sed -i s/a/b/ src/lib.rs",
        f.wt_s()
    );
    assert_parent_judged(&f, &[&cmd]);
}

/// `popd` is unresolvable: fail closed (SubagentStop blocks).
#[test]
fn control_popd_is_fail_closed() {
    let f = Fixture::new();
    let cmd = format!(
        "pushd {} && sed -i s/a/b/ src/lib.rs && popd && sed -i s/a/b/ src/lib.rs",
        f.wt_s()
    );
    let agent_t = f.sidechain_calls(&[&cmd]);
    let o = f.subagent_stop(&f.parent.clone(), &agent_t);
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// `cd <wt>` in one Bash call, the write in a LATER call. A subagent's shell
/// cwd is reset between calls (both lines record cwd = parent root), so the
/// write lands in the parent root and must be judged there.
#[test]
fn control_cd_in_earlier_separate_call_does_not_carry_over() {
    let f = Fixture::new();
    let cd = format!("cd {}", f.wt_s());
    assert_parent_judged(&f, &[&cd, "sed -i s/a/b/ src/lib.rs"]);
}

// ---- fail-open holes ------------------------------------------------------

/// `( cd wt && ... ); sed -i src/lib.rs`: the subshell's `cd` does not persist.
#[test]
fn hole_subshell_cd_leaks_into_later_statement() {
    let f = Fixture::new();
    let cmd = format!(
        "(cd {} && sed -i s/a/b/ src/lib.rs); sed -i s/a/b/ src/lib.rs",
        f.wt_s()
    );
    assert_parent_judged(&f, &[&cmd]);
}

/// `cd wt & sed ...`: a background job's `cd` does not move the shell.
#[test]
fn hole_background_cd_does_not_move_cwd() {
    let f = Fixture::new();
    let cmd = format!("cd {} & sed -i s/a/b/ src/lib.rs", f.wt_s());
    assert_parent_judged(&f, &[&cmd]);
}

/// `false && cd wt; sed ...`: the `cd` never runs.
#[test]
fn hole_skipped_conditional_cd() {
    let f = Fixture::new();
    let cmd = format!("false && cd {}; sed -i s/a/b/ src/lib.rs", f.wt_s());
    assert_parent_judged(&f, &[&cmd]);
}

/// `cd wt || sed ...`: the write runs only when the `cd` FAILED.
#[test]
fn hole_write_on_cd_failure_branch() {
    let f = Fixture::new();
    let cmd = format!("cd {} || sed -i s/a/b/ src/lib.rs", f.wt_s());
    assert_parent_judged(&f, &[&cmd]);
}

/// `cd <missing>; sed ...`: the `cd` fails, `;` still runs the write in the
/// original directory.
#[test]
fn hole_failed_cd_to_missing_dir_then_semicolon() {
    let f = Fixture::new();
    let cmd = format!(
        "cd {}/no-such-dir; sed -i s/a/b/ src/lib.rs",
        f.home.display()
    );
    assert_parent_judged(&f, &[&cmd]);
}

/// `pushd wt; pushd +1; sed ...`: `+1` rotates the stack back to the parent.
#[test]
fn hole_pushd_plus_n_rotates_back() {
    let f = Fixture::new();
    let cmd = format!("pushd {}; pushd +1; sed -i s/a/b/ src/lib.rs", f.wt_s());
    assert_parent_judged(&f, &[&cmd]);
}

/// `export CDPATH=<home>; cd wt && sed ...`: bash resolves `wt` through
/// CDPATH to the subagent worktree. Hook cwd = wt must judge wt.
#[test]
fn hole_cdpath_redirects_relative_cd() {
    let f = Fixture::new();
    let cmd = format!(
        "export CDPATH={}; cd wt && sed -i s/a/b/ src/lib.rs",
        f.home.display()
    );
    let agent_t = f.sidechain_calls(&[&cmd]);
    let o = f.subagent_stop(&f.wt.clone(), &agent_t);
    assert!(
        f.ran("ran-wt"),
        "bash (CDPATH) lands the write in {}, yet its checks did not run: {}",
        f.wt.display(),
        dump(&o)
    );
}

// ---- false positives ------------------------------------------------------

/// A commit message containing the word `cd` is not a directory change.
#[test]
fn fp_word_cd_in_commit_message() {
    let f = Fixture::new();
    let cmd = format!("git -C {} commit -qm 'make cd work'", f.wt_s());
    assert_parent_not_judged(&f, &cmd);
}

/// A heredoc body line `then cd somewhere` (data, never executed).
#[test]
fn fp_word_cd_in_heredoc_body() {
    let f = Fixture::new();
    let cmd = format!(
        "cat > {}/notes.md <<'XEOF'\nthen cd somewhere\nXEOF",
        f.wt_s()
    );
    assert_parent_not_judged(&f, &cmd);
}

/// `cd <wt> 2>/dev/null && sed ...`: a literal `cd` with a stderr redirect.
#[test]
fn fp_cd_with_stderr_redirect() {
    let f = Fixture::new();
    let cmd = format!("cd {} 2>/dev/null && sed -i s/a/b/ src/lib.rs", f.wt_s());
    assert_parent_not_judged(&f, &cmd);
}

/// `cd <wt> && cat > src/lib.rs <<'XEOF' ... XEOF`: a heredoc keeps the whole
/// command, which is then read as a `cd` with many arguments.
#[test]
fn fp_cd_then_heredoc_write() {
    let f = Fixture::new();
    let cmd = format!(
        "cd {} && cat > src/lib.rs <<'XEOF'\nfn b() {{}}\nXEOF",
        f.wt_s()
    );
    assert_parent_not_judged(&f, &cmd);
}

/// Parent Stop with a GREEN check, the session's only write a commit whose
/// message contains `cd`: must not block on "cannot be determined".
#[test]
fn fp_parent_stop_blocks_on_word_cd_in_commit_message() {
    let f = Fixture::new();
    std::fs::write(
        f.parent.join("donegate.toml"),
        "max_attempts = 50\n\n[[check]]\nname = \"green\"\ncmd = \"true\"\n",
    )
    .unwrap();
    git(&f.parent, &["add", "donegate.toml"]);
    git(
        &f.parent,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "green"],
    );
    let body = bash_use("t-par", "git commit -qm 'fix cd handling'", &f.parent, false);
    let parent_t = f.parent_transcript(&body);
    let o = f.parent_stop(&parent_t);
    assert_ne!(o.decision, "block", "{}", dump(&o));
}
