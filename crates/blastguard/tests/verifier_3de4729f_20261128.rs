#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent adversarial verification of backlog 3de4729f (branch
//! `fix-3de4729f`, HEAD ecb238398, base cf33a5b2c): `relink.rs` now reads a
//! line made only of closed-list data programs (`jq` / `echo` / `printf` /
//! `git commit|add|status` / `argv_is_data_only` / `cd`) as naming no
//! relinking verb, whatever its quoted words say. Written by a verifier, not
//! the implementer (CLAUDE.md §2(a)).
//!
//! FAIL-OPEN FOUND (`zsh_glob_qualifier_e_*`, RED, ignored): a zsh glob
//! qualifier `*(e:'CODE':)` is shell CODE run once per match, inside an
//! argv word of `echo` / `ls` / `jq` / `git add`. `line_is_relink_data`
//! returns true for those lines (the listed programs' argv is not checked
//! for literalness, and the AST — a bash parser — sees no command in the
//! word), so the `ln` the qualifier runs is no longer a relinker and the
//! following `echo x > d/hosts` is Allow. Measured at base cf33a5b2c: Ask;
//! at ecb238398: Allow. Execution observed 2026-11-28 in the verifier's
//! scratch dir: `zsh -fc "echo *(e:' ln -s ../tgt d ':)"` in a directory
//! holding one file created `d -> ../tgt`. The session shell is zsh.
//!
//! PRE-EXISTING (`for_list_substitution_*`, RED, ignored): a relinker in a
//! `for … in $( … )` list is Allow at base AND at the fix — not caused by
//! this change, recorded so it is not lost.
//!
//! DOES NOT PROVE: subagent-mode verdicts; reason texts; that the shapes
//! below are the only ones; Allow/Ask of the intended relaxation rows (the
//! implementer's own tests cover those).

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
    let base = neutral_base::neutral_base().join(format!("verifier-3de4729f-{tag}-{n}"));
    let scratch_root = fixed_tmp::canonical_fixed_temp_root().join(format!(
        "bg-verifier-3de4729f-{tag}-{}-{n}",
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
        "session_id": "verifier-3de4729f-20261128",
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

/// zsh `e` glob qualifier: the quoted text is shell code run per match.
/// Base cf33a5b2c: every row Ask. ecb238398: every row Allow.
const ZSH_E_QUALIFIER: &[&str] = &[
    "echo *(e:' ln -s /etc d ':); echo x > d/hosts",
    "echo *(e:' busybox ln -s /etc d ':); echo x > d/hosts",
    "ls *(e:' ln -s /etc d ':); echo x > d/hosts",
    "jq . *(e:' ln -s /etc d ':); echo x > d/hosts",
    "git add *(e:' ln -s /etc d ':); echo x > d/hosts",
    "echo *(e.' ln -s /etc d '.); echo x > d/hosts",
    "echo *(e:' python3 -c \"import os; os.symlink(\\\"/etc\\\", \\\"d\\\")\" ':); echo x > d/hosts",
];

#[test]
#[ignore = "RED: fail-open of 3de4729f — zsh *(e:'CODE':) glob qualifier inside a listed data program's argv runs a relinker; line_is_relink_data reads the line as data so `> d/hosts` is Allow (base: Ask)"]
fn zsh_glob_qualifier_e_running_relinker_on_data_line_stays_not_allow() {
    let fx = fixture("zsh-e");
    let bad = not_in(&fx, &in_scratch(&fx, ZSH_E_QUALIFIER), &["ask", "deny"]);
    assert!(
        bad.is_empty(),
        "expected Ask or Deny (zsh e-qualifier runs ln):\n{}",
        bad.join("\n")
    );
}

/// Pre-existing (Allow at base AND fix): a relinker in a `for` word list.
const FOR_LIST: &[&str] = &[
    "for i in $( busybox ln -s /etc d ); do echo; done; echo x > d/hosts",
    "for i in $( python3 -c 'import os; os.symlink(\"/etc\",\"d\")' ); do echo; done; echo x > d/hosts",
];

#[test]
#[ignore = "RED: pre-existing at base cf33a5b2c (not caused by 3de4729f) — relinker inside a for-list $( ) is not seen; `> d/hosts` is Allow"]
fn for_list_substitution_relinker_stays_not_allow() {
    let fx = fixture("for-list");
    let bad = not_in(&fx, &in_scratch(&fx, FOR_LIST), &["ask", "deny"]);
    assert!(
        bad.is_empty(),
        "expected Ask or Deny (for-list substitution relinks):\n{}",
        bad.join("\n")
    );
}

/// System-harm targets on data lines: Deny at base and fix.
const SYSTEM_DENY: &[&str] = &[
    "cd /etc && echo 'ln -s x hosts'; echo x > hosts",
    "cd / && echo rsync; echo x > etc/hosts",
    "echo 'rsync'; echo x > /etc/hosts",
    "jq '.a=\"rsync\"' in.json > /etc/hosts",
    "jq -n '\"rsync\"'; ln -s /etc d; echo x > d/hosts",
    "echo hi > >( busybox ln -s /etc d ); echo x > d/hosts",
    "git -C . -c core.hooksPath=./h commit -m ln; echo x > d/hosts",
];

#[test]
fn control_system_targets_on_data_lines_stay_deny() {
    let fx = fixture("sys");
    let bad = not_in(&fx, &in_scratch(&fx, SYSTEM_DENY), &["deny"]);
    assert!(
        bad.is_empty(),
        "expected Deny (system harm):\n{}",
        bad.join("\n")
    );
}

/// Shapes tried against the relaxation that stay Ask or Deny at the fix
/// (all measured Ask/Deny at base too).
const CONTROLS: &[&str] = &[
    // zsh ${(e)...}
    "echo ${(e):-'$( busybox ln -s /etc d )'}; echo x > d/hosts",
    "echo ${(e)${:-'$( busybox ln -s /etc d )'}}; echo x > d/hosts",
    // bash arithmetic in [[ -eq ]] with a single-quoted subscript
    "[[ 'a[$( busybox ln -s /etc d )]' -eq 0 ]]; echo x > d/hosts",
    "[[ 'a[$( ln -s /etc d )]' -eq 0 ]]; echo x > d/hosts",
    "[[ -v 'a[$( busybox ln -s /etc d )]' ]]; echo x > d/hosts",
    "echo $(( 'a[$( busybox ln -s /etc d )]' )); echo x > d/hosts",
    "echo \"${a['$( busybox ln -s /etc d )']}\"; echo x > d/hosts",
    // unquoted heredoc body command substitution
    "cat <<EOF\n$( busybox ln -s /etc d )\nEOF\necho x > d/hosts",
    "cat <<EOF\n$( ln -s /etc d )\nEOF\necho x > d/hosts",
    "cat <<EOF\n`busybox ln -s /etc d`\nEOF\necho x > d/hosts",
    "jq . <<EOF\n$( busybox ln -s /etc d )\nEOF\necho x > d/hosts",
    // here-string with substitution
    "cat <<< \"$( busybox ln -s /etc d )\"; echo x > d/hosts",
    // process substitution / command substitution in listed program argv
    "cat <( busybox ln -s /etc d ); echo x > d/hosts",
    "echo \"$( busybox ln -s /etc d )\"; echo x > d/hosts",
    "echo `busybox ln -s /etc d`; echo x > d/hosts",
    "cd \"$( busybox ln -s /etc d; echo . )\" && echo x > d/hosts",
    "echo ${x:-$( busybox ln -s /etc d )}; echo x > d/hosts",
    // case / for / if / subshell / brace compound
    "case $( busybox ln -s /etc d ) in *) echo ;; esac; echo x > d/hosts",
    "( busybox ln -s /etc d ); echo x > d/hosts",
    "{ busybox ln -s /etc d; }; echo x > d/hosts",
    "if busybox ln -s /etc d; then echo; fi; echo x > d/hosts",
    "while busybox ln -s /etc d; do break; done; echo x > d/hosts",
    "! busybox ln -s /etc d; echo x > d/hosts",
    "time busybox ln -s /etc d; echo x > d/hosts",
    "coproc busybox ln -s /etc d; echo x > d/hosts",
    // trap/eval/command/builtin/exec/xargs/env/nohup wrappers
    "trap 'busybox ln -s /etc d' EXIT; echo x > d/hosts",
    "eval 'busybox ln -s /etc d'; echo x > d/hosts",
    "command echo; busybox ln -s /etc d; echo x > d/hosts",
    "echo d | xargs busybox ln -s /etc; echo x > d/hosts",
    "env busybox ln -s /etc d; echo x > d/hosts",
    // printf -v variants
    "printf -v PATH %s ./bin; cat ln -s /etc d; echo x > d/hosts",
    "printf -vPATH %s ./bin; cat ln -s /etc d; echo x > d/hosts",
    "printf -- -v; cat ln -s /etc d; echo x > d/hosts",
    // zsh print -v / read / typeset
    "print -v PATH ./bin; cat ln -s /etc d; echo x > d/hosts",
    // data program writes a hook that git commit then runs
    "printf 'busybox ln -s /etc d\\n' > .git/hooks/pre-commit; git commit -m x; echo x > d/hosts",
    "echo 'busybox ln -s /etc d' >> .git/hooks/pre-commit; git commit -m x; echo x > d/hosts",
    // ~/.claude targets (Ask, not Deny, at base too)
    "cd ~/.claude && echo 'mv settings.json x'; echo x > settings.json",
    "git commit -m rsync; echo x > ~/.claude/settings.json",
    // the real relinker mixed with data programs
    "echo rsync; busybox ln -s /etc d; echo x > d/hosts",
    "echo rsync && ditto /etc d && echo x > d/hosts",
    "echo rsync | python3 -c 'import os; os.symlink(\"/etc\",\"d\")'; echo x > d/hosts",
    // function across lines
    "echo() { busybox ln -s /etc \"$@\"; }\necho d; echo x > d/hosts",
    "function echo { busybox ln -s /etc \"$@\"; }\necho d; echo x > d/hosts",
];

#[test]
fn control_shapes_against_data_line_relaxation_stay_not_allow() {
    let fx = fixture("ctl");
    let bad = not_in(&fx, &in_scratch(&fx, CONTROLS), &["ask", "deny"]);
    assert!(
        bad.is_empty(),
        "expected Ask or Deny (control):\n{}",
        bad.join("\n")
    );
}
