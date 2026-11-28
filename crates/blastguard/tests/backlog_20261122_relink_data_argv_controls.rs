#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Controls for backlog 3de4729f (blastguard relink layer, `src/detect/
//! relink.rs` `names_relink_verb` / `argv_is_relink_data` /
//! `names_resolve_plainly`).
//!
//! The fix lets a CLOSED list of programs whose argv runs nothing
//! (`readonly::argv_is_data_only` heads, `jq`, `echo`, `printf`, `git
//! [-C DIR|--no-pager|-P] commit|add|status`) carry relinking words (`ln`,
//! `mv`, `rsync`, …) as data, so a redirect on the same line is no longer Ask
//! because of them — but only when the program is named plainly: no prefix
//! assignment, no `/` in the command word, and a line that remaps no name (no
//! function definition, no alias/unalias/hash/enable/functions/autoload/eval/
//! source/., no PATH/path/FPATH/fpath assignment or export/declare/typeset/
//! local/readonly of them). Every other shape must keep the old whitespace
//! reading: an unmodelled command that names a relinking verb and a component
//! of the redirect target (`d` of `d/hosts`) makes that redirect Ask (or
//! stricter).
//!
//! This file asserts, in the MAIN session, with the scratch dir as cwd:
//! - not Allow (Ask or Deny), redirect target `d/hosts`, sentinel
//!   `/etc/bg-sentinel`, for programs off the list that really can relink
//!   (they exec an operand, or run a quoted argument as a command: `watch`,
//!   `strace`, `sed 'e'`, `gdb -ex shell`, `sh -c`, `git -c alias.x=!…`,
//!   `git rebase -x`);
//! - not Allow, same target, for listed programs that are NOT named plainly
//!   (prefix assignment, `./` command word, function definition, alias,
//!   `export PATH`, `hash -p`, prefix `PATH=`);
//! - Allow for listed programs whose relinking word is only data, writing
//!   `out.txt`;
//! - not Allow, target `d/hosts`, sentinel `/etc/bg-sentinel`, for listed
//!   programs whose output is piped into a shell (`echo … | sh`,
//!   `printf … | bash`, `jq -rn … | sh`) or for lines that remap a name
//!   through a shape the plain-name guard must not miss (`printf -v PATH`,
//!   `read PATH <<<`, a command word built by `$(echo hash)`, a quoted-split
//!   `export "PA""TH=…"`, an indirect `export $V=…`) — test
//!   `listed_programs_feeding_or_remapping_stay_not_allow`.
//!
//! `/etc/bg-sentinel` stands in for a real system path; nothing is executed —
//! the hook only judges the text.
//!
//! LEFT OUT on purpose (pre-existing fail-opens at base f9bb4b9aa, to be filed
//! separately, not controls of this fix):
//! - `vim -c 'silent !ln -s /etc/bg-sentinel d' -c q; echo x > d/hosts`
//! - `echo "$(ln -s /etc/bg-sentinel d)"; echo x > d/hosts`
//! - `jq . "$(ln -s /etc/bg-sentinel d)"; echo x > d/hosts`
//!
//! Written by an independent test author (not the implementer, CLAUDE.md
//! §2(a)). Fixture and harness copied from the earlier draft, which copied
//! `backlog_20261121_unmodelled_cmd_redirect_target_fp.rs`.
//!
//! DOES NOT PROVE:
//! - which layer produced a not-Allow verdict. A not-Allow row may be Ask/Deny
//!   for a reason other than the relink layer (e.g. a `sh -c`, `git -c`,
//!   alias, or PATH rule elsewhere), so these rows are controls on the
//!   VERDICT, not proof that `names_relink_verb` / `names_resolve_plainly`
//!   keep the whitespace reading for that shape;
//! - that the not-Allow rows would go Allow if the plain-name guard were
//!   removed (no mutation of the guard was run here);
//! - that the list is complete or that each listed program is safe in
//!   general (only these argv shapes);
//! - anything about the three left-out shapes above;
//! - subagent-mode verdicts, the reason text, or behaviour with a redirect
//!   target that does not share a component with the relinking command;
//! - that the Allow rows were Ask before the fix (no RED observed here for
//!   them; the base-vs-fix RED is in the sibling 20261121 file);
//! - for `listed_programs_feeding_or_remapping_stay_not_allow`: which layer
//!   blocks each row (a pipe into `sh`/`bash` or a `$(…)` command word may be
//!   Ask/Deny from a non-relink rule), that any row was Allow on some earlier
//!   revision (no RED observed for these rows here), or that every other way
//!   to pipe into a shell or remap PATH is caught (only these eight shapes).

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
    let base = neutral_base::neutral_base().join(format!("relink-data-argv-{tag}-{n}"));
    let scratch_root = fixed_tmp::canonical_fixed_temp_root().join(format!(
        "bg-relink-data-argv-{tag}-{}-{n}",
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
    std::fs::write(fx.scratch().join("f"), "x\n").unwrap();
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

/// Run the hook once in the MAIN session (cwd = `CLAUDE_PROJECT_DIR` = own
/// worktree). Empty stdout is pass-through = Allow.
fn run_main(fx: &Fx, command: &str) -> (String, String) {
    let cwd = fx.wt();
    let p = serde_json::json!({
        "session_id": "relink-data-argv-controls-20261122",
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

/// Prefix each line body with `cd <scratch> && ` so relative targets resolve
/// below the scratch dir.
fn in_scratch(fx: &Fx, bodies: &[&str]) -> Vec<String> {
    let dir = fx.scratch().to_string_lossy().into_owned();
    bodies.iter().map(|b| format!("cd {dir} && {b}")).collect()
}

/// Every command whose decision is NOT one of `want`, with decision and
/// reason (one run shows every failing shape).
fn not_in(fx: &Fx, cmds: &[String], want: &[&str]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| {
            let (d, r) = run_main(fx, c);
            (!want.contains(&d.as_str())).then(|| format!("{c:?} => {d} ({r})"))
        })
        .collect()
}

const STRICT: &[&str] = &["ask", "deny"];

#[test]
fn unlisted_programs_that_can_relink_stay_not_allow() {
    let fx = fixture("unlisted");
    let cmds = in_scratch(
        &fx,
        &[
            "watch 'busybox ln -s /etc/bg-sentinel d' & echo x > d/hosts",
            "strace -f ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "sed -n '1e ln -s /etc/bg-sentinel d' f; echo x > d/hosts",
            "gdb -batch -ex 'shell ln -s /etc/bg-sentinel d'; echo x > d/hosts",
            "sh -c 'ln -s /etc/bg-sentinel d'; echo x > d/hosts",
            "git -c alias.x='!ln -s /etc/bg-sentinel d' x; echo x > d/hosts",
            "git rebase -x 'ln -s /etc/bg-sentinel d' HEAD~1; echo x > d/hosts",
        ],
    );
    let bad = not_in(&fx, &cmds, STRICT);
    assert!(
        bad.is_empty(),
        "expected Ask or stricter (unlisted relinker):\n{}",
        bad.join("\n")
    );
}

#[test]
fn listed_programs_not_named_plainly_stay_not_allow() {
    let fx = fixture("plain");
    let cmds = in_scratch(
        &fx,
        &[
            "GIT_EXTERNAL_DIFF='sh -c \"ln -s /etc/bg-sentinel d\"' git status ln; echo x > d/hosts",
            "PATH=./bin cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "./echo ln -s /etc/bg-sentinel d; echo x > d/hosts",
            // `echo() { command ln "$@"; }; echo -s … d` does not name `ln` as
            // a whole word in the call, so the call shape below does.
            "cat() { \"$@\"; }; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "alias cat=ln; cat -s /etc/bg-sentinel d ln; echo x > d/hosts",
            "export PATH=./bin; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "hash -p ./bin/ln cat; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
        ],
    );
    let bad = not_in(&fx, &cmds, STRICT);
    assert!(
        bad.is_empty(),
        "expected Ask or stricter (listed program not named plainly):\n{}",
        bad.join("\n")
    );
}

#[test]
fn listed_programs_feeding_or_remapping_stay_not_allow() {
    let fx = fixture("feed");
    let cmds = in_scratch(
        &fx,
        &[
            "echo 'ln -s /etc/bg-sentinel d' | sh; echo x > d/hosts",
            "printf '%s\\n' 'ln -s /etc/bg-sentinel d' | bash; echo x > d/hosts",
            "jq -rn '\"ln -s /etc/bg-sentinel d\"' | sh; echo x > d/hosts",
            "printf -v PATH %s ./bin; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "read PATH <<< ./bin; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "$(echo hash) -p ./bin/x cat; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "export \"PA\"\"TH=./bin\"; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
            "V=PATH; export $V=./bin; cat ln -s /etc/bg-sentinel d; echo x > d/hosts",
        ],
    );
    let bad = not_in(&fx, &cmds, STRICT);
    assert!(
        bad.is_empty(),
        "expected Ask or stricter (listed program feeding a shell or remapping a name):\n{}",
        bad.join("\n")
    );
}

#[test]
fn listed_programs_carrying_relink_words_as_data_allow() {
    let fx = fixture("data");
    let cmds = in_scratch(
        &fx,
        &[
            "echo 'ln -s /etc/bg-sentinel d'; echo x > out.txt",
            "git -C . commit -qm \"mv the rsync helper\"; echo x > out.txt",
            "jq '.mv=\"ln\"' in.json; echo x > out.txt",
        ],
    );
    let bad = not_in(&fx, &cmds, &["allow"]);
    assert!(
        bad.is_empty(),
        "expected Allow (relink words are data):\n{}",
        bad.join("\n")
    );
}
