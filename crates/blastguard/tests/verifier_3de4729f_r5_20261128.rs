#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent adversarial verification, ROUND 5, of backlog 3de4729f
//! (branch `fix-3de4729f`, HEAD ca91d7e89 = blastguard 0.2.1049, base
//! cf33a5b2c). Written by a verifier, not the implementer (CLAUDE.md §2(a)).
//!
//! Target: the round-4 changes to `relink::word_evaluates` /
//! `zsh_runs_code`: `group_runs_code` (a non-leading `(…)` runs code only
//! when it holds `+` or an `e` followed by a non-alphanumeric), the
//! `${…}` skip via `skip_group`, the quote-aware `skip_group`, and
//! `prompt_expands_substitution` (`print -P`).
//!
//! Execution observed 2026-11-28 with zsh 5.9 (`zsh -fc`) in the verifier's
//! scratch dir:
//! - an ALPHANUMERIC `e` delimiter runs code: `*(eXtouch a1X)`,
//!   `*(e1touch a2 1)`, `*(oeQtouch b5Q)`, and `*(eQln -s tgt lnkQ)` created
//!   the symlink `lnk -> tgt`.
//! - the operand of `${…}` is globbed: `${:-*(e:"touch a3":)}`,
//!   `${^:-…}` (b7), `${=:-…}` (c1), `${~:-…}` (c8); a nested
//!   `${${(e):-\$(touch b1)}}` runs; `Y='*(e:touch b2x:)'; echo ${X:-$~Y}`
//!   runs (b2x). Quoted `"${:-*(e:…:)}"` does NOT run (b8).
//! - `$'\''` inside `$( )` / `${ }`: `$(: $'\'')*(e:"touch a4":)`,
//!   `${X:+$'\''}*(e:"touch a9":)`; a `'` inside a double-quoted `${ }`:
//!   `"${X:+'}"*(e:"touch a5":)` — all run.
//! - `setopt promptsubst` + `print -P $X` with `X='$(touch a6)'`, `print -P
//!   -- "$X"` (a8), `builtin print -P` (a7), `noglob print -P` (b6),
//!   `print -rP` (c9) — all run.
//! - `*('e':…)` (c2), `*(\e:…)` (c3), `*(e[…])` (c4), `*(e{…})` (c5),
//!   `*('+'fn)` (c7) run.
//! - `*(N)(eQtouch b4Q)` did NOT run ("no matches found": only the last
//!   group is a qualifier list, `(N)` is pattern text).
//!
//! Not observed: `*(e<…>)` — the probe was refused by blastguard in the
//! verifier's session ("'> ' destroys the file's current contents and ``
//! is a relative path …").
//!
//! Measured 2026-11-28, fix ca91d7e89 vs base cf33a5b2c (this file copied
//! into a worktree at cf33a5b2c and run there); per-test notes below.
//!
//! DOES NOT PROVE: subagent-mode verdicts; reason texts; that the shapes
//! below are the only ones; any zsh option set other than `-f` defaults
//! (plus the `setopt` written).

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
    scratch_root: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.scratch_root);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn scratch(&self) -> PathBuf {
        self.scratch_root.join("scratchpad")
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

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("verifier-3de4729f-r5-{tag}-{n}"));
    let scratch_root = fixed_tmp::canonical_fixed_temp_root().join(format!(
        "bg-verifier-3de4729f-r5-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&scratch_root);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
        scratch_root.join("scratchpad"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base, scratch_root };
    std::fs::write(fx.scratch().join("in.json"), "{}\n").unwrap();
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
    std::fs::write(wt.join("in.json"), "{}\n").unwrap();
    std::fs::write(wt.join("f"), "\n").unwrap();
    fx
}

fn run_main(fx: &Fx, command: &str) -> (String, String) {
    let cwd = fx.wt();
    let p = serde_json::json!({
        "session_id": "verifier-3de4729f-r5-20261128",
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

fn expect(tag: &str, cmds: &[&str], want: &[&str], why: &str) {
    let fx = fixture(tag);
    let mut bad = Vec::new();
    for c in cmds {
        let (d, r) = run_main(&fx, c);
        eprintln!("[{tag}] {c:?} => {d}");
        if !want.contains(&d.as_str()) {
            bad.push(format!("{c:?} => {d} ({r})"));
        }
    }
    assert!(
        bad.is_empty(),
        "expected {want:?} ({why}):\n{}",
        bad.join("\n")
    );
}

const NOT_ALLOW: &[&str] = &["ask", "deny"];

// --------------------------------------------------------- A. fail-open --

/// An ALPHANUMERIC delimiter after the `e` qualifier (zsh takes any
/// character; observed running: a1, a2, b5, `lnk -> tgt`).
/// `group_runs_code` needs a non-alphanumeric after `e`. PRE-EXISTING:
/// Allow at base cf33a5b2c AND ca91d7e89, but a bypass of this branch's
/// guard, which Asks `echo *(e:…:)` (control) at ca91d7e89.
const FAILOPEN_ALNUM_E_DELIMITER: &[&str] = &[
    "echo *(eQln -s /etc dQ); echo x > d/hosts",
    "echo *(e1ln -s /etc d1); echo x > d/hosts",
    "echo *(oeQln -s /etc dQ); echo x > d/hosts",
    "echo *(.eQln -s /etc dQ); echo x > d/hosts",
    "echo f*(NeXln -s /etc dX); echo x > d/hosts",
];

#[test]
fn failopen_alnum_e_delimiter_stays_not_allow() {
    expect(
        "fo-alnum",
        FAILOPEN_ALNUM_E_DELIMITER,
        NOT_ALLOW,
        "zsh runs ln from the qualifier",
    );
}

/// A glob qualifier inside an unquoted `${…}` operand, which zsh globs
/// (a3, b7, c1, c8), and a `$~` / `${~` nested inside `${…}` (b2x).
/// `skip_group` skips the whole `${…}`. PRE-EXISTING: Allow at base AND
/// ca91d7e89, but a bypass of this branch's guard (`${~:-…}` is Asked).
const FAILOPEN_PARAM_OPERAND: &[&str] = &[
    "echo ${:-*(e:\"ln -s /etc d\":)}; echo x > d/hosts",
    "echo ${X:-*(e:\"ln -s /etc d\":)}; echo x > d/hosts",
    "echo ${^:-*(e:\"ln -s /etc d\":)}; echo x > d/hosts",
    "echo ${=:-*(e:\"ln -s /etc d\":)}; echo x > d/hosts",
    "Y='*(e:ln -s /etc d:)'; echo ${X:-$~Y}; echo x > d/hosts",
    "Y='*(e:ln -s /etc d:)'; echo ${X:-${~Y}}; echo x > d/hosts",
];

#[test]
fn failopen_param_operand_stays_not_allow() {
    expect(
        "fo-param",
        FAILOPEN_PARAM_OPERAND,
        NOT_ALLOW,
        "zsh runs ln from the operand",
    );
}

/// `print -P` under `promptsubst` reached through a precommand
/// (`builtin`, `noglob`; a7, b6) or with its operand in a variable (a6).
/// PRE-EXISTING: Allow at base AND ca91d7e89.
const FAILOPEN_PRINT_P_VARIANTS: &[&str] = &[
    "setopt promptsubst; builtin print -P '$(ln -s /etc d)'; echo x > d/hosts",
    "setopt promptsubst; noglob print -P '$(ln -s /etc d)'; echo x > d/hosts",
    "X='$(ln -s /etc d)'; setopt promptsubst; print -P $X; echo x > d/hosts",
];

#[test]
fn failopen_print_p_variants_stay_not_allow() {
    expect(
        "fo-printp",
        FAILOPEN_PRINT_P_VARIANTS,
        NOT_ALLOW,
        "promptsubst runs ln",
    );
}

// ---------------------------------------------------------- controls --

/// Executing shapes the fix catches (c2, c3, c4, c5, c7, c8, c9, a4, a5,
/// a8, a9, b1 observed running; `*(N)(e…)` not running but read
/// conservatively). Not Allow at ca91d7e89 (measured 2026-11-28); at base
/// cf33a5b2c all but `cd *(eQ…)`, `${${(e)…}}`, `"${X:+'}"…`, `${(%)…}` and
/// `*(+fn)` were Allow — this branch made them stricter.
const CONTROL_NOT_ALLOW: &[&str] = &[
    "cd *(eQln -s /etc dQ); echo x > d/hosts",
    "echo ${${(e):-\\$(ln -s /etc d)}}; echo x > d/hosts",
    "echo $(: $'\\'')*(e:\"ln -s /etc d\":); echo x > d/hosts",
    "echo ${X:+$'\\''}*(e:\"ln -s /etc d\":); echo x > d/hosts",
    "echo \"${X:+'}\"*(e:\"ln -s /etc d\":); echo x > d/hosts",
    "X='$(ln -s /etc d)'; setopt promptsubst; print -P -- \"$X\"; echo x > d/hosts",
    "echo *('e':\"ln -s /etc d\":); echo x > d/hosts",
    "echo *(\\e:\"ln -s /etc d\":); echo x > d/hosts",
    "echo *(\"e\":\"ln -s /etc d\":); echo x > d/hosts",
    "echo *(e[ln -s /etc d]); echo x > d/hosts",
    "echo *(e{ln -s /etc d}); echo x > d/hosts",
    "echo *(N)(e:\"ln -s /etc d\":); echo x > d/hosts",
    "echo *(#qe:\"ln -s /etc d\":); echo x > d/hosts",
    "fn() { ln -s /etc d; }; echo *('+'fn); echo x > d/hosts",
    "fn() { ln -s /etc d; }; echo *(+fn); echo x > d/hosts",
    "echo ${~:-*(e:\"ln -s /etc d\":)}; echo x > d/hosts",
    "echo ${(%):-'$(ln -s /etc d)'}; echo x > d/hosts",
    "setopt promptsubst; print -P '$(ln -s /etc d)'; echo x > d/hosts",
    "setopt promptsubst; print -rP '$(ln -s /etc d)'; echo x > d/hosts",
    "setopt promptsubst; print -r -P '$(ln -s /etc d)'; echo x > d/hosts",
];

#[test]
fn control_executing_shapes_stay_not_allow() {
    expect("ctl-not", CONTROL_NOT_ALLOW, NOT_ALLOW, "zsh runs ln");
}

/// Everyday shapes that run no code: Allow at the fix.
const CONTROL_ALLOW: &[&str] = &[
    "ls *(e) > o",
    "grep 'x(ex' f > o",
    "echo $(printf 'e:') > o",
    "echo $(date +%s) > o",
    "ls *(.om[1,3]) > o",
    "ls -d *(/) > o",
    "ls *(.N) > o",
    "ls *(mh-1) > o",
    "ls *.(rs|toml) > o",
    "echo ${PWD:-(x)} > o",
    "echo \"${:-*(e:x:)}\" > o",
    "print -P '%F{red}hi%f' > o",
    "find . -name x -exec ls {} + > o",
    "git diff HEAD~1 > o",
];

#[test]
fn control_everyday_shapes_stay_allow() {
    expect("ctl-allow", CONTROL_ALLOW, &["allow"], "no code runs");
}

// --------------------------------------------------------------- FP --

/// Everyday zsh globs / bash extglob that run no code, but whose group
/// holds an `e` before `|` / `_` or a `+` (size / time qualifier).
/// Allow at base cf33a5b2c, Ask at ca91d7e89 (introduced by this branch).
const FP_NON_EXECUTING_GROUP: &[&str] = &[
    "ls *.(vue|ts) > o",
    "ls !(target|node_modules) > o",
    "ls *(Lk+100) > o",
    "ls *(.mh+2) > o",
];

#[test]
fn fp_non_executing_group_is_allow() {
    expect(
        "fp-group",
        FP_NON_EXECUTING_GROUP,
        &["allow"],
        "no code runs",
    );
}
