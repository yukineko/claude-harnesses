#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-open REDs for the dot-name rule of `globs_may_intersect`
//! (`crates/blastguard/src/detect/ast/judge/deny_targets.rs`), measured by
//! an independent test author (not the implementer; CLAUDE.md §2(a)) at
//! 4377a0816 (blastguard 0.2.821). Harness and placeholders copied from
//! `glob_and_config_write_failopen_20261120.rs` (session cwd = own worktree
//! `{W}`).
//!
//! The rule: a shell glob component that does not start with `.` never
//! matches a dot-name (bash without `dotglob`, zsh without `GLOB_DOTS`). Its
//! stated limit: a command line that turns `dotglob` / `GLOB_DOTS` on is not
//! seen. Under `shopt -s dotglob` / `setopt globdots` / `setopt GLOB_DOTS` /
//! `setopt glob_dots`, `{W}/*`, `{W}/*gitconfig`, `{W}/?gitconfig` DO
//! expand to `{W}/.gitconfig`, a deny-target (`**/.gitconfig`). Property
//! asserted by each RED: the glob's verdict, per mode, is not looser
//! (Allow < Ask < Deny) than the verdict of the literal dot-name spelling
//! it can expand to, measured in the same fixture; the literal's verdict is
//! also asserted to be `deny/deny` (the 4377a0816 measurement) so that both
//! drifting to Allow together cannot pass.
//!
//! * REDs: `#[ignore = "RED ...: <observed> at 4377a0816"]`.
//! * Controls (not ignored, characterization pins, not endorsements): the
//!   same globs WITHOUT a dotglob switch keep their 4377a0816 verdict
//!   (`allow/allow`); the zsh `(D)` qualifier and per-invocation switches
//!   (`bash -O dotglob -c`, `zsh -o globdots -c`), already non-Allow, keep
//!   `ask/deny`; the literal dot-name spellings keep `deny/deny`.
//!
//! Placeholders: `{T}` dir strictly inside the fixed temp root, `{D}` the
//! hook's `TMPDIR`, `{H}` fixture `HOME`, `{W}` own worktree (session cwd),
//! `{O}` other worktree, `{P}` project main tree. Nothing is executed; the
//! hook only judges the text.
//!
//! Excluded on purpose: `dd of={W}/*` (`of=…*` is one word whose glob would
//! have to match a name starting with `of=`, so it stays literal), `cp u.txt
//! {W}/*` and bash `> {W}/*` (several matches change the meaning: last
//! operand as directory / "ambiguous redirect"); single-match `*gitconfig`
//! spellings are used for those writers instead.
//!
//! DOES NOT PROVE: that dotglob set OUTSIDE the command line (a user's rc
//! file, an earlier Bash call in the same shell) is handled — the no-dotglob
//! controls pin Allow, which is only right when the shell starts without
//! it; other dot-name deny-targets (`.ssh`, `.githooks`, `.claude`) reached
//! by a final-component glob; `{H}`/`{P}` placements (already deny/deny at
//! 4377a0816 with or without dotglob); which layer produces a verdict; the
//! Ask/Deny reason text; Write/Edit tools.

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
    fn wt(&self, name: &str) -> PathBuf {
        self.base.join("src").join(".harness-worktrees").join(name)
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
    let base = neutral_base::neutral_base().join(format!("dotglob-{tag}-{n}"));
    let tmp = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-dotglob-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&tmp);
    for d in [
        base.join("home").join(".cache"),
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
    write(&repo.join(".claude").join("settings.json"), "{}\n");
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    for (name, br) in [("wt-own", "own"), ("wt-other", "other")] {
        let wt = fx.wt(name);
        fx.git(
            &repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", br],
        );
        write(&wt.join("u.txt"), "a\n");
        write(&wt.join("sub").join("f.txt"), "a\n");
    }
    write(&repo.join("u.txt"), "a\n");
    write(&fx.tmp.join("sub").join("f.txt"), "a\n");
    let own = fx.wt("wt-own");
    write(&own.join(".claude").join("settings.local.json"), "{}\n");
    for tree in [own.clone(), fx.repo(), fx.wt("wt-other")] {
        write(&tree.join("hooks").join("hooks.json"), "{}\n");
        write(&tree.join(".githooks").join("pre-commit"), "#!/bin/sh\n");
        write(&tree.join("scripts").join("check-x.py"), "pass\n");
    }
    // Protected dot-names a bare `*` reaches only under dotglob.
    for dir in [own.clone(), fx.tmp.clone(), fx.tmpdir(), fx.home()] {
        write(&dir.join(".ssh").join("id_rsa"), "k\n");
        write(&dir.join(".gitconfig"), "[x]\n");
    }
    write(&fx.tmpdir().join("sub").join("f.txt"), "a\n");
    write(&fx.home().join(".cache").join("x").join("f.txt"), "a\n");
    write(&fx.home().join(".zshrc"), "x\n");
    write(&fx.home().join("docs").join("f.txt"), "a\n");
    fx
}

/// Run the hook once with the session cwd in `wt-own`; `(decision, reason)`.
/// A non-zero exit panics; only then is an empty stdout read as Allow;
/// non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt("wt-own");
    let mut p = serde_json::json!({
        "session_id": "dotglob-failopen-20261120",
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
    t.replace("{T}", fx.tmp.to_str().unwrap())
        .replace("{D}", fx.tmpdir().to_str().unwrap())
        .replace("{H}", fx.home().to_str().unwrap())
        .replace("{W}", fx.wt("wt-own").to_str().unwrap())
        .replace("{O}", fx.wt("wt-other").to_str().unwrap())
        .replace("{P}", fx.repo().to_str().unwrap())
}

/// `"main/subagent"` for the template, plus both reasons.
fn measure(fx: &Fx, template: &str) -> (String, String, String) {
    let c = expand(fx, template);
    let (m, rm) = run_as(fx, &c, false);
    let (s, rs) = run_as(fx, &c, true);
    (format!("{m}/{s}"), rm, rs)
}

const A: &str = "allow/allow";
const D: &str = "deny/deny";
const AD: &str = "ask/deny";

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unexpected decision {other:?}"),
    }
}

/// RED body: under a dotglob switch, `glob` can expand to the dot-name that
/// `literal` spells; per mode the glob must not be looser than the literal,
/// and the literal must still be `deny/deny` (floor).
fn expect_not_looser(tag: &str, glob: &str, literal: &str) {
    let fx = fixture(tag);
    let c = expand(&fx, glob);
    let l = expand(&fx, literal);
    let (gm, grm) = run_as(&fx, &c, false);
    let (gs, grs) = run_as(&fx, &c, true);
    let (lm, lrm) = run_as(&fx, &l, false);
    let (ls, lrs) = run_as(&fx, &l, true);
    assert_eq!(
        format!("{lm}/{ls}"),
        D,
        "{literal:?} (the literal dot-name spelling) moved off {D}\n  main: {lrm}\n  sub: {lrs}"
    );
    assert!(
        rank(&gm) >= rank(&lm) && rank(&gs) >= rank(&ls),
        "{glob:?} turns dotglob/GLOB_DOTS on, so its glob can expand to the dot-name \
         {literal:?} spells; its verdict {gm}/{gs} is looser than the literal's {lm}/{ls}\n  \
         glob main: {grm}\n  glob sub:  {grs}"
    );
}

/// Control body: the template keeps its 4377a0816 verdict.
fn expect_pinned(tag: &str, template: &str, want: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    assert_eq!(
        got, want,
        "{template:?} moved off its 4377a0816 verdict {want}\n  main: {rm}\n  sub: {rs}"
    );
}

macro_rules! reds {
    ($($name:ident: $g:literal => $l:literal;)*) => {
        $(
            #[test]
            #[ignore = "RED dotglob glob reaches a dot-name deny-target: allow/allow at 4377a0816"]
            fn $name() {
                expect_not_looser(stringify!($name), $g, $l);
            }
        )*
    };
}

macro_rules! pins {
    ($($name:ident: $t:literal => $want:expr;)*) => {
        $(
            #[test]
            fn $name() {
                expect_pinned(stringify!($name), $t, $want);
            }
        )*
    };
}

// ---------------------------------------------------------------------------
// REDs — every glob below was observed allow/allow at 4377a0816; every
// literal was observed deny/deny.
// ---------------------------------------------------------------------------

reds! {
    // own worktree {W}
    red_own_shopt_redirect_star_gitconfig:
        "shopt -s dotglob; echo x > {W}/*gitconfig" => "echo x > {W}/.gitconfig";
    red_own_shopt_and_redirect_star_gitconfig:
        "shopt -s dotglob && echo x > {W}/*gitconfig" => "echo x > {W}/.gitconfig";
    red_own_setopt_globdots_redirect_star_gitconfig:
        "setopt globdots; echo x > {W}/*gitconfig" => "echo x > {W}/.gitconfig";
    red_own_shopt_tee_star:
        "shopt -s dotglob; echo x | tee {W}/*" => "echo x | tee {W}/.gitconfig";
    red_own_setopt_globdots_tee_star:
        "setopt globdots; echo x | tee {W}/*" => "echo x | tee {W}/.gitconfig";
    red_own_setopt_glob_dots_upper_tee_star:
        "setopt GLOB_DOTS; echo x | tee {W}/*" => "echo x | tee {W}/.gitconfig";
    red_own_setopt_glob_dots_tee_star_gitconfig:
        "setopt glob_dots; echo x | tee {W}/*gitconfig" => "echo x | tee {W}/.gitconfig";
    red_own_shopt_tee_question_gitconfig:
        "shopt -s dotglob; echo x | tee {W}/?gitconfig" => "echo x | tee {W}/.gitconfig";
    red_own_shopt_truncate_star:
        "shopt -s dotglob; truncate -s0 {W}/*" => "truncate -s0 {W}/.gitconfig";
    red_own_shopt_truncate_star_gitconfig:
        "shopt -s dotglob; truncate -s0 {W}/*gitconfig" => "truncate -s0 {W}/.gitconfig";
    red_own_shopt_cp_star_gitconfig:
        "shopt -s dotglob; cp u.txt {W}/*gitconfig" => "cp u.txt {W}/.gitconfig";
    // Scratch {T} (inside the fixed temp root)
    red_scratch_shopt_redirect_star_gitconfig:
        "shopt -s dotglob; echo x > {T}/*gitconfig" => "echo x > {T}/.gitconfig";
    red_scratch_shopt_tee_star:
        "shopt -s dotglob; echo x | tee {T}/*" => "echo x | tee {T}/.gitconfig";
    red_scratch_setopt_glob_dots_tee_star_gitconfig:
        "setopt GLOB_DOTS; echo x | tee {T}/*gitconfig" => "echo x | tee {T}/.gitconfig";
    red_scratch_shopt_truncate_star:
        "shopt -s dotglob; truncate -s0 {T}/*" => "truncate -s0 {T}/.gitconfig";
    // hook TMPDIR {D}
    red_tmpdir_shopt_tee_star:
        "shopt -s dotglob; echo x | tee {D}/*" => "echo x | tee {D}/.gitconfig";
    red_tmpdir_shopt_tee_star_gitconfig:
        "shopt -s dotglob; echo x | tee {D}/*gitconfig" => "echo x | tee {D}/.gitconfig";
}

// ---------------------------------------------------------------------------
// Controls — the same globs without a dotglob switch (allow/allow at
// 4377a0816: without dotglob they cannot reach `.gitconfig`).
// ---------------------------------------------------------------------------

pins! {
    pin_own_redirect_star_gitconfig: "echo x > {W}/*gitconfig" => A;
    pin_own_tee_star: "echo x | tee {W}/*" => A;
    pin_own_tee_question_gitconfig: "echo x | tee {W}/?gitconfig" => A;
    pin_own_truncate_star: "truncate -s0 {W}/*" => A;
    pin_own_truncate_star_gitconfig: "truncate -s0 {W}/*gitconfig" => A;
    pin_own_cp_star_gitconfig: "cp u.txt {W}/*gitconfig" => A;
    pin_scratch_redirect_star_gitconfig: "echo x > {T}/*gitconfig" => A;
    pin_scratch_tee_star: "echo x | tee {T}/*" => A;
    pin_scratch_tee_star_gitconfig: "echo x | tee {T}/*gitconfig" => A;
    pin_scratch_truncate_star: "truncate -s0 {T}/*" => A;
    pin_tmpdir_tee_star: "echo x | tee {D}/*" => A;
    pin_tmpdir_tee_star_gitconfig: "echo x | tee {D}/*gitconfig" => A;
}

// zsh `(D)` qualifier and per-invocation switches: already non-Allow.
pins! {
    pin_own_zsh_d_qualifier_tee_star: "echo x | tee {W}/*(D)" => AD;
    pin_own_zsh_d_qualifier_tee_star_gitconfig: "echo x | tee {W}/*gitconfig(D)" => AD;
    pin_own_bash_o_dotglob_tee: "bash -O dotglob -c 'echo x | tee {W}/*gitconfig'" => AD;
    pin_own_zsh_o_globdots_tee: "zsh -o globdots -c 'echo x | tee {W}/*gitconfig'" => AD;
}

// Literal dot-name floors (also asserted inside each RED).
pins! {
    pin_literal_own_redirect: "echo x > {W}/.gitconfig" => D;
    pin_literal_own_tee: "echo x | tee {W}/.gitconfig" => D;
    pin_literal_own_truncate: "truncate -s0 {W}/.gitconfig" => D;
    pin_literal_own_cp: "cp u.txt {W}/.gitconfig" => D;
    pin_literal_scratch_redirect: "echo x > {T}/.gitconfig" => D;
    pin_literal_scratch_tee: "echo x | tee {T}/.gitconfig" => D;
    pin_literal_scratch_truncate: "truncate -s0 {T}/.gitconfig" => D;
    pin_literal_tmpdir_tee: "echo x | tee {D}/.gitconfig" => D;
}
