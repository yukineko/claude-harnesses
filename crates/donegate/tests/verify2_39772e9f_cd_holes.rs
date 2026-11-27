// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial probes, round 2 (2026-11-27), of the 39772e9f fix at b1981dc2c:
//! `harness_core::attribution::walk_cwd` moves the cwd only for a "certain"
//! `cd` (single stage, no compound keyword in the SAME statement, reached
//! after `;`/newline/`(`/`)` or an all-`&&` chain), widens otherwise.
//!
//! Classes:
//!  - `hole_*` (fail-open): a `cd` the shell never runs is judged certain
//!    because the compound keyword (`then`, `do`, `{`, a `case` pattern, a
//!    function body) sits in an EARLIER statement, or because a top-level
//!    comment's `;` splits it off; or a `cd` is not followed at all in the
//!    unparsed / substitution fallback. The write bash lands in the hook root
//!    is attributed elsewhere and the gate exits 0 without running checks.
//!  - `fp_*`: a `)` in a comment or a `case` pattern pops an empty subshell
//!    stack, `walk_cwd` errs, and SubagentStop blocks on a wt-only write.
//!  - `control_*`: tokenizer cases the fix gets right.
//!
//! Transcripts are synthetic: no recorded command runs. Each probe states
//! where bash WOULD land the write. Not proven: that Claude Code records such
//! commands with these cwds; what the right fix is.
//!
//! Written by a disinterested verifier (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const AGENT_ID: &str = "a20261127cdhole0002";
const SESSION: &str = "s-cdhole2-20261127";

struct Fixture {
    home: PathBuf,
    parent: PathBuf,
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

fn bash_use(id: &str, command: &str, cwd: &Path, sidechain: bool) -> String {
    line(serde_json::json!({
        "parentUuid": "p", "isSidechain": sidechain, "cwd": cwd.to_string_lossy(),
        "message": {"model": "claude-opus-5", "id": format!("m-{id}"), "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": "Bash",
                "input": {"command": command}}]}
    }))
}

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
        let home = std::env::temp_dir().join(format!("dg-cdhole2-{}-{n}", std::process::id()));
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

    fn parent_transcript(&self) -> PathBuf {
        let p = self.project_dir().join("sess.jsonl");
        std::fs::write(&p, user_line()).unwrap();
        p
    }

    /// A sidechain with one Bash call `cmd`, its line recording `cwd`.
    fn sidechain_at(&self, cwd: &Path, cmd: &str) -> PathBuf {
        let dir = self.project_dir().join("sess").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!("{}{}", user_line(), bash_use("t-sub", cmd, cwd, true));
        let p = dir.join(format!("agent-{AGENT_ID}.jsonl"));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn subagent_stop(&self, cwd: &Path, agent_t: &Path) -> Outcome {
        let parent_t = self.parent_transcript();
        let payload = serde_json::json!({
            "session_id": SESSION,
            "transcript_path": parent_t.to_string_lossy(),
            "agent_transcript_path": agent_t.to_string_lossy(),
            "agent_id": AGENT_ID,
            "cwd": cwd.to_string_lossy(),
            "hook_event_name": "SubagentStop",
            "stop_hook_active": false,
        });
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("subagent-gate")
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

/// Line cwd = hook cwd = parent; bash lands the write in the parent root.
fn assert_parent_judged(f: &Fixture, cmd: &str) {
    let agent_t = f.sidechain_at(&f.parent.clone(), cmd);
    let o = f.subagent_stop(&f.parent.clone(), &agent_t);
    assert!(
        f.ran("ran-parent"),
        "bash lands `src/lib.rs` of {cmd:?} in the parent root (hook cwd), yet its \
         checks did not run: {}",
        dump(&o)
    );
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// Line cwd = hook cwd = parent; the write lands only in the wt: no block.
fn assert_parent_not_judged(f: &Fixture, cmd: &str) {
    let agent_t = f.sidechain_at(&f.parent.clone(), cmd);
    let o = f.subagent_stop(&f.parent.clone(), &agent_t);
    assert!(!f.ran("ran-parent"), "{}", dump(&o));
    assert_ne!(
        o.decision,
        "block",
        "{cmd:?} writes only in the wt, yet SubagentStop blocks: {}",
        dump(&o)
    );
}

/// Line cwd = home (the wt's parent dir), hook cwd = wt; bash lands the
/// write in the wt, so the wt's checks must run.
fn assert_wt_judged_from_home(f: &Fixture, cmd: &str) {
    let agent_t = f.sidechain_at(&f.home.clone(), cmd);
    let o = f.subagent_stop(&f.wt.clone(), &agent_t);
    assert!(
        f.ran("ran-wt"),
        "bash lands {cmd:?}'s write in {}, yet its checks did not run: {}",
        f.wt.display(),
        dump(&o)
    );
}

// ---- controls -------------------------------------------------------------

/// `time cd <wt> && sed ...`: `time` runs cd in the current shell.
#[test]
fn control_time_cd_is_followed() {
    let f = Fixture::new();
    assert_parent_not_judged(&f, &format!("time cd {} && sed -i s/a/b/ src/lib.rs", f.wt_s()));
}

/// `! cd <wt>; sed ...`: `!` negates the status; the cd still runs.
#[test]
fn control_bang_cd_is_followed() {
    let f = Fixture::new();
    assert_parent_not_judged(&f, &format!("! cd {}; sed -i s/a/b/ src/lib.rs", f.wt_s()));
}

/// `exec cd <wt>`: a wrapper; undetermined, fail closed.
#[test]
fn control_exec_cd_fails_closed() {
    let f = Fixture::new();
    let cmd = format!("exec cd {}; sed -i s/a/b/ src/lib.rs", f.wt_s());
    let agent_t = f.sidechain_at(&f.parent.clone(), &cmd);
    let o = f.subagent_stop(&f.parent.clone(), &agent_t);
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// `echo a<backslash>;cd <wt>; sed ...`: the escaped `;` is not a separator.
#[test]
fn control_escaped_semicolon_is_not_a_separator() {
    let f = Fixture::new();
    assert_parent_judged(&f, &format!("echo a\x5c;cd {}; sed -i s/a/b/ src/lib.rs", f.wt_s()));
}

/// A quoted `)`/`;` in an argument is not a separator.
#[test]
fn control_quoted_separators_are_not_separators() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("echo 'x); cd {}' \"(;\"; sed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// A heredoc body holding `)` and `cd x` is data.
#[test]
fn control_heredoc_body_paren_and_cd_are_data() {
    let f = Fixture::new();
    let cmd = format!(
        "cat > {}/n.md <<'XEOF'\n)\ncd {}\nXEOF\nsed -i s/a/b/ src/lib.rs",
        f.wt_s(),
        f.wt_s()
    );
    assert_parent_judged(&f, &cmd);
}

/// `if false; then cd <wt>; fi; sed ...` (cd in the `then` statement itself).
#[test]
fn control_then_cd_same_statement_widens() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("if false; then cd {}; fi; sed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// Nested subshells restore the outer cwd at each `)`.
#[test]
fn control_nested_subshells_restore() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("((cd {}) ; true); sed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

// ---- fail-open holes ------------------------------------------------------

/// `then` on its own line: the `cd` on the next line is a statement without a
/// compound word, so it is judged certain although `if false` skips it.
#[test]
fn hole_then_newline_cd_judged_certain() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("if false; then\ncd {}\nfi\nsed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// `then :; cd <wt>; fi`: the `cd` is the second statement of the branch.
#[test]
fn hole_then_second_statement_cd_judged_certain() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("if false; then :; cd {}; fi; sed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// `while false; do :; cd <wt>; done`: the loop body never runs.
#[test]
fn hole_loop_body_cd_judged_certain() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("while false; do :; cd {}; done; sed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// `true || { :; cd <wt>; }`: the group is skipped.
#[test]
fn hole_skipped_brace_group_cd_judged_certain() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("true || {{ :; cd {}; }}; sed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// `f() {\ncd <wt>\n}`: a function body that is never called.
#[test]
fn hole_uncalled_function_body_cd_judged_certain() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("f() {{\ncd {}\n}}\nsed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// `case x in (y) cd <wt>;; esac`: the pattern does not match; the `(`/`)` of
/// the pattern are read as a subshell and the `cd` after `)` as reached.
#[test]
fn hole_unmatched_case_arm_cd_judged_certain() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("case x in (y) cd {};; esac; sed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// A top-level comment holding `;` splits off a `cd` that never runs.
#[test]
fn hole_comment_semicolon_splits_off_cd() {
    let f = Fixture::new();
    assert_parent_judged(
        &f,
        &format!("# step one; cd {}\nsed -i s/a/b/ src/lib.rs", f.wt_s()),
    );
}

/// Unparsed fallback (`<<<` makes `statements` return None): `(cd wt` is not
/// an exact `cd` token, so the call is not undetermined and the relative `cd`
/// is not followed; the write in the wt is not attributed to it.
#[test]
fn hole_unparsed_fallback_misses_glued_cd() {
    let f = Fixture::new();
    assert_wt_judged_from_home(
        &f,
        "cat <<< x >/dev/null; (cd wt && sed -i s/a/b/ src/lib.rs)",
    );
}

/// Substitution: `$(cd wt && sed -i ...)` writes in the wt; the `$(cd` token
/// is not recognized (trim does not strip `$`) and the bare `wt` names nothing.
#[test]
fn hole_substitution_cd_not_followed() {
    let f = Fixture::new();
    assert_wt_judged_from_home(&f, "x=$(cd wt && sed -i s/a/b/ src/lib.rs)");
}

// ---- false positives ------------------------------------------------------

/// A trailing comment with `)` pops an empty subshell stack: walk errs and
/// SubagentStop blocks although the write is in the wt only.
#[test]
fn fp_comment_close_paren_blocks() {
    let f = Fixture::new();
    assert_parent_not_judged(
        &f,
        &format!("cd {} && sed -i s/a/b/ src/lib.rs # done :)", f.wt_s()),
    );
}

/// A `case` pattern without the optional `(`: unbalanced `)`, walk errs.
#[test]
#[ignore = "RED(39772e9f) false positive: case pattern ) pops an empty subshell stack, blocks"]
fn fp_case_pattern_paren_blocks() {
    let f = Fixture::new();
    assert_parent_not_judged(
        &f,
        &format!(
            "cd {}; case x in x) sed -i s/a/b/ src/lib.rs;; esac",
            f.wt_s()
        ),
    );
}
