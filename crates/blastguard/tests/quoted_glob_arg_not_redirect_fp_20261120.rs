#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False-positive REDs: a `>` INSIDE a single-quoted word is data, not a
//! redirect. Written by an independent test author (not the implementer;
//! CLAUDE.md §2(a)) for an FP measured on branch
//! `red/bg-glob-config-failopen-20261120`: main 0.2.816 = ask, branch
//! 4377a0816 = deny on allowed-corpus entry 1174 (a mutation-testing
//! script). The new Deny reason reads "'> head[k]]' destroys the file's
//! current contents and `head[k]]` is a glob that may expand to an existing
//! file" — but that `> head[k]]` sits inside the quoted function ARGUMENT
//! `'if len(v) > head[k]]'`, never in a redirect position.
//!
//! Property asserted (not a frozen verdict letter): the script with the
//! glob-looking quoted argument gets the SAME main/subagent verdict as the
//! same script whose quoted argument is a non-glob literal (`headk`), and
//! the main-mode verdict is not Deny. Both are measured in the same
//! fixture, so the comparison is the property.
//!
//! * REDs: `#[ignore = "RED ...: <observed> at <rev>"]`.
//! * Controls (not ignored): a real unquoted `echo a > head[k]]` stays
//!   non-Allow; the quoted target `echo a > 'head[k]]'` (a literal file
//!   name) stays Allow. A control is a characterization pin, not an
//!   endorsement.
//!
//! Harness copied from `glob_and_config_write_failopen_20261120.rs`; the
//! session cwd here is the project main tree `{P}` (the corpus entry's cwd
//! was the main tree). Placeholders: `{P}` project main tree, `{W}` a
//! linked worktree, `{H}` fixture `HOME`. Nothing is executed; the hook
//! only judges the text.
//!
//! DOES NOT PROVE: which layer produces the verdict; the Ask/Deny reason
//! text; quoting forms other than single quotes; globs other than the
//! bracket form `head[k]]`; that the non-glob baseline verdict itself is
//! the right one (only equality with it is asserted); behaviour with the
//! session cwd inside a worktree.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/fixed_tmp.rs"]
mod fixed_tmp;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    tmp: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.tmp);
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
            .join("dupgate")
    }
    fn tmpdir(&self) -> PathBuf {
        self.base.join("tmpdir")
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

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("qglob-{tag}-{n}"));
    let tmp = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-qglob-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&tmp);
    for d in [
        base.join("home").join(".claude").join("jobs"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base, tmp };
    let repo = fx.repo();
    write(&repo.join("t.txt"), "a\n");
    write(
        &repo.join("scripts").join("check-doc-duplication.py"),
        "x\n",
    );
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "dupgate",
        ],
    );
    fx
}

/// Run the hook once with the session cwd in the project main tree;
/// `(decision, reason)`. A non-zero exit panics; only then is an empty
/// stdout read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.repo();
    let mut p = serde_json::json!({
        "session_id": "quoted-glob-arg-fp-20261120",
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
        .env("TMPDIR", fx.tmpdir())
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", fx.repo())
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

fn expand(fx: &Fx, t: &str) -> String {
    t.replace("{H}", fx.home().to_str().unwrap())
        .replace("{W}", fx.wt().to_str().unwrap())
        .replace("{P}", fx.repo().to_str().unwrap())
}

/// `(main, sub, main reason, sub reason)` for the template.
fn measure(fx: &Fx, template: &str) -> (String, String, String, String) {
    let c = expand(fx, template);
    let (m, rm) = run_as(fx, &c, false);
    let (s, rs) = run_as(fx, &c, true);
    (m, s, rm, rs)
}

/// The glob-looking text that sits inside a quoted argument, and the
/// non-glob literal it is compared against.
const GLOB_ARG: &str = "head[k]]";
const PLAIN_ARG: &str = "headk";

/// Body: `template` contains `@ARG@`; the `GLOB_ARG` instance must get the
/// verdict of the `PLAIN_ARG` instance (both modes), and must not be Deny
/// in main mode (a `>` inside a quoted word is data).
fn expect_quoted_gt_is_data(tag: &str, template: &str) {
    let fx = fixture(tag);
    let glob = template.replace("@ARG@", GLOB_ARG);
    let plain = template.replace("@ARG@", PLAIN_ARG);
    let (gm, gs, grm, grs) = measure(&fx, &glob);
    let (pm, ps, prm, prs) = measure(&fx, &plain);
    assert_ne!(
        gm, "deny",
        "the `> {GLOB_ARG}` is inside a single-quoted argument, not a redirect, so it must \
         not be denied in main mode\n  glob main: {grm}\n  glob sub:  {grs}"
    );
    assert_eq!(
        (gm.as_str(), gs.as_str()),
        (pm.as_str(), ps.as_str()),
        "a quoted argument {GLOB_ARG:?} must get the verdict of the same script with the \
         non-glob literal {PLAIN_ARG:?} (main/sub)\n  glob main: {grm}\n  glob sub:  {grs}\n  \
         plain main: {prm}\n  plain sub:  {prs}"
    );
}

/// The exact allowed-corpus entry 1174 (0-based line of
/// `allowed-corpus.jsonl`), with sandboxed paths substituted:
/// `/Users/yuki/src/.harness-worktrees/dupgate` -> `{W}`,
/// `/Users/yuki/.claude/jobs/1a4ac77f/tmp/dupgate-mut` ->
/// `{H}/.claude/jobs/1a4ac77f/tmp/dupgate-mut`. `@ARG@` replaces the
/// `head[k]]` of the `ratchet` call only.
const CORPUS_1174: &str = r#"cd {W} && python3 -m pytest -q scripts/test_check_doc_duplication.py 2>&1 | tail -2
M={H}/.claude/jobs/1a4ac77f/tmp/dupgate-mut; mkdir -p $M
mut(){ name=$1; old=$2; new=$3; python3 - "$old" "$new" "$M/$name.py" <<'EOF'
import sys
src=open("scripts/check-doc-duplication.py").read()
assert src.count(sys.argv[1])==1, sys.argv[1]
open(sys.argv[3],"w").write(src.replace(sys.argv[1],sys.argv[2]))
EOF
r=$(DUPGATE_SCRIPT=$M/$name.py python3 -m pytest -q scripts/test_check_doc_duplication.py 2>&1 | tail -1); echo "$name: $r"; }
mut ratchet 'if len(v) > @ARG@' 'if len(v) > 0]'
mut readme 'if ma and mb and ma.group(1) == mb.group(1):' 'if False:'
mut instr 'if not (is_instruction(fa) or is_instruction(fb)):' 'if False:'
mut minc 'if len(n) < MIN_CHARS:' 'if len(n) < 1:'
mut utf8 'return data.decode("utf-8")' 'return data.decode("utf-8", "replace")'
mut thresh 'if jac >= THRESHOLD:' 'if jac >= 0.5:'
mut fence 'if stripped.startswith("```"):' 'if False:'"#;

#[test]
#[ignore = "RED quoted-glob-arg FP: corpus 1174 head[k]]=deny/deny vs headk=ask/deny at f61fb9a6b"]
fn red_corpus_1174_exact() {
    expect_quoted_gt_is_data("corpus1174", CORPUS_1174);
}

/// Minimal reproduction: a literal `M=` dir, a function whose body runs
/// `python3 -` with a quoted heredoc and uses `$M` and its argument, then a
/// call with a quoted argument carrying `> <glob>`. Measured at f61fb9a6b:
/// without the heredoc (plain `python3 - ...;`) or with `echo` in place of
/// `python3 -`, both spellings already agree, so those are not REDs here.
const MINIMAL: &str = "M={H}/m\nmut(){ name=$1; old=$2; python3 - \"$old\" \"$M/$name.py\" <<'EOF'\nimport sys\nEOF\n}\nmut ratchet 'a > @ARG@'";

#[test]
#[ignore = "RED quoted-glob-arg FP: minimal head[k]]=deny/deny vs headk=allow/allow at f61fb9a6b"]
fn red_minimal_function_quoted_arg() {
    expect_quoted_gt_is_data("minimal", MINIMAL);
}

// ---------------------------------------------------------------------------
// Controls (not ignored). Characterization pins, not endorsements.
// ---------------------------------------------------------------------------

/// A real unquoted redirect onto the glob target stays non-Allow.
#[test]
fn control_unquoted_redirect_onto_glob_non_allow() {
    let fx = fixture("ctl-unquoted");
    let (m, s, rm, rs) = measure(&fx, "echo a > head[k]]");
    let ok = |d: &str| d == "ask" || d == "deny";
    assert!(
        ok(&m) && ok(&s),
        "unquoted `echo a > head[k]]` is a real redirect onto a glob and must stay non-Allow; \
         got {m}/{s}\n  main: {rm}\n  sub: {rs}"
    );
}

/// A quoted redirect target names a literal file `head[k]]` and stays Allow.
#[test]
fn control_quoted_redirect_target_allow() {
    let fx = fixture("ctl-quoted");
    let (m, s, rm, rs) = measure(&fx, "echo a > 'head[k]]'");
    assert_eq!(
        (m.as_str(), s.as_str()),
        ("allow", "allow"),
        "quoted `echo a > 'head[k]]'` writes a literal file name and must stay Allow\n  \
         main: {rm}\n  sub: {rs}"
    );
}

/// The non-glob baseline of the minimal reproduction is not Deny in main
/// mode, asserted on its own so the RED's equality cannot be met by both
/// spellings drifting to Deny together.
#[test]
fn control_minimal_plain_arg_not_main_deny() {
    let fx = fixture("ctl-plain");
    let (m, _s, rm, _rs) = measure(&fx, &MINIMAL.replace("@ARG@", PLAIN_ARG));
    assert_ne!(
        m, "deny",
        "non-glob baseline moved to main Deny\n  main: {rm}"
    );
}

/// The measurement agent's `echo`-in-`$(...)` shape already satisfies the
/// property at f61fb9a6b (allow/allow for both spellings); pinned so it
/// does not regress.
#[test]
fn control_echo_subst_shape_already_data() {
    expect_quoted_gt_is_data(
        "ctl-echo",
        "M=/tmp/m\nf(){ name=$1; old=$2; r=$(X=$M/$name.py echo $old); }\nf ratchet 'a > @ARG@'",
    );
}
