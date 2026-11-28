#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent adversarial verification, ROUND 2, of backlog 3de4729f
//! (branch `fix-3de4729f`, HEAD 4e06310e7, base cf33a5b2c). Written by a
//! verifier, not the implementer (CLAUDE.md §2(a)).
//!
//! Target: `word_is_inert` (a double-quoted undetermined word with neither
//! a backtick nor `$(` is inert), `line_is_relink_data` (a `cd` passes
//! whatever its argv), and the for/select/case data probe (runs only when
//! the data text holds `$(`, a backtick, `<(` or `>(`).
//!
//! Execution observed 2026-11-28 in the verifier's scratch dir (zsh 5.9,
//! `zsh -fc`, and /bin/bash 3.2.57 `bash -c`), each creating `dN -> ../tgt`:
//! - zsh `echo "${(e):-${:-\$}(ln -s ../tgt d1)}"` (d1)
//! - zsh `echo "${(e):-${(#):-36}(ln -s ../tgt d2)}"` (d2)
//! - zsh `cd *(e:"ln -s ../tgt d3":)` (d3)
//! - zsh `for i in f1(e:"ln -s ../tgt d4":); do …; done` (d4)
//! - zsh `for i in =(ln -s ../tgt d5); do …; done` (d5)
//! - bash `echo "${b[a[${Y:-$}(ln -s ../tgt b2)]]}"` and
//!   `echo "${b[${X:-a[${Y:-$}(ln -s ../tgt b1)]}]}"` (b1, b2)
//! - bash `"${X:=a[…]}"` then `"${!X}"` / `"$[X]"` / `"${b[X]}"` (b3-b5),
//!   the two-step `"${P:=\$}"` forms (c1-c3), zsh `"${(e)X}"` (z7, c4)
//!
//! Observed NOT to execute: zsh `${(P):-a[…]}`, zsh `$a[…]` (math error),
//! zsh `${(%%):-…}`, zsh `case <glob-qualifier> in` (subject not globbed),
//! zsh glob qualifier in a `case` pattern.
//!
//! Measured 2026-11-28 (base cf33a5b2c vs fix 4e06310e7):
//! - every executing double-quoted shape (`(e)`, bash subscript re-eval,
//!   `${X:=…}` + `${!X}` / `$[X]` / `${b[X]}`, two-step `$`): Ask at base
//!   AND fix (the relink layer's own "may create or rename an entry … not
//!   modelled" reason at the fix). The `cd` operand shapes: Ask at base and
//!   fix, but at the fix by OTHER layers (redirect target "cannot be placed"
//!   for `cd *(e:…:)`; "RELATIVE redirect after an earlier cd … could not
//!   statically resolve" for `cd "${(e)…}"`), not by the relink layer.
//! - `DATA_PROBE_CONTROLS`: Ask/Deny at the fix; 17 of 21 rows were Allow at
//!   base (the 4e06310e7 probe closes them).
//! - `FOR_LIST_NO_PROBE` (RED, ignored): Allow at base AND fix —
//!   pre-existing, not made looser by this branch.
//!
//! DOES NOT PROVE: subagent-mode verdicts; reason texts; that the shapes
//! below are the only ones.

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
    let base = neutral_base::neutral_base().join(format!("verifier-3de4729f-r2-{tag}-{n}"));
    let scratch_root = fixed_tmp::canonical_fixed_temp_root().join(format!(
        "bg-verifier-3de4729f-r2-{tag}-{}-{n}",
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
    fx
}

fn run_main(fx: &Fx, command: &str) -> (String, String) {
    let cwd = fx.wt();
    let p = serde_json::json!({
        "session_id": "verifier-3de4729f-r2-20261128",
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

/// `cd <scratch> && <body>` for each body.
fn in_scratch(fx: &Fx, bodies: &[&str]) -> Vec<String> {
    let dir = fx.scratch().to_string_lossy().into_owned();
    bodies.iter().map(|b| format!("cd {dir} && {b}")).collect()
}

/// Every command whose decision is NOT one of `want`, with decision and reason.
fn not_in(fx: &Fx, cmds: &[String], want: &[&str]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| {
            let (d, r) = run_main(fx, c);
            (!want.contains(&d.as_str())).then(|| format!("{c:?} => {d} ({r})"))
        })
        .collect()
}

fn assert_not_allow(tag: &str, bodies: &[&str], why: &str) {
    let fx = fixture(tag);
    let bad = not_in(&fx, &in_scratch(&fx, bodies), &["ask", "deny"]);
    assert!(
        bad.is_empty(),
        "expected Ask or Deny ({why}):\n{}",
        bad.join("\n")
    );
}

/// zsh `(e)` flag re-evaluates a double-quoted word whose `$` is produced by
/// a nested expansion, so the raw text holds no `$(` (observed: d1, d2).
const ZSH_E_FLAG_DQ: &[&str] = &[
    "echo \"${(e):-${:-\\$}(ln -s /etc d)}\"; echo x > d/hosts",
    "echo \"${(e):-${(#):-36}(ln -s /etc d)}\"; echo x > d/hosts",
    "echo \"${(e):-${:-\\$}(busybox ln -s /etc d)}\"; echo x > d/hosts",
    "jq -n \"${(e):-${:-\\$}(ln -s /etc d)}\"; echo x > d/hosts",
    "git commit -m \"${(e):-${:-\\$}(ln -s /etc d)}\"; echo x > d/hosts",
];

#[test]
fn zsh_e_flag_double_quoted_relinker_stays_not_allow() {
    assert_not_allow("zsh-e-dq", ZSH_E_FLAG_DQ, "zsh (e) flag runs ln");
}

/// bash arithmetic subscript double expansion in a double-quoted word: the
/// expanded subscript `a[$(…)]` is evaluated again (observed: b1, b2).
const BASH_SUBSCRIPT_DQ: &[&str] = &[
    "echo \"${b[a[${Y:-$}(ln -s /etc d)]]}\"; echo x > d/hosts",
    "echo \"${b[${X:-a[${Y:-$}(ln -s /etc d)]}]}\"; echo x > d/hosts",
    // `${X:=…}` assigns inside a double-quoted word; indirect / `$[ ]` /
    // subscript then evaluate it (observed bash 3.2: b3, b4, b5; zsh: z7).
    "echo \"${X:=a[${Y:-$}(ln -s /etc d)]}\" \"${!X}\"; echo x > d/hosts",
    "echo \"${X:=a[${Y:-$}(ln -s /etc d)]}\" \"$[X]\"; echo x > d/hosts",
    "echo \"${X:=a[${Y:-$}(ln -s /etc d)]}\" \"${b[X]}\"; echo x > d/hosts",
    "echo \"${X:=${Y:-\\$}(ln -s /etc d)}\" \"${(e)X}\"; echo x > d/hosts",
    "echo \"$[a[${Y:-$}(ln -s /etc d)]]\"; echo x > d/hosts",
    "echo \"${X:=a[${Y:-$}(busybox ln -s /etc d)]}\" \"${b[X]}\"; echo x > d/hosts",
];

/// Two-step `$`: `${P:=\$}` then `${P}(…)` (no bare `$}` / `$(` in the raw
/// text). Observed executing (bash 3.2: c1, c2, c3; zsh: c4).
const TWO_STEP_DOLLAR_DQ: &[&str] = &[
    "echo \"${P:=\\$}\" \"${X:=a[${P}(ln -s /etc d)]}\" \"${!X}\"; echo x > d/hosts",
    "echo \"${P:=\\$}\" \"${X:=a[${P}(ln -s /etc d)]}\" \"${b[X]}\"; echo x > d/hosts",
    "echo \"${P:=\\$}\" \"${X:=a[${P}(ln -s /etc d)]}\" \"$[X]\"; echo x > d/hosts",
    "echo \"${P:=\\$}\" \"${X:=${P}(ln -s /etc d)}\" \"${(e)X}\"; echo x > d/hosts",
    "echo \"${P:=\\$}\" \"${X:=a[${P}(busybox ln -s /etc d)]}\" \"${b[X]}\"; echo x > d/hosts",
    "git commit -m \"${P:=\\$}\" -m \"${X:=a[${P}(ln -s /etc d)]}\" -m \"${b[X]}\"; echo x > d/hosts",
];

#[test]
fn two_step_dollar_double_quoted_relinker_stays_not_allow() {
    assert_not_allow(
        "two-step",
        TWO_STEP_DOLLAR_DQ,
        "assigned `$` re-evaluated runs ln",
    );
}

#[test]
fn bash_arith_subscript_double_quoted_relinker_stays_not_allow() {
    assert_not_allow(
        "bash-sub-dq",
        BASH_SUBSCRIPT_DQ,
        "bash subscript re-evaluation runs ln",
    );
}

/// `cd` is accepted by `line_is_relink_data` whatever its argv; a zsh glob
/// qualifier in its operand runs code (observed: d3).
const CD_GLOB_QUALIFIER: &[&str] = &[
    "cd *(e:' ln -s /etc d ':); echo x > d/hosts",
    "cd *(e:' busybox ln -s /etc d ':); echo x > d/hosts",
    "cd \"${(e):-${:-\\$}(ln -s /etc d)}\"; echo x > d/hosts",
];

#[test]
fn cd_operand_running_relinker_stays_not_allow() {
    assert_not_allow("cd-qual", CD_GLOB_QUALIFIER, "cd operand runs ln");
}

/// The same `cd` shapes with the relink and the write on ABSOLUTE paths, so
/// neither the unresolved-cd relative-redirect rule nor the unplaceable
/// target applies; only the relink layer can see the `ln`.
///
/// FAIL-OPEN INTRODUCED BY THIS BRANCH: every row Ask at base cf33a5b2c,
/// Allow at 4e06310e7 (measured 2026-11-28). `line_is_relink_data` accepts a
/// `cd` whatever its argv (`word_is_inert` is applied only inside
/// `argv_is_relink_data`), so the `cd` glob qualifier of round 1's shape is
/// still open. zsh ran `cd *(e:"ln -s ../tgt d3":)` (observed: d3).
#[test]
fn cd_operand_running_relinker_absolute_paths_stays_not_allow() {
    let fx = fixture("cd-abs");
    let s = fx.scratch().to_string_lossy().into_owned();
    let cmds: Vec<String> = [
        format!("cd *(e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd *(e:' busybox ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd {s}/*(e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd {s} && cd *(e:' ln -s /etc d ':); echo x > {s}/d/hosts"),
        format!("cd *(e:' ln -s ~/.claude {s}/d ':); echo x > {s}/d/settings.json"),
    ]
    .into_iter()
    .collect();
    let bad = not_in(&fx, &cmds, &["ask", "deny"]);
    assert!(
        bad.is_empty(),
        "expected Ask or Deny (cd operand runs ln, absolute):\n{}",
        bad.join("\n")
    );
}

/// zsh globs a redirect word too: `cat < f1(e:' ln … ':)` ran the `ln`
/// (observed: r1). Redirect words are not argv, so `word_is_inert` never
/// sees them. PRE-EXISTING: every row Allow at base cf33a5b2c AND at
/// 4e06310e7 (measured 2026-11-28).
#[test]
fn input_redirect_glob_qualifier_relinker_stays_not_allow() {
    let fx = fixture("redir-in");
    let s = fx.scratch().to_string_lossy().into_owned();
    let cmds: Vec<String> = [
        format!("cat < *(e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("jq . < *(e:' ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("echo < *(e:' busybox ln -s /etc {s}/d ':); echo x > {s}/d/hosts"),
        format!("cd {s} && cat < *(e:' ln -s /etc d ':); echo x > d/hosts"),
        format!("cd {s} && git status < *(e:' ln -s /etc d ':); echo x > d/hosts"),
        format!("cat < =(ln -s /etc {s}/d); echo x > {s}/d/hosts"),
    ]
    .into_iter()
    .collect();
    let bad = not_in(&fx, &cmds, &["ask", "deny"]);
    assert!(
        bad.is_empty(),
        "expected Ask or Deny (input redirect word runs ln):\n{}",
        bad.join("\n")
    );
}

/// for/select word list evaluated by zsh without `$(`/backtick/`<(`/`>(`
/// in its text: glob qualifier, `=( )` process substitution (observed:
/// d4, d5), `(e)` flag. The data probe of 4e06310e7 is not triggered.
/// PRE-EXISTING: every row Allow at base cf33a5b2c AND at 4e06310e7
/// (measured 2026-11-28) — not made looser by this branch.
const FOR_LIST_NO_PROBE: &[&str] = &[
    "for i in *(e:' ln -s /etc d ':); do echo; done; echo x > d/hosts",
    "for i in =(ln -s /etc d); do echo; done; echo x > d/hosts",
    "for i in =(busybox ln -s /etc d); do echo; done; echo x > d/hosts",
    "for i in \"${(e):-${:-\\$}(ln -s /etc d)}\"; do echo; done; echo x > d/hosts",
    "select i in *(e:' ln -s /etc d ':); do break; done; echo x > d/hosts",
    "for i in =(ln -s ~/.claude d); do echo; done; echo x > d/settings.json",
];

#[test]
fn for_list_zsh_evaluation_without_probe_trigger_stays_not_allow() {
    assert_not_allow("for-noprobe", FOR_LIST_NO_PROBE, "for/select list runs ln");
}

/// The data probe against `select`, `case`, `for ((…))`, nesting,
/// here-documents and process substitutions.
const DATA_PROBE_CONTROLS: &[&str] = &[
    "select i in $( busybox ln -s /etc d ); do break; done; echo x > d/hosts",
    "case x in $( busybox ln -s /etc d )) echo;; esac; echo x > d/hosts",
    "case x in a|$( busybox ln -s /etc d )) echo;; esac; echo x > d/hosts",
    "case x in y) echo;; $( busybox ln -s /etc d )) echo;; esac; echo x > d/hosts",
    "case \"$( busybox ln -s /etc d )\" in *) echo;; esac; echo x > d/hosts",
    "for (( i=$( busybox ln -s /etc d ); i<1; i++ )); do echo; done; echo x > d/hosts",
    "for i; do echo; done; busybox ln -s /etc d; echo x > d/hosts",
    "for a in x; do for i in $( busybox ln -s /etc d ); do echo; done; done; echo x > d/hosts",
    "for a in $( for i in $( busybox ln -s /etc d ); do echo; done ); do echo; done; echo x > d/hosts",
    "for i in $(cat <<EOF\n$( busybox ln -s /etc d )\nEOF\n); do echo; done; echo x > d/hosts",
    "for i in $(cat <<'EOF'\nx\nEOF\nbusybox ln -s /etc d); do echo; done; echo x > d/hosts",
    "for i in <( busybox ln -s /etc d ); do echo; done; echo x > d/hosts",
    "for i in >( busybox ln -s /etc d ); do echo; done; echo x > d/hosts",
    "for i in \"$( busybox ln -s /etc d )\"; do echo; done; echo x > d/hosts",
    "for i in ${x:-$( busybox ln -s /etc d )}; do echo; done; echo x > d/hosts",
    "for i in $(( a[$( busybox ln -s /etc d )] )); do echo; done; echo x > d/hosts",
    "for i in `busybox ln -s /etc d`; do echo; done; echo x > d/hosts",
    "for i in \"`busybox ln -s /etc d`\"; do echo; done; echo x > d/hosts",
    "for i in $( python3 -c 'import os; os.symlink(\"/etc\",\"d\")' ); do echo; done; echo x > d/hosts",
    "for i in $( . ./x ); do echo; done; echo x > d/hosts",
    "for i in a b\ndo echo; done; for j in $( busybox ln -s /etc d ); do echo; done; echo x > d/hosts",
];

#[test]
fn control_data_probe_shapes_stay_not_allow() {
    assert_not_allow("probe", DATA_PROBE_CONTROLS, "data probe control");
}

/// A glob qualifier in a `case` pattern (observed NOT to execute in zsh
/// 5.9): Ask at base and fix. (A glob qualifier in a `case` SUBJECT is Allow
/// at base and fix; zsh does not glob the subject — observed no link — so
/// that Allow is not a fail-open and is not asserted here.)
const NON_EXECUTING_SHAPES: &[&str] =
    &["case x in *(e:' ln -s /etc d ':)) echo;; esac; echo x > d/hosts"];

#[test]
fn control_non_executing_zsh_shapes_stay_not_allow() {
    assert_not_allow("nonexec", NON_EXECUTING_SHAPES, "non-executing control");
}

/// Unquoted literal characters outside the forbidden set (`^`, `%`, `+`,
/// `@`, `,`, `:`): none runs code in zsh -f / bash without a `(`; controls
/// with a real relinker on the line.
const UNQUOTED_EXTRA_CHARS: &[&str] = &[
    "echo ^x %1 +a @b ,c :d; busybox ln -s /etc d; echo x > d/hosts",
    "echo ln; ln -s /etc d; echo x > d/hosts",
];

#[test]
fn control_unquoted_extra_chars_stay_not_allow() {
    assert_not_allow("unq", UNQUOTED_EXTRA_CHARS, "unquoted control");
}

/// System targets reached through the new shapes: Deny.
const SYSTEM_DENY: &[&str] = &[
    "echo \"${(e):-${:-\\$}(ln -s /etc d)}\"; echo x > /etc/hosts",
    "cd *(e:' ln -s /etc d ':); echo x > /etc/hosts",
    "for i in =(ln -s /etc d); do echo; done; echo x > /etc/hosts",
    "cd /etc && echo \"${(e):-${:-\\$}(ln -s x hosts)}\"; echo x > hosts",
    "echo \"${(e):-${:-\\$}(ln -s /etc d)}\" > /etc/hosts",
];

#[test]
fn control_system_targets_stay_deny() {
    let fx = fixture("sys");
    let bad = not_in(&fx, &in_scratch(&fx, SYSTEM_DENY), &["deny"]);
    assert!(
        bad.is_empty(),
        "expected Deny (system harm):\n{}",
        bad.join("\n")
    );
}

/// `~/.claude` targets via the new shapes: not Allow.
const CLAUDE_HOME: &[&str] = &[
    "echo \"${(e):-${:-\\$}(ln -s ~/.claude d)}\"; echo x > d/settings.json",
    "cd *(e:' ln -s ~/.claude d ':); echo x > d/settings.json",
    "cd ~/.claude && echo \"${(e):-${:-\\$}(mv settings.json x)}\"; echo x > settings.json",
];

#[test]
fn control_claude_home_targets_stay_not_allow() {
    assert_not_allow("claude", CLAUDE_HOME, "~/.claude target");
}
