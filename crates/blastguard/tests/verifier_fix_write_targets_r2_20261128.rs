#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Independent adversarial verifier, round 2, for branch fix-write-targets
//! (tip 3ad6f54e2, blastguard 0.2.1024; backlog 4e7fc8e2 + f63b97b3). Written by
//! a verifier, not the implementer (CLAUDE.md §2(a)).
//!
//! HOLE tests: the hook verdict is taken FIRST (the hook runs before the
//! command), then the exact same command line is run for real with
//! `/bin/bash -c` in a sandbox under `/private/tmp/claude-wt-verify` with a
//! fake `$HOME`, and the protected path is checked to have really been
//! written / deleted (`PRECONDITION`). Only a real write/delete plus an
//! `allow` verdict is a HOLE.
//!
//! FP tests: ordinary harness operations in the same layout must stay
//! `allow`.
//!
//! Layout: `<base>/home/.claude/{settings.json,hooks/pre.sh}`; main tree
//! `<home>/src/repo` (branch main; tracked `settings.json`, `hooks/x.sh`,
//! `src/`, `sub/`, `target/`, `build/`, `.claude/settings.json`) with linked
//! worktree `<home>/src/.harness-worktrees/wt`.
//!
//! DOES NOT PROVE: real `$HOME`; Linux tool semantics; any git other than the
//! one on this machine; that an Ask reaches a human; that the pre-fix binary
//! gave the same FP verdicts (no baseline binary was measured — an FP here is
//! "not allow now", not "changed by this branch").
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
            .prefix("vr2-")
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
        write(&repo.join("src/a/b.rs"), "x\n");
        write(&repo.join("sub/f"), "x\n");
        write(&repo.join("target/o"), "x\n");
        write(&repo.join("build/o"), "x\n");
        write(&repo.join(".claude/settings.json"), "{}\n");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        std::fs::create_dir_all(fx.store().join("x")).unwrap();
        git(&repo, &["worktree", "add", "-q", fx.wt().to_str().unwrap()]);
        write(
            &fx.wt().join("x.patch"),
            "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-x\n+y\n",
        );
        write(
            &fx.home().join("p.patch"),
            "--- a/.claude/settings.json\n+++ b/.claude/settings.json\n@@ -1 +1 @@\n-{}\n+{\"hooks\":1}\n",
        );
        write(&fx.base.join("mf/b.txt"), "{}\n");
        write(&fx.base.join("mf/o.txt"), "{\"hooks\":1}\n");
        write(
            &fx.base.join("m.mbox"),
            "From 0 Mon Sep 17 00:00:00 2001\nSubject: a\n\nbody\n",
        );
        fx
    }
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn claude(&self) -> PathBuf {
        self.home().join(".claude")
    }
    fn hooks(&self) -> String {
        self.claude().join("hooks").display().to_string()
    }
    fn settings(&self) -> PathBuf {
        self.claude().join("settings.json")
    }
    fn repo(&self) -> PathBuf {
        self.home().join("src/repo")
    }
    fn store(&self) -> PathBuf {
        self.home().join("src/.harness-worktrees")
    }
    fn wt(&self) -> PathBuf {
        self.store().join("wt")
    }
    /// `{H}` home, `{C}` ~/.claude, `{K}` hooks dir, `{B}` base, `{W}` wt, `{R}` repo.
    fn sub(&self, t: &str) -> String {
        t.replace("{H}", &self.home().display().to_string())
            .replace("{C}", &self.claude().display().to_string())
            .replace("{K}", &self.hooks())
            .replace("{B}", &self.base.display().to_string())
            .replace("{W}", &self.wt().display().to_string())
            .replace("{R}", &self.repo().display().to_string())
    }
}

fn verdict(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "verifier-fix-write-targets-r2",
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
    if std::env::var_os("VR2_REASON").is_some() {
        eprintln!(
            "[{cmd}] {d}: {}",
            v["hookSpecificOutput"]["permissionDecisionReason"]
        );
    }
    d
}

/// Run `cmd` for real with bash in the sandbox.
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
        "status={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// What proves the real command touched a protected path.
enum Effect {
    /// The file now exists.
    Created(&'static str),
    /// The file no longer exists.
    Deleted(&'static str),
    /// `~/.claude/settings.json` no longer reads `{}`.
    SettingsChanged,
}

fn effect_seen(fx: &Fx, e: &Effect) -> bool {
    match e {
        Effect::Created(p) => Path::new(&fx.sub(p)).exists(),
        Effect::Deleted(p) => !Path::new(&fx.sub(p)).exists(),
        Effect::SettingsChanged => std::fs::read_to_string(fx.settings())
            .map(|s| s != "{}\n")
            .unwrap_or(true),
    }
}

/// Each case on a FRESH fixture: verdict first, then the real run. Collects
/// PRECONDITION failures (not a hole) and HOLEs separately; only HOLEs fail.
/// A PRECONDITION failure is also a failure, so a non-hole is never counted
/// as covered by accident.
fn holes(cases: &[(&str, &str, Effect)], cwd_of: fn(&Fx) -> PathBuf) {
    let mut bad = Vec::new();
    for (cwd_rel, cmd_t, eff) in cases {
        let fx = Fx::new();
        let cwd = if cwd_rel.is_empty() {
            cwd_of(&fx)
        } else {
            PathBuf::from(fx.sub(cwd_rel))
        };
        let cmd = fx.sub(cmd_t);
        let v = verdict(&fx, &cwd, &cmd);
        let log = real(&fx, &cwd, &cmd);
        if !effect_seen(&fx, eff) {
            bad.push(format!("PRECONDITION (not a hole): `{cmd_t}` real run did not touch the protected path ({log})"));
        } else if v == "allow" {
            bad.push(format!(
                "HOLE: `{cmd_t}` (cwd {cwd_rel}) => allow, real git/shell touched the protected path"
            ));
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

fn repo_cwd(fx: &Fx) -> PathBuf {
    fx.repo()
}

// ================================================================ HOLES

#[test]
fn hole_generic_fallthrough_subcommands_that_write_a_named_path() {
    // The fall-through scans only `--output` (archive: `-o`). These git
    // subcommands write a path named on the command line by another spelling.
    holes(
        &[
            // merge-file writes the merge result into its FIRST operand.
            ("", "git merge-file {C}/settings.json {B}/mf/b.txt {B}/mf/o.txt", Effect::SettingsChanged),
            // mailsplit -o<dir> writes 0001 into <dir>.
            ("", "git mailsplit -o{K} {B}/m.mbox", Effect::Created("{K}/0001")),
            // fast-export --export-marks=F writes F.
            ("", "git fast-export --export-marks={K}/marks HEAD > /dev/null", Effect::Created("{K}/marks")),
            // credential-store --file=F store writes F.
            (
                "",
                "printf 'protocol=https\\nhost=x\\nusername=u\\npassword=p\\n\\n' | git credential-store --file={K}/cred store",
                Effect::Created("{K}/cred"),
            ),
        ],
        repo_cwd,
    );
}

#[test]
fn hole_alias_hides_init_subcommand() {
    // A non-`!` alias is not exec-banned; `sub` is the alias name, so the
    // init arm never runs.
    holes(
        &[(
            "",
            "git -c alias.i=init i -q {K}/ai",
            Effect::Created("{K}/ai/.git/HEAD"),
        )],
        repo_cwd,
    );
}

#[test]
fn hole_work_tree_moved_by_global_option_or_config() {
    // `--work-tree` global without `-a` / `core.worktree` via `-c`: the
    // tracked settings.json / hooks/x.sh land in ~/.claude.
    holes(
        &[
            (
                "",
                "git --work-tree={C} checkout-index -f -- settings.json",
                Effect::SettingsChanged,
            ),
            // NOT a hole (observed 2026-11-28): `git -c core.worktree={C}
            // checkout-index -a -f` exits 0 and writes nothing into ~/.claude.
            (
                "",
                "git --work-tree={C} reset -q --hard",
                Effect::Created("{K}/x.sh"),
            ),
        ],
        repo_cwd,
    );
}

#[test]
fn hole_git_apply_in_an_ancestor_of_claude() {
    // `git apply` writes the paths its patch names below the directory it runs
    // in; run in $HOME a patch for `.claude/settings.json` rewrites it.
    holes(
        &[
            ("", "git -C {H} apply {H}/p.patch", Effect::SettingsChanged),
            ("{H}", "git apply p.patch", Effect::SettingsChanged),
        ],
        repo_cwd,
    );
}

#[test]
fn hole_git_rm_recursive_pathspec_spellings_in_main_tree() {
    // `git rm -r src` in the shared main tree is Ask on this branch. Pathspec
    // magic / globs delete the same tracked files.
    holes(
        &[
            (
                "{R}/sub",
                "git rm -r -q ':/src'",
                Effect::Deleted("{R}/src/lib.rs"),
            ),
            (
                "{R}/sub",
                "git rm -r -q ':(top)src'",
                Effect::Deleted("{R}/src/lib.rs"),
            ),
            (
                "{R}",
                "git rm -r -q 'sr*'",
                Effect::Deleted("{R}/src/lib.rs"),
            ),
        ],
        repo_cwd,
    );
}

#[test]
fn hole_partially_quoted_relative_rm_of_settings() {
    holes(
        &[
            (
                "{C}",
                "rm \"settings\".json",
                Effect::Deleted("{C}/settings.json"),
            ),
            (
                "{C}",
                "rm settings'.json'",
                Effect::Deleted("{C}/settings.json"),
            ),
            (
                "{C}",
                "rm $'settings.json'",
                Effect::Deleted("{C}/settings.json"),
            ),
            (
                "{C}",
                "rm \"settings.json\"''",
                Effect::Deleted("{C}/settings.json"),
            ),
        ],
        repo_cwd,
    );
}

#[test]
fn green_argv_boundary_spellings_are_not_allow() {
    // Parser boundaries the branch claims to handle, with real-git effect.
    holes(
        &[
            (
                "",
                "git init -q --end-of-options {K}/eo",
                Effect::Created("{K}/eo/.git/HEAD"),
            ),
            (
                "",
                "git init -q --no-template {K}/nt",
                Effect::Created("{K}/nt/.git/HEAD"),
            ),
            (
                "",
                "git -C {H} -C .claude/hooks init -q",
                Effect::Created("{K}/.git/HEAD"),
            ),
            (
                "",
                "git -c user.name=x init -q {K}/cv",
                Effect::Created("{K}/cv/.git/HEAD"),
            ),
            // NOT a hole (observed 2026-11-28): real git rejects the
            // abbreviation `format-patch --output-dir=` ("unrecognized argument").
            (
                "",
                "GIT_WORK_TREE={C} git checkout-index -f -- settings.json",
                Effect::SettingsChanged,
            ),
            (
                "",
                "env GIT_WORK_TREE={C} git checkout-index -a -f",
                Effect::Created("{K}/x.sh"),
            ),
        ],
        repo_cwd,
    );
}

// ================================================================ FP

fn expect_allow(fx: &Fx, cases: &[(PathBuf, String)]) {
    let bad: Vec<String> = cases
        .iter()
        .filter_map(|(cwd, c)| {
            let v = verdict(fx, cwd, c);
            (v != "allow").then(|| {
                format!(
                    "FP: `{c}` (cwd {}) => {v}",
                    cwd.strip_prefix(&fx.base).unwrap_or(cwd).display()
                )
            })
        })
        .collect();
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

#[test]
fn fp_ordinary_harness_operations_stay_allow() {
    let fx = Fx::new();
    let r = fx.repo();
    let w = fx.wt();
    let s = |t: &str| fx.sub(t);
    let src = fx.home().join("src");
    expect_allow(
        &fx,
        &[
            (
                r.clone(),
                s("git worktree add {H}/src/.harness-worktrees/n -b n main"),
            ),
            (r.clone(), s("git -C {W} merge -q --no-edit main")),
            (w.clone(), "git apply x.patch".into()),
            (w.clone(), "git apply --3way x.patch".into()),
            (w.clone(), "git rm -r --cached target".into()),
            (w.clone(), "git rm -r src".into()),
            (r.clone(), "git clone https://example.invalid/x.git".into()),
            (
                r.clone(),
                "git clone https://example.invalid/x.git dir".into(),
            ),
            (r.clone(), "git init".into()),
            (w.clone(), "git init".into()),
            (w.clone(), "git format-patch -1".into()),
            (r.clone(), "git format-patch -1".into()),
            (w.clone(), "git config user.name x".into()),
            (
                w.clone(),
                "git config --file .gitmodules submodule.x.path x".into(),
            ),
            (w.clone(), "mkdir -p target/x".into()),
            (w.clone(), "touch a".into()),
            (w.clone(), "mv build build2".into()),
            (src.clone(), "mv build-nonexistent b2".into()),
        ],
    );
}

#[test]
fn fp_mv_plain_dir_inside_worktree_store_pre_existing() {
    // `x` is a plain directory (no `.git`) inside the storage root. Observed
    // reason: "names a worktree directory that could not be placed". That
    // branch of `worktree_move_ask` is unchanged by fix-write-targets (only
    // the `Outside` arm was edited), so this is NOT shown to be a regression
    // of this branch — it is an FP on its own.
    let fx = Fx::new();
    let src = fx.home().join("src");
    expect_allow(
        &fx,
        &[(src, "mv .harness-worktrees/x .harness-worktrees/y".into())],
    );
}

#[test]
fn fp_git_rm_cached_in_main_tree_deletes_no_file() {
    // `--cached` only unstages: no file of the shared main tree is deleted.
    let fx = Fx::new();
    let r = fx.repo();
    expect_allow(
        &fx,
        &[
            (r.clone(), "git rm -r --cached target".into()),
            (r.clone(), "git rm -r -q --cached target".into()),
        ],
    );
}

#[test]
fn fp_git_rm_dry_run_in_main_tree_deletes_no_file() {
    let fx = Fx::new();
    let r = fx.repo();
    expect_allow(&fx, &[(r.clone(), "git rm -r -n src".into())]);
}
