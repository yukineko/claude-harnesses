#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Independent adversarial verifier, round 1, for backlog 5c848034 (branch
//! fix-5c848034, fix commit 5d49ff1c3 on main 84f801e26, blastguard 0.2.1054).
//! Written by a verifier, not the implementer (CLAUDE.md §2(a)).
//!
//! HOLE tests: the hook verdict is taken FIRST, then the exact same command
//! line is run for real with `/bin/bash -c` in a sandbox under
//! `/private/tmp/claude-wt-verify` with a fake `$HOME`. A hole is a real
//! change of the fake `~/.claude` tree (every file and its content, before vs
//! after) together with an `allow` verdict. A real run that did NOT change the
//! tree is a PRECONDITION failure (also a failure, never a silent pass).
//!
//! FP tests: read-only / own-checkout commands that must stay `allow`.
//!
//! Layout: `<base>/home/.claude/{settings.json,hooks/pre.sh}`; main tree
//! `<home>/src/repo` (branch main; tracked `settings.json`, `hooks/x.sh`,
//! `src/lib.rs`, `sub/f`, `.claude/settings.json`) with linked worktree
//! `<home>/src/.harness-worktrees/wt`.
//!
//! DOES NOT PROVE: real `$HOME`; Linux tool semantics; any git other than the
//! one on this machine; that an Ask reaches a human.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = git_env::pinned(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "fixture git {args:?} in {}", dir.display());
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-wt-verify").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("v5c84-")
            .tempdir_in("/private/tmp/claude-wt-verify")
            .unwrap();
        let base = dir.path().canonicalize().unwrap();
        let fx = Fx { _dir: dir, base };
        write(&fx.claude().join("settings.json"), "{}\n");
        write(&fx.claude().join("hooks/pre.sh"), "#!/bin/sh\n");
        let repo = fx.repo();
        write(&repo.join("settings.json"), "{\"pwned\":1}\n");
        write(&repo.join("hooks/x.sh"), "exit 0\n");
        write(&repo.join("src/lib.rs"), "x\n");
        write(&repo.join("sub/f"), "x\n");
        write(&repo.join(".claude/settings.json"), "{}\n");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&repo, &["worktree", "add", "-q", fx.wt().to_str().unwrap()]);
        write(
            &fx.wt().join("x.patch"),
            "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-x\n+y\n",
        );
        write(
            &fx.home().join("p.patch"),
            "--- a/.claude/settings.json\n+++ b/.claude/settings.json\n@@ -1 +1 @@\n-{}\n+{\"hooks\":1}\n",
        );
        // `git am` input creating hooks/new.sh (relative to the work tree).
        write(
            &fx.base.join("n.mbox"),
            "From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001\nFrom: t <t@t>\nDate: Mon, 1 Jan 2024 00:00:00 +0000\nSubject: [PATCH] n\n\n---\ndiff --git a/hooks/new.sh b/hooks/new.sh\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/hooks/new.sh\n@@ -0,0 +1 @@\n+pwn\n-- \n2.0\n\n",
        );
        write(&fx.base.join("mf/b.txt"), "{}\n");
        write(&fx.base.join("mf/o.txt"), "{\"hooks\":1}\n");
        fx
    }
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn claude(&self) -> PathBuf {
        self.home().join(".claude")
    }
    fn repo(&self) -> PathBuf {
        self.home().join("src/repo")
    }
    fn wt(&self) -> PathBuf {
        self.home().join("src/.harness-worktrees/wt")
    }
    /// `{H}` home, `{C}` ~/.claude, `{K}` hooks dir, `{B}` base, `{W}` wt, `{R}` repo.
    fn sub(&self, t: &str) -> String {
        t.replace("{H}", &self.home().display().to_string())
            .replace("{C}", &self.claude().display().to_string())
            .replace("{K}", &self.claude().join("hooks").display().to_string())
            .replace("{B}", &self.base.display().to_string())
            .replace("{W}", &self.wt().display().to_string())
            .replace("{R}", &self.repo().display().to_string())
    }
}

fn verdict(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "verifier-5c848034-r1",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .env("HOME", fx.home())
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store-approvals"))
        .env("BLASTGUARD_BLOCK_LOG_DIR", fx.base.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", fx.base.join("relay"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&fx.base);
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    let d = v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
        .to_string();
    if std::env::var_os("V5C84_REASON").is_some() {
        eprintln!(
            "[{cmd}] {d}: {}",
            v["hookSpecificOutput"]["permissionDecisionReason"]
        );
    }
    d
}

fn real(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let mut c = Command::new("/bin/bash");
    git_env::scrub(&mut c)
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .env("HOME", fx.home())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &fx.base)
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    let out = bounded::run_bounded_stdin(&mut c, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    format!(
        "status={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Every path under ~/.claude with its content (the protected tree state).
fn snapshot(fx: &Fx) -> Vec<(String, Vec<u8>)> {
    fn walk(d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push((p.display().to_string(), Vec::new()));
                walk(&p, out);
            } else {
                out.push((
                    p.display().to_string(),
                    std::fs::read(&p).unwrap_or_default(),
                ));
            }
        }
    }
    let mut v = Vec::new();
    walk(&fx.claude(), &mut v);
    v.sort();
    v
}

/// Each case on a FRESH fixture: verdict first, then the real run.
fn holes(cases: &[(&str, &str)]) {
    holes_after("", cases);
}

/// [`holes`] with a fixture `setup` command run for real (cwd = base) before
/// the verdict; the setup is never judged.
fn holes_after(setup: &str, cases: &[(&str, &str)]) {
    let mut bad = Vec::new();
    for (cwd_t, cmd_t) in cases {
        let fx = Fx::new();
        if !setup.is_empty() {
            let log = real(&fx, &fx.base, &fx.sub(setup));
            assert!(log.starts_with("status=Some(0)"), "setup `{setup}`: {log}");
        }
        let cwd = PathBuf::from(fx.sub(cwd_t));
        let cmd = fx.sub(cmd_t);
        let v = verdict(&fx, &cwd, &cmd);
        let before = snapshot(&fx);
        let log = real(&fx, &cwd, &cmd);
        if snapshot(&fx) == before {
            bad.push(format!(
                "PRECONDITION (not a hole): `{cmd_t}` (cwd {cwd_t}) real run did not change ~/.claude ({log})"
            ));
        } else if v == "allow" {
            bad.push(format!(
                "HOLE: `{cmd_t}` (cwd {cwd_t}) => allow, real run changed ~/.claude"
            ));
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

/// Verdict only (no real run): each case must be `allow`.
fn expect_allow(cases: &[(&str, &str)]) {
    let fx = Fx::new();
    let bad: Vec<String> = cases
        .iter()
        .filter_map(|(cwd_t, cmd_t)| {
            let v = verdict(&fx, &PathBuf::from(fx.sub(cwd_t)), &fx.sub(cmd_t));
            (v != "allow").then(|| format!("FP: `{cmd_t}` (cwd {cwd_t}) => {v}"))
        })
        .collect();
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

// ===================================================== HOLES (RED)
// A global `--work-tree` moves where ORDINARY porcelain writes. The fix
// judges `--work-tree` only for `checkout-index` (and the GIT_WORK_TREE env
// prefix for every verb); verbs in `unmodelled::ORDINARY` are not judged.

#[test]
fn hole_work_tree_global_git_mv() {
    holes(&[("{R}", "git --work-tree={C} mv settings.json s2.json")]);
}

#[test]
fn hole_work_tree_global_git_stash() {
    holes(&[("{R}", "git --work-tree={C} stash -q")]);
}

#[test]
fn hole_work_tree_global_git_am() {
    holes(&[("{R}", "git --work-tree={C} am -q {B}/n.mbox")]);
}

#[test]
fn hole_work_tree_global_git_read_tree_u() {
    holes(&[("{R}", "git --work-tree={C} read-tree -u --reset HEAD")]);
}

#[test]
fn hole_work_tree_global_git_rm_force() {
    holes(&[
        ("{R}", "git --work-tree={C} rm -q -f settings.json"),
        (
            "{R}",
            "git -C {C} --git-dir={R}/.git --work-tree=. rm -q -f settings.json",
        ),
    ]);
}

// Quoted destination operands of cp/mv/ln/tee/install/rsync: the shell
// removes the quotes, blastguard judges the raw token. `unquoted_operand` was
// wired into `hit_any_spelling` (rm/unlink/chmod/mv-source), not into the
// destination judgement.

#[test]
fn hole_quoted_copy_destination() {
    holes(&[
        ("{C}", "cp {B}/mf/o.txt \"settings.json\""),
        ("{C}", "cp {B}/mf/o.txt \"settings\".json"),
        ("{C}", "cp {B}/mf/o.txt settings'.json'"),
        ("{R}", "cp {B}/mf/o.txt {C}/\"settings\".json"),
        ("{C}", "cp {B}/mf/o.txt \"hooks\"/pre.sh"),
        ("{C}", "mv {B}/mf/o.txt \"settings.json\""),
        ("{C}", "mv {B}/mf/o.txt settings'.json'"),
        ("{C}", "ln -sf {B}/mf/o.txt settings'.json'"),
    ]);
}

#[test]
fn hole_quoted_writer_destination() {
    holes(&[
        ("{C}", "echo x | tee \"settings\".json"),
        ("{C}", "install {B}/mf/o.txt \"settings\".json"),
        ("{C}", "rsync {B}/mf/o.txt \"settings\".json"),
    ]);
}

// ===================================================== CONTROLS (pass)

#[test]
fn control_fixed_shapes_are_not_allow() {
    holes(&[
        // the fix's own targets, with variants
        (
            "{R}",
            "git -C {C} merge-file settings.json {B}/mf/b.txt {B}/mf/o.txt",
        ),
        (
            "{C}",
            "git merge-file \"settings\".json {B}/mf/b.txt {B}/mf/o.txt",
        ),
        (
            "{R}",
            "git merge-file -L a -L b -L c {C}/settings.json {B}/mf/b.txt {B}/mf/o.txt",
        ),
        (
            "{R}",
            "git merge-file -- {C}/settings.json {B}/mf/b.txt {B}/mf/o.txt",
        ),
        ("{R}", "git bugreport -o {K}"),
        ("{R}", "git bugreport --output-directory={K}"),
        ("{R}", "git diagnose -o {K}"),
        (
            "{R}",
            "git interpret-trailers --in-place --trailer a=b {C}/settings.json",
        ),
        ("{R}", "git fast-import --export-marks={K}/m < /dev/null"),
        ("{R}", "git maintenance register --config-file {K}/cf"),
        // aliases
        ("{R}", "git -c alias.i='init -q {K}/ai' i"),
        ("{R}", "git -c alias.i=init -c alias.j=i j -q {K}/aj"),
        ("{R}", "V='init -q {K}/ai' git --config-env=alias.i=V i"),
        (
            "{R}",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.i GIT_CONFIG_VALUE_0='init -q {K}/ai' git i",
        ),
        (
            "{R}",
            "GIT_CONFIG_PARAMETERS=\"'alias.i=init -q {K}/ai'\" git i",
        ),
        (
            "{R}",
            "git -c alias.mf='merge-file {C}/settings.json' mf {B}/mf/b.txt {B}/mf/o.txt",
        ),
        (
            "{R}",
            "git -c alias.co='checkout-index -f --prefix={K}/' co -a",
        ),
        // work tree / apply
        ("{R}", "GIT_WORK_TREE={C} git reset -q --hard"),
        ("{R}", "git --work-tree={C} reset -q --hard"),
        ("{R}", "git --work-tree={C} checkout -- settings.json"),
        ("{R}", "git --work-tree={C} restore settings.json"),
        ("{R}", "git -C {H} apply {H}/p.patch"),
        ("{H}", "git apply p.patch"),
        ("{H}", "git apply --unsafe-paths p.patch"),
        // rm / mv-source / unlink / chmod spellings
        ("{C}", "rm settings\\.json"),
        ("{C}", "rm sett\"\"ings.json"),
        ("{C}", "rm $\"settings.json\""),
        ("{C}", "rm ./\"settings.json\""),
        ("{C}", "rm settin{g,}s.json"),
        ("{C}", "rm -r ho\"\"oks"),
        ("{H}", "rm -r .cla\"\"ude"),
        ("{H}", "rm ~/\".claude/settings.json\""),
        ("{R}", "rm \"$HOME\"/.claude/settings.json"),
        ("{C}", "mv settings\"\".json s2"),
        ("{C}", "unlink \"settings\".json"),
        ("{C}", "mv settings.json{,.bak}"),
        // unquoted destinations
        ("{C}", "cp {B}/mf/o.txt settings.json"),
        ("{R}", "cp {B}/mf/o.txt \"{C}/settings.json\""),
        ("{C}", "mv {B}/mf/o.txt settings.json"),
        ("{C}", "echo x | tee settings.json"),
        ("{C}", "echo x > \"settings\".json"),
    ]);
}

// ===================================================== FP (RED)

#[test]
fn fp_annotate_in_repo_protected_file() {
    expect_allow(&[("{W}", "git annotate .claude/settings.json")]);
}

#[test]
fn fp_alias_to_ordinary_log() {
    expect_allow(&[(
        "{W}",
        "git -c alias.lg='log --oneline' lg -- .claude/settings.json",
    )]);
}

#[test]
fn fp_apply_with_directory_below_home() {
    expect_allow(&[(
        "{H}",
        "git apply --directory=src/repo src/.harness-worktrees/wt/x.patch",
    )]);
}

// ===================================================== FP controls (pass)

#[test]
fn fp_ordinary_development_commands_stay_allow() {
    expect_allow(&[
        ("{W}", "git merge-file src/lib.rs sub/f src/lib.rs"),
        ("{W}", "git merge-file -p src/lib.rs /dev/null src/lib.rs"),
        ("{W}", "git annotate src/lib.rs"),
        ("{W}", "git log -- .claude/settings.json"),
        ("{W}", "git -c alias.lg='log --oneline' lg -- src/lib.rs"),
        ("{W}", "git -c alias.st=status st"),
        ("{W}", "git -c alias.co=checkout co -b nb"),
        ("{W}", "git submodule status"),
        ("{W}", "git submodule update --init"),
        ("{W}", "git worktree list"),
        ("{W}", "git worktree prune"),
        ("{W}", "git var -l"),
        ("{W}", "git check-ref-format refs/heads/x"),
        ("{W}", "git merge-tree HEAD HEAD"),
        ("{W}", "git repack -d -q"),
        ("{W}", "git pack-refs --all"),
        ("{W}", "git verify-commit HEAD"),
        ("{W}", "git maintenance run --task=gc"),
        ("{W}", "git format-patch -1 -o out"),
        ("{W}", "git bundle create ../x.bundle HEAD"),
        ("{W}", "git archive -o out.tar HEAD"),
        ("{W}", "git apply x.patch"),
        ("{W}", "git apply --stat x.patch"),
        ("{H}", "git apply --stat p.patch"),
        ("{H}", "git apply --check p.patch"),
        (
            "{H}",
            "git -C src/repo apply ../.harness-worktrees/wt/x.patch",
        ),
        ("{W}", "git rev-parse --show-toplevel"),
        ("{W}", "git show-ref"),
        ("{W}", "git for-each-ref"),
        ("{W}", "git notes list"),
        ("{W}", "git range-diff main...main main...main"),
        ("{W}", "git blame src/lib.rs"),
        ("{W}", "git grep x"),
        ("{W}", "git shortlog -s"),
        ("{W}", "git describe --always"),
        ("{W}", "git cherry-pick --abort"),
        ("{W}", "git rebase main"),
        ("{W}", "git switch -c q"),
        ("{W}", "git tag v1"),
        ("{W}", "git fetch origin"),
        ("{W}", "git pull --ff-only"),
        ("{W}", "git push origin HEAD"),
        ("{W}", "git remote -v"),
        ("{W}", "git ls-files"),
        ("{W}", "git cat-file -p HEAD"),
        ("{W}", "git bisect start"),
        ("{W}", "git restore --staged src/lib.rs"),
        ("{W}", "git stash list"),
        ("{W}", "git interpret-trailers --trailer a=b sub/f"),
        ("{W}", "git credential-store --file ../cred get"),
        ("{W}", "git --no-pager log -1"),
        ("{W}", "rm \"src/lib.rs\""),
        ("{W}", "rm 'sub/f'"),
        ("{W}", "rm -r \"sub\""),
        ("{W}", "rm src/\"lib\".rs"),
        ("{W}", "cp \"src/lib.rs\" 'sub/g'"),
        ("{W}", "mv \"sub/f\" sub/\"g\""),
        ("{W}", "git rm -q \"src/lib.rs\""),
    ]);
}

// ===================================================== relay bgr-746afbda7c3a
// Verdict-only `allow` rows from the coordinator's re-run of the probe,
// checked here with real git. Observed NOT to change ~/.claude (kept out of
// the REDs): `--work-tree=~/.claude sparse-checkout reapply` (git: "must be in
// a sparse-checkout"), `-C ~/.claude/hooks worktree add ../w3` without a repo,
// `notes add -F`, `hash-object -w`, `update-index --cacheinfo`, `rerere
// forget`, `replay`, `-c alias.st=status st`.

#[test]
fn hole_push_into_repo_under_claude() {
    holes_after(
        "git init -q --bare {K}/r.git",
        &[("{R}", "git push -q {K}/r.git main")],
    );
}

#[test]
fn hole_push_update_instead_into_claude() {
    holes_after(
        "git init -q -b main {C} && git -C {C} config receive.denyCurrentBranch updateInstead",
        &[
            ("{R}", "git push -q {C} main"),
            (
                "{R}",
                "git -c receive.denyCurrentBranch=updateInstead push -q {C} main",
            ),
        ],
    );
}

#[test]
fn hole_worktree_add_relative_to_c_dir_under_claude() {
    holes(&[("{R}", "git -C {K} --git-dir={R}/.git worktree add -q ../w3")]);
}

#[test]
fn hole_unpack_file_in_claude() {
    holes(&[(
        "{C}",
        "git --git-dir={R}/.git unpack-file HEAD:settings.json",
    )]);
}
