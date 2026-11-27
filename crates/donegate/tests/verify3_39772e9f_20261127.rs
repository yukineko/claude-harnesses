// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial probes, round 3 (2026-11-27), of 2882004f5 (39772e9f round 3:
//! cwd followed only for a FLAT list; any other command holding a
//! `cd`/`pushd`/`popd` token is fenced) and 8a2b68ecb (ff46a113: a `cargo fmt`
//! writer widens only a root related to its effective cwd).
//!
//! Two harnesses:
//!  - SubagentStop (`donegate subagent-gate`, hook cwd = the judged root),
//!    which judges the root iff `own_written_paths` places a write inside it;
//!  - Stop (`donegate gate`, hook cwd = `main`), whose `when_changed` scope is
//!    narrowed to the session's named writes unless
//!    `has_unattributable_write_in(transcript, main)` holds.
//!
//! Classes: `hole_*` = fail-open (bash lands a write in the root, its checks
//! do not run); `fp_*` = an ordinary read-only call makes the gate block;
//! `control_*` = cases the branch gets right.
//!
//! Transcripts are synthetic: no recorded command runs inside the gate. Where
//! a probe relies on shell / cargo behaviour, that behaviour was observed
//! separately on this host (2026-11-27): logical `cd lnk/..` in bash and zsh
//! returns to the link's parent; `cargo fmt --all` rewrote a path dependency
//! outside the cwd's workspace; `cargo fmt -p b` run in member `w/a` rewrote
//! member `w/sub/b`. Not proven: how often Claude Code records such commands.
//!
//! Written by a disinterested verifier (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const AGENT_ID: &str = "a20261127verify3x";
const SESSION: &str = "s-verify3-20261127";

fn git_st(dir: &Path, args: &[&str]) -> bool {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs")
        .success()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        git_st(dir, args),
        "git {args:?} failed in {}",
        dir.display()
    );
}

fn jline(v: serde_json::Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

fn user_line() -> String {
    jline(serde_json::json!({
        "type": "user", "message": {"role": "user", "content": "go"}
    }))
}

/// One Bash tool_use line; `cwd: None` omits the field.
fn bash_use(id: &str, command: &str, cwd: Option<&Path>, sidechain: bool) -> String {
    let mut v = serde_json::json!({
        "parentUuid": "p", "isSidechain": sidechain,
        "message": {"model": "claude-opus-5", "id": format!("m-{id}"), "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": "Bash",
                "input": {"command": command}}]}
    });
    if let Some(c) = cwd {
        v["cwd"] = serde_json::json!(c.to_string_lossy());
    }
    jline(v)
}

struct Outcome {
    code: Option<i32>,
    decision: String,
    stdout: String,
    stderr: String,
}

fn dump(o: &Outcome) -> String {
    format!(
        "code={:?} stdout={:?} stderr={:?}",
        o.code, o.stdout, o.stderr
    )
}

fn run_hook(sub: &str, home: &Path, cwd: &Path, payload: serde_json::Value) -> Outcome {
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
        .arg(sub)
        .current_dir(cwd)
        .env("HOME", home)
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

fn fresh_home(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let home = std::env::temp_dir().join(format!("dg-v3-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home.canonicalize().unwrap()
}

fn trust(home: &Path, roots: &[&Path]) {
    std::fs::create_dir_all(home.join(".harness")).unwrap();
    let list: Vec<String> = roots
        .iter()
        .map(|r| format!("\"{}\"", r.display()))
        .collect();
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [{}]\n", list.join(", ")),
    )
    .unwrap();
}

// ---- SubagentStop harness (mirrors verify2_39772e9f_cd_holes.rs) ----------

struct Sub {
    home: PathBuf,
    parent: PathBuf,
    wt: PathBuf,
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
    git(&root, &["commit", "-qm", "init"]);
    root
}

impl Sub {
    fn new() -> Sub {
        let home = fresh_home("sub");
        let parent = make_repo(&home, "parent", "ran-parent");
        let wt = make_repo(&home, "wt", "ran-wt");
        trust(&home, &[&parent, &wt]);
        Sub { home, parent, wt }
    }

    fn ran(&self, marker: &str) -> bool {
        self.home.join(marker).exists()
    }

    fn stop(&self, line_cwd: &Path, hook_cwd: &Path, cmd: &str) -> Outcome {
        let d = self.home.join(".claude/projects/-slug");
        let side = d.join("sess").join("subagents");
        std::fs::create_dir_all(&side).unwrap();
        let parent_t = d.join("sess.jsonl");
        std::fs::write(&parent_t, user_line()).unwrap();
        let agent_t = side.join(format!("agent-{AGENT_ID}.jsonl"));
        std::fs::write(
            &agent_t,
            format!(
                "{}{}",
                user_line(),
                bash_use("t-sub", cmd, Some(line_cwd), true)
            ),
        )
        .unwrap();
        run_hook(
            "subagent-gate",
            &self.home,
            hook_cwd,
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": parent_t.to_string_lossy(),
                "agent_transcript_path": agent_t.to_string_lossy(),
                "agent_id": AGENT_ID,
                "cwd": hook_cwd.to_string_lossy(),
                "hook_event_name": "SubagentStop",
                "stop_hook_active": false,
            }),
        )
    }
}

impl Drop for Sub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Line cwd = home, hook cwd = wt; bash lands the write in wt.
fn assert_wt_judged_from_home(f: &Sub, cmd: &str) {
    let o = f.stop(&f.home.clone(), &f.wt.clone(), cmd);
    assert!(
        f.ran("ran-wt"),
        "bash lands {cmd:?}'s write in {}, yet its checks did not run: {}",
        f.wt.display(),
        dump(&o)
    );
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// Line cwd = hook cwd = parent; bash lands the write in parent.
fn assert_parent_judged(f: &Sub, cmd: &str) {
    let o = f.stop(&f.parent.clone(), &f.parent.clone(), cmd);
    assert!(
        f.ran("ran-parent"),
        "bash lands {cmd:?}'s write in the parent root (hook cwd), yet its checks did not \
         run: {}",
        dump(&o)
    );
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// Line cwd = home, hook cwd = wt; the command is undetermined: block.
fn assert_wt_blocks_from_home(f: &Sub, cmd: &str) {
    let o = f.stop(&f.home.clone(), &f.wt.clone(), cmd);
    assert_eq!(o.decision, "block", "{cmd:?}: {}", dump(&o));
}

// ---- Stop harness (mirrors backlog_20261121_foreign_merge_...rs) ----------

const RS: &str = "crates/blastguard/src/lib.rs";
const MARKER: &str = "check-ran.marker";

struct Top {
    home: PathBuf,
    main: PathBuf,
    wt: PathBuf,
}

impl Top {
    /// `main` (judged root, `when_changed = **/*.rs` red check) with a
    /// linked worktree `wt`, and an UNCOMMITTED change of `RS` in `main`
    /// that no call of the transcript names (what a path-less writer such
    /// as `cargo fmt` leaves behind).
    fn new() -> Top {
        let home = fresh_home("top");
        let main = home.join("main");
        std::fs::create_dir_all(main.join("crates/blastguard/src")).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@t.com"]);
        git(&main, &["config", "user.name", "t"]);
        std::fs::write(main.join(RS), "pub fn f() {}\n").unwrap();
        std::fs::write(
            main.join("crates/blastguard/Cargo.toml"),
            "[package]\nname = \"blastguard\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            main.join("donegate.toml"),
            format!(
                "max_attempts = 50\n\n[[check]]\nname = \"test-changed\"\n\
                 cmd = \"echo ran > {}; exit 1\"\nwhen_changed = [\"**/*.rs\"]\n",
                home.join(MARKER).display()
            ),
        )
        .unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-qm", "init"]);
        let wt = home.join("wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session",
                &wt.to_string_lossy(),
            ],
        );
        let main = main.canonicalize().unwrap();
        let wt = wt.canonicalize().unwrap();
        trust(&home, &[&main, &wt]);
        std::fs::write(main.join(RS), "pub fn f() {\n}\n").unwrap();
        Top { home, main, wt }
    }

    fn marker(&self) -> bool {
        self.home.join(MARKER).exists()
    }

    /// Stop with cwd = main; one parent transcript line per `(line cwd, cmd)`.
    fn stop(&self, calls: &[(Option<&Path>, &str)]) -> Outcome {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = user_line();
        for (i, (cwd, cmd)) in calls.iter().enumerate() {
            body.push_str(&bash_use(&format!("t{i}"), cmd, *cwd, false));
        }
        std::fs::write(&p, body).unwrap();
        run_hook(
            "gate",
            &self.home,
            &self.main,
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": p.to_string_lossy(),
                "cwd": self.main.to_string_lossy(),
                "hook_event_name": "Stop",
                "stop_hook_active": false,
            }),
        )
    }
}

impl Drop for Top {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn assert_main_judged(f: &Top, calls: &[(Option<&Path>, &str)]) {
    let o = f.stop(calls);
    assert!(
        f.marker(),
        "the session's writer can have produced {RS} in main, yet main's when_changed check \
         did not run (scope narrowed away): calls={calls:?} {}",
        dump(&o)
    );
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

fn assert_main_not_judged(f: &Top, calls: &[(Option<&Path>, &str)]) {
    let o = f.stop(calls);
    assert!(
        !f.marker() && o.decision != "block",
        "the session only READ (or wrote outside main), yet main's check ran on dirt it did \
         not make: calls={calls:?} marker={} {}",
        f.marker(),
        dump(&o)
    );
}

// ===== 39772e9f: SubagentStop =============================================

/// CONTROL: `"cd" wt` (quoted command word) is still a directory command.
#[test]
fn control_quoted_cd_word_is_followed() {
    let f = Sub::new();
    assert_wt_judged_from_home(&f, "\"cd\" wt && sed -i s/a/b/ src/lib.rs");
}

/// CONTROL: `builtin cd`, `command cd`, `eval cd`: undetermined, block.
#[test]
fn control_wrapped_cd_blocks() {
    for w in [
        "builtin cd wt",
        "command cd wt",
        "eval cd wt",
        "eval 'cd wt'",
    ] {
        let f = Sub::new();
        assert_wt_blocks_from_home(&f, &format!("{w} && sed -i s/a/b/ src/lib.rs"));
    }
}

/// CONTROL: `cd $D`, `cd ~/wt`, bare `cd`: undetermined, block.
#[test]
fn control_nonliteral_cd_blocks() {
    for c in ["D=wt; cd $D", "cd ~/wt", "cd"] {
        let f = Sub::new();
        assert_wt_blocks_from_home(&f, &format!("{c} && sed -i s/a/b/ src/lib.rs"));
    }
}

/// CONTROL: a subshell `cd` is fenced and the candidate dir is judged.
#[test]
fn control_subshell_cd_is_fenced() {
    let f = Sub::new();
    assert_wt_judged_from_home(&f, "(cd wt && sed -i s/a/b/ src/lib.rs)");
}

/// HOLE: `\cd wt` (the usual alias-bypass spelling) runs the cd builtin, but
/// `has_dir_token` does not split on `\`, so `has_cd` is false and the
/// recorded cwd (home) is used: `src/lib.rs` resolves to the nonexistent
/// `home/src/lib.rs`, nothing is placed in wt, and wt is not judged.
#[test]
fn hole_backslash_cd_not_seen() {
    let f = Sub::new();
    assert_wt_judged_from_home(&f, "\\cd wt && sed -i s/a/b/ src/lib.rs");
}

/// HOLE: same with `c''d wt` (quotes glued inside the word).
#[test]
fn hole_quote_glued_cd_not_seen() {
    let f = Sub::new();
    assert_wt_judged_from_home(&f, "c''d wt && sed -i s/a/b/ src/lib.rs");
}

/// HOLE (regression): `bash -c '<writer>'` — the old split_whitespace scan
/// found `src/lib.rs` in the statement text; the parsed-word scan sees one
/// word `sed -i s/a/b/ src/lib.rs` and names nothing.
#[test]
fn hole_bash_c_inner_path_not_named() {
    let f = Sub::new();
    assert_parent_judged(&f, "bash -c 'sed -i s/a/b/ src/lib.rs'");
}

/// HOLE: logical `cd lnk/..` (lnk -> wt/src) returns to parent in bash and
/// zsh (observed), but the walk joins `parent/lnk/..` and canonicalizes it
/// PHYSICALLY to wt, so the write in parent/src/lib.rs is placed in wt.
#[test]
fn hole_symlink_dotdot_resolved_physically() {
    let f = Sub::new();
    std::os::unix::fs::symlink(f.wt.join("src"), f.parent.join("lnk")).unwrap();
    assert_parent_judged(&f, "cd lnk/.. && sed -i s/a/b/ src/lib.rs");
}

/// HOLE (pre-existing, not introduced here): `git -C wt checkout -- f`
/// writes wt/f; `wt` has no `/` or `.`, so it is not a path word.
#[test]
#[ignore = "RED(8533e79d): pre-existing, git -C <bare relative dir> writer not placed in that dir (verify3)"]
fn hole_git_dash_c_relative_dir() {
    let f = Sub::new();
    assert_wt_judged_from_home(&f, "git -C wt checkout -- src/lib.rs");
}

/// FP (regression): a READ-ONLY pipeline after `cd` is not flat, so it is
/// fenced and returned as writing with every token joined with every
/// candidate dir: `parent/src/lib.rs` counts as a write and the parent root
/// is judged although nothing was written.
#[test]
fn fp_fenced_readonly_pipeline_judged() {
    let f = Sub::new();
    let cmd = "cd ../wt && grep -n a src/lib.rs | head -3";
    let o = f.stop(&f.parent.clone(), &f.parent.clone(), cmd);
    assert!(
        !f.ran("ran-parent") && o.decision != "block",
        "{cmd:?} only reads, yet SubagentStop judged/blocked the parent root: {}",
        dump(&o)
    );
}

// ===== ff46a113: Stop scope narrowing =====================================

/// CONTROL: `cd <main> && cargo fmt` from a wt line: related to main, judged.
#[test]
fn control_cd_main_cargo_fmt_judged() {
    let f = Top::new();
    let cmd = format!("cd {} && cargo fmt", f.main.display());
    assert_main_judged(&f, &[(Some(&f.wt), &cmd)]);
}

/// CONTROL: forms outside the closed list stay global (judged): flags,
/// toolchain, env prefix, rustfmt passthrough, redirect.
#[test]
fn control_unlisted_cargo_fmt_forms_are_global() {
    for cmd in [
        "cargo fmt --manifest-path ../main/Cargo.toml",
        "cargo +nightly fmt",
        "RUSTFMT=/tmp/x cargo fmt",
        "cargo fmt -- --config-path ../main",
        "cargo fmt > /tmp/log",
        "cargo fmt -p",
    ] {
        let f = Top::new();
        assert_main_judged(&f, &[(Some(&f.wt.clone()), cmd)]);
    }
}

/// CONTROL: python3 - stays global wherever it runs.
#[test]
fn control_python_stdin_is_global() {
    let f = Top::new();
    assert_main_judged(&f, &[(Some(&f.wt.clone()), "python3 - < /dev/null")]);
}

/// HOLE: `cargo fmt --all` rewrites "local path-based dependencies" too
/// (cargo fmt --help; observed: it rewrote ../b). Run in home/x whose crate
/// depends on main's crate by path, it writes main, yet Within(home/x) is
/// not related to main and main's scope is narrowed.
#[test]
fn hole_cargo_fmt_all_path_dep() {
    let f = Top::new();
    let x = f.home.join("x");
    std::fs::create_dir_all(x.join("src")).unwrap();
    std::fs::write(
        x.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         [dependencies]\nblastguard = { path = \"../main/crates/blastguard\" }\n",
    )
    .unwrap();
    std::fs::write(x.join("src/lib.rs"), "").unwrap();
    assert_main_judged(&f, &[(Some(&x), "cargo fmt --all")]);
}

/// HOLE: `cargo fmt -p NAME` formats any MEMBER of the cwd's workspace, not
/// only what is under the cwd (observed: from w/a it rewrote w/sub/b). Here
/// the workspace at home has members x and main/crates/blastguard; run in
/// home/x it writes main, but Within(home/x) is unrelated to main.
#[test]
fn hole_cargo_fmt_p_sibling_member() {
    let f = Top::new();
    let x = f.home.join("x");
    std::fs::create_dir_all(x.join("src")).unwrap();
    std::fs::write(
        f.home.join("Cargo.toml"),
        "[workspace]\nmembers = [\"x\", \"main/crates/blastguard\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    std::fs::write(
        x.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(x.join("src/lib.rs"), "").unwrap();
    assert_main_judged(&f, &[(Some(&x), "cargo fmt -p blastguard")]);
}

/// HOLE: unknown line cwd + relative `cd main && cargo fmt`: the walk keeps
/// the relative `main` as the base, so the call is Within(["main"]), which
/// `related` resolves against the HOOK process cwd (main/main: absent) and
/// finds unrelated. The docstring says an undeterminable cwd is global.
#[test]
fn hole_unknown_cwd_relative_cd_cargo_fmt() {
    let f = Top::new();
    assert_main_judged(&f, &[(None, "cd main && cargo fmt")]);
}

/// HOLE: `\cd <main> && cargo fmt` from a wt line: `has_cd` is false (see
/// hole_backslash_cd_not_seen), so the effective cwd is the wt line cwd.
#[test]
fn hole_backslash_cd_main_cargo_fmt() {
    let f = Top::new();
    let cmd = format!("\\cd {} && cargo fmt", f.main.display());
    assert_main_judged(&f, &[(Some(&f.wt), &cmd)]);
}

/// FP (regression vs the old per-stage rule): a READ-ONLY pipeline after a
/// `cd` (`cd <wt> && git log --oneline | head -3`) is fenced, and
/// `call_unattributable` returns Global for any fenced call before looking
/// at its stages, so main's scope widens to dirt the session never touched.
#[test]
fn fp_fenced_readonly_pipeline_is_global() {
    let f = Top::new();
    let cmd = format!("cd {} && git log --oneline | head -3", f.wt.display());
    assert_main_not_judged(&f, &[(Some(&f.main.clone()), &cmd)]);
}

/// CONTROL: the same read-only call as a flat list is not unattributable.
#[test]
fn control_flat_readonly_cd_not_global() {
    let f = Top::new();
    let cmd = format!("cd {} && git log --oneline -3", f.wt.display());
    assert_main_not_judged(&f, &[(Some(&f.main.clone()), &cmd)]);
}
