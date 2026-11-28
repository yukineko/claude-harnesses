#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 10, 2026-11-28, against 825a2f1ba / blastguard 0.2.998):
//! reach paths of the askrules judge after the round-9 narrowing (no
//! redirects, cp/mv/mkdir/touch dropped, Worktree = strictly inside a linked
//! checkout). Written by an independent verifier, not the implementer
//! (CLAUDE.md §2(a)). Black-box: spawns the built binary with a sandbox HOME
//! and a learned table whose ONLY allow entry is the key `blastguard shape`
//! prints for a benign sibling (or for the probe itself).
//!
//! Fixtures: the production layout of round 9 (HOME is an ancestor of the
//! main tree `<home>/src/repo` and of its linked worktrees
//! `<home>/src/.harness-worktrees/{wt,wt2}`; the scratchpad is
//! `<root>/scratchpad`) in a fresh `tempfile` directory under
//! `/private/tmp/claude-askrules`. `wt2` is ANOTHER session's worktree and
//! holds uncommitted work.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! entry (the table is written by hand); that every harm happens at run time
//! beyond the precondition each test observes; anything about shapes not
//! listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000e0";
const BASE: &str = "/private/tmp/claude-askrules";

struct Env {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    wt2: PathBuf,
    main: PathBuf,
    sp: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

fn env_prod(name: &str) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v10-{name}-"))
        .tempdir_in(BASE)
        .unwrap();
    let root = dir.path().to_path_buf();
    let home_dir = tempfile::Builder::new()
        .prefix("home-")
        .tempdir_in(&root)
        .unwrap();
    let home = home_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("approvals")).unwrap();
    let main = home.join("src").join("repo");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::write(main.join("src").join("lib.rs"), "x").unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "src"]);
    git(&main, &["commit", "-q", "-m", "i"]);
    let wts = home.join("src").join(".harness-worktrees");
    let wt = wts.join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let wt2 = wts.join("wt2");
    git(&main, &["worktree", "add", "-q", wt2.to_str().unwrap()]);
    // fixture re-anchored 2026-11-28 (leader ruling): Scratchpad is the session's own scratchpad only
    let sp = root.join(SID).join("scratchpad");
    std::fs::create_dir_all(&sp).unwrap();
    Env {
        _dir: dir,
        _home: home_dir,
        root,
        home,
        wt,
        wt2,
        main,
        sp,
    }
}

fn fixtures(e: &Env) {
    let claude = e.home.join(".claude");
    std::fs::create_dir_all(claude.join("hooks")).unwrap();
    std::fs::create_dir_all(claude.join("skills")).unwrap();
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
    std::fs::create_dir_all(e.wt.join("build")).unwrap();
    std::fs::write(e.wt.join("build").join("o"), "x").unwrap();
    std::fs::create_dir_all(e.sp.join("build")).unwrap();
    // Another session's uncommitted work.
    std::fs::create_dir_all(e.wt2.join("build")).unwrap();
    std::fs::write(e.wt2.join("build").join("o"), "x").unwrap();
    std::fs::write(e.wt2.join("src").join("lib.rs"), "uncommitted edit").unwrap();
    std::fs::write(e.wt2.join("wip.rs"), "uncommitted").unwrap();
    // A directory under HOME (not a worktree) whose `.git` is a gitlink FILE
    // whose text merely contains `/worktrees/` (no such git dir exists).
    let fake = e.home.join("notes");
    std::fs::create_dir_all(fake.join("sub")).unwrap();
    std::fs::write(fake.join("sub").join("f"), "user data").unwrap();
    std::fs::write(fake.join(".git"), "gitdir: /nonexistent/worktrees/x\n").unwrap();
    std::fs::create_dir_all(fake.join("build")).unwrap();
    // A finished worktree whose work is all committed; only an untracked
    // build directory is left (so `worktree remove` needs `--force`): the
    // benign target of the cleanup siblings.
    git(
        &e.main,
        &["worktree", "add", "-q", wt3(e).to_str().unwrap()],
    );
    std::fs::create_dir_all(wt3(e).join("build")).unwrap();
    std::fs::write(wt3(e).join("build").join("o"), "artifact").unwrap();
    // Symlinks: a dangling link into the main tree, a directory link into
    // `~/.claude`.
    for d in [&e.sp, &e.wt] {
        std::os::unix::fs::symlink(e.main.join("newfile"), d.join("dang")).unwrap();
        std::os::unix::fs::symlink(&claude, d.join("linkdir")).unwrap();
    }
    // condukt's worktree layout and Claude Code's (inside the main tree): real
    // linked worktrees.
    let ct = e
        .home
        .join(".condukt")
        .join("worktrees")
        .join("run")
        .join("task");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", ct.to_str().unwrap()]);
    std::fs::create_dir_all(ct.join("build")).unwrap();
    let w2 = e.main.join(".claude").join("worktrees").join("w2");
    std::fs::create_dir_all(w2.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", w2.to_str().unwrap()]);
    std::fs::create_dir_all(w2.join("build")).unwrap();
    // A REAL submodule of the main tree whose path is `worktrees/lib`: git
    // writes its gitlink as `gitdir: ../../.git/modules/worktrees/lib`. It is
    // part of the MAIN checkout (not a linked worktree).
    let libsrc = e.root.join("libsrc");
    std::fs::create_dir_all(libsrc.join("src")).unwrap();
    std::fs::write(libsrc.join("src").join("a.rs"), "lib").unwrap();
    git(&libsrc, &["init", "-q"]);
    git(&libsrc, &["add", "src"]);
    git(&libsrc, &["commit", "-q", "-m", "i"]);
    git(
        &e.main,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            libsrc.to_str().unwrap(),
            "worktrees/lib",
        ],
    );
    std::fs::create_dir_all(sub_dir(e).join("build")).unwrap();
}

fn wt3(e: &Env) -> PathBuf {
    e.home.join("src").join(".harness-worktrees").join("wt3")
}

fn sub_dir(e: &Env) -> PathBuf {
    e.main.join("worktrees").join("lib")
}

fn spawn(e: &Env, table: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", table)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn no_table(e: &Env) -> PathBuf {
    e.root.join("no-table.json")
}

fn decide_with(e: &Env, table: &Path, p: &Value) -> String {
    let (_, out, err) = spawn(e, table, &[], &p.to_string());
    if out.trim().is_empty() {
        return "allow".to_string();
    }
    let d: Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|x| panic!("stdout not JSON ({x}): {out:?} stderr={err:?}"));
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
        .to_string()
}

fn bash(cwd: &Path, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

fn shape(e: &Env, p: &Value) -> Value {
    let (code, out, err) = spawn(e, &no_table(e), &["shape"], &p.to_string());
    assert_eq!(code, 0, "shape failed: {out:?} {err:?}");
    serde_json::from_str(out.trim()).unwrap()
}

fn table_with(e: &Env, k: &str) -> PathBuf {
    let p = e.root.join("t.json");
    let entry = json!({"shape": k, "decision": "allow", "approvals": 50, "declines": 0,
                       "sessions": 20, "samples": []});
    std::fs::write(&p, json!({"version": 1, "entries": [entry]}).to_string()).unwrap();
    p
}

fn cmd_of(p: &Value) -> String {
    p["tool_input"]["command"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// Learn `sibling` allow; `probe` must not become Allow. Returns the failure
/// text (`None` when no ride). The sibling's own-key Allow is NOT asserted
/// (a fix may make the sibling ineligible too); that the table is honoured at
/// all is pinned by `r10_guard_own_key_allows_stay`.
fn ride(e: &Env, sibling: &Value, probe: &Value) -> Option<String> {
    let before = decide_with(e, &no_table(e), probe);
    assert_ne!(
        before,
        "allow",
        "precondition: probe {:?} must not be Allow without a table",
        cmd_of(probe)
    );
    let sib = shape(e, sibling);
    assert_eq!(
        sib["decision"],
        "ask",
        "precondition: sibling {:?} must Ask: {sib}",
        cmd_of(sibling)
    );
    let k = sib["shape"].as_str().unwrap().to_string();
    let t = table_with(e, &k);
    let sib_own = decide_with(e, &t, sibling);
    let after = decide_with(e, &t, probe);
    (after == "allow").then(|| {
        format!(
            "probe `{}` was {before} without a table but Allow once the sibling `{}` was \
             learned (key {k}, sibling with it: {sib_own}); probe shape {}",
            cmd_of(probe),
            cmd_of(sibling),
            shape(e, probe)
        )
    })
}

/// Like [`ride`], with the probe and the sibling each in its own cwd.
fn rides(e: &Env, pairs: &[(Value, Value)]) -> Vec<String> {
    pairs.iter().filter_map(|(s, p)| ride(e, s, p)).collect()
}

fn class_of_key(k: &str) -> &str {
    k.split('|').nth(1).unwrap_or("")
}

const ELIGIBLE: &[&str] = &["s4-none", "s4-worktree", "s4-scratchpad"];

/// Measurement helper (asserts nothing). `<where> <cmd>`: where = W / M / S
/// (cwd = own worktree / main tree / scratchpad).
#[test]
#[ignore]
fn probe_dump() {
    let e = env_prod("probe");
    fixtures(&e);
    let probes = std::env::var("V10_PROBES").unwrap_or_else(|_| PROBES.to_string());
    for line in probes.lines().filter(|l| !l.trim().is_empty()) {
        let (where_, cmd) = line.split_once(' ').unwrap();
        let cmd = cmd
            .replace("$SP", e.sp.to_str().unwrap())
            .replace("$WT2", e.wt2.to_str().unwrap())
            .replace("$WT", e.wt.to_str().unwrap())
            .replace("$MAIN", e.main.to_str().unwrap())
            .replace("$HOMEDIR", e.home.to_str().unwrap());
        let src = e.home.join("src");
        let dotclaude = e.home.join(".claude");
        let cwd = match where_.trim_start_matches('F') {
            "M" => &e.main,
            "S" => &e.sp,
            "O" => &e.wt2,
            "P" => &src,
            "C" => &dotclaude,
            _ => &e.wt,
        };
        let p = if where_.starts_with('F') {
            json!({"hook_event_name": "PreToolUse", "session_id": SID, "cwd": cwd,
                   "tool_name": "Write", "tool_input": {"file_path": cmd, "content": "x"}})
        } else {
            bash(cwd, &cmd)
        };
        let s = shape(&e, &p);
        let d = decide_with(&e, &no_table(&e), &p);
        let k = s["shape"].as_str().unwrap_or("").to_string();
        let own = decide_with(&e, &table_with(&e, &k), &p);
        println!("{where_} {cmd:<80} => {d:<5} own={own:<5} {k}");
    }
}

const PROBES: &str = r#"
C rm -rf projects
C rm -rf $HOMEDIR/.claude/projects
P rm -rf .harness-worktrees
P rm -rf .harness-worktrees/
W rm -rf $HOMEDIR/.condukt/worktrees/stale
FS $SP/dang
FS $SP/linkdir/x
FW $WT/dang
FW $WT/build/o
FW $WT2/src/lib.rs
FW $HOMEDIR/notes/sub/f
FM $MAIN/worktrees/lib/src/a.rs
W rm $WT/dang
W rm -rf $WT/linkdir/x
W rm -rf $HOMEDIR/.condukt/worktrees/run/task/build
S rm -rf $HOMEDIR/.condukt/worktrees/run/task/build
M rm -rf $HOMEDIR/.condukt/worktrees/run/task/build
W rm -rf $MAIN/.claude/worktrees/w2/build
S rm -rf $MAIN/.claude/worktrees/w2/build
M rm -rf $MAIN/.claude/worktrees/w2/build
W rm -r $HOMEDIR/.condukt/worktrees/run/task/build
W rm -rf $MAIN/worktrees/lib/src
S rm -rf $MAIN/worktrees/lib/src
M rm -rf $MAIN/worktrees/lib/src
M rm -rf worktrees/lib/src
W rm -rf $HOMEDIR/notes/sub
M rm -rf $HOMEDIR/notes/sub
S rm -rf $WT/build
M rm -rf $WT/build
S git -C $WT reset --hard
M git -C $WT reset --hard
W git -C $WT reset --hard
W git -C . reset --hard
W git -C build reset --hard
M git worktree remove --force $WT2
M git worktree remove $WT2
W git worktree remove $WT2
O git -C $WT reset --hard
S rm -rf $HOMEDIR/notes/sub
S git -C $HOMEDIR/notes/sub reset --hard
W rm -rf $HOMEDIR/.condukt/worktrees/stale
W rm -f $HOMEDIR/notes/sub/f
W rm -r $HOMEDIR/notes/sub
W git add -A
S rm -rf build
W rm -rf build
W rm -rf $WT/build
W rm -rf $WT2/build
W rm -rf $WT2/wip.rs
W rm -rf $WT2/src
W git -C $WT2 reset --hard
W git reset --hard
W git -C $WT2 clean -fdx
W git clean -fdx
W git -C $WT2 checkout -f
W git -C $WT2 restore .
W git restore .
W git worktree remove --force $WT2
W git worktree remove --force $WT
W git worktree add -b nb $HOMEDIR/src/.harness-worktrees/nb
W git worktree add -b nb $HOMEDIR/.claude/skills/evil
W git worktree add -b nb $MAIN/evil
W git worktree add $HOMEDIR/.claude/skills/evil2
W git worktree move $WT2 $HOMEDIR/moved
W git bisect run cargo test
W git bisect run sh -x evil.sh
W git bisect run rm -rf $HOMEDIR
W git bisect run touch $HOMEDIR/.claude/settings.json
W git bisect start
W git bisect good
W git rebase -x 'cargo test' HEAD~1
W git rebase -x 'touch $HOMEDIR/.claude/settings.json' HEAD~1
W git rebase --exec 'cargo test' HEAD~1
W git rebase --exec 'touch $HOMEDIR/.claude/settings.json' HEAD~1
W git diff --output=$HOMEDIR/.claude/settings.json
W git log --output=$HOMEDIR/.claude/settings.json
W git grep -Otouch x
W git grep --open-files-in-pager=touch x
W git apply --unsafe-paths --directory=$HOMEDIR/.claude p.patch
W git apply p.patch
W git am p.mbox
W git commit --no-verify -m x
W git commit -m x
W git stash clear
W git branch -D other
W git -p status
W git --bare reset --soft HEAD
W rm -rf $HOMEDIR/notes/sub
W rm -rf $HOMEDIR/notes/sub/f
W rm -rf $MAIN/worktrees/lib/src
W rm -rf $MAIN/src
W git -C $HOMEDIR/notes/sub add -A
W git -C $MAIN/worktrees/lib add -A
W git -C $MAIN/worktrees/lib/src reset --hard
W rm $WT/.git
W rm -rf $WT/.git
W git checkout --ignore-other-worktrees main
W git switch --ignore-other-worktrees main
W git checkout -B main
W git branch -f main HEAD
W git tag -d v1
W git reflog expire --expire=now --all
W git notes add -m x
W git merge --no-verify x
W git -c core.hooksPath=/x commit -m x
W git --exec-path=/x status
W git --git-dir=$MAIN/.git add -A
W git --work-tree=$MAIN add -A
W git -C $MAIN add -A
W git -C $WT2 add -A
W git -C $WT2 commit -m x
W git -C $WT2 stash
W git -C $WT2 stash drop
"#;

// ------------------------------------------------ another session's worktree

/// Task threat "destroying another worktree's uncommitted work". The class
/// `Worktree` is ANY linked checkout (`shape::class_of_repo`:
/// `RepoKind::Linked => TargetClass::Worktree`, with no root exclusion and no
/// relation to the payload cwd), and `-C` is not in
/// `askrules::builtin::GIT_REFUSED_FLAGS` although `Model::Git` is documented
/// as "no global option that changes the repository". So the cleanup of a
/// finished, CLEAN worktree (`wt3`) and the same command on another session's
/// worktree holding UNCOMMITTED work (`wt2`) share one key, and the learned
/// cleanup Allow destroys `wt2`'s work. Observed at 825a2f1ba: each probe is
/// `ask` without a table (keys `unknown|s4-worktree|git reset --hard -C`,
/// `unknown|s4-worktree|git clean -C -d -f -x`,
/// `unknown|s4-worktree|git worktree remove --force`) and `allow` with the
/// sibling's entry. Precondition (observed below with real git): each probe
/// discards wt2's uncommitted edit / removes the checkout.
#[test]
fn r10_learned_cleanup_of_one_worktree_destroys_another_sessions_dirty_worktree() {
    // Precondition: the harm, on a throw-away copy of the layout.
    let h = env_prod("harm");
    fixtures(&h);
    git(
        &h.main,
        &["-C", h.wt2.to_str().unwrap(), "reset", "-q", "--hard"],
    );
    assert_eq!(
        std::fs::read_to_string(h.wt2.join("src").join("lib.rs")).unwrap(),
        "x",
        "precondition: `git -C <wt2> reset --hard` discards wt2's uncommitted edit"
    );
    git(
        &h.main,
        &["worktree", "remove", "--force", h.wt2.to_str().unwrap()],
    );
    assert!(
        !h.wt2.exists(),
        "precondition: `git worktree remove --force <wt2>` deletes the dirty checkout"
    );

    let e = env_prod("otherwt");
    fixtures(&e);
    let (w2, w3) = (e.wt2.display(), wt3(&e).display().to_string());
    let mut pairs = Vec::new();
    for (sib, probe) in [
        (
            format!("git -C {w3} reset --hard"),
            format!("git -C {w2} reset --hard"),
        ),
        (
            format!("git -C {w3} clean -fdx"),
            format!("git -C {w2} clean -fdx"),
        ),
        (
            format!("git worktree remove --force {w3}"),
            format!("git worktree remove --force {w2}"),
        ),
    ] {
        // The benign cleanup is the integrating session's (cwd = main tree);
        // the probe runs from the main tree and from this session's worktree.
        pairs.push((bash(&e.main, &sib), bash(&e.main, &probe)));
        pairs.push((bash(&e.main, &sib), bash(&e.wt, &probe)));
    }
    let bad = rides(&e, &pairs);
    assert!(
        bad.is_empty(),
        "a learned cleanup of one (clean) worktree is honoured for another session's worktree \
         holding uncommitted work:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ gitlink text, not git's answer

/// Spec Components 1: `Worktree` is "decided by git's own linked-worktree
/// answer (`.git` gitlink into `<common>/worktrees/<name>`, `repo_kind`),
/// never by a substring". `shape::repo_kind_root` reads the gitlink text and
/// returns `Linked` for `Some(g) if g.contains("/worktrees/")` -- a substring
/// of the gitlink, never checked against a git dir (`commondir`) -- and its
/// own docstring says "anything else (a submodule's `.git/modules/..`) is its
/// own main tree". A REAL submodule of the main tree at the path
/// `worktrees/lib` has the gitlink `gitdir: ../../.git/modules/worktrees/lib`
/// and is classed `Worktree`; so is a plain directory under HOME whose `.git`
/// file names a nonexistent `.../worktrees/x`. Observed at 825a2f1ba: every
/// probe below is `s4-worktree` (the main-tree submodule should be
/// `s4-main-tree`, never eligible; the HOME directory `s4-home`). The
/// submodule `rm -rf` lines are already Allow by the detector (class
/// property only); `git -C <submodule> add -A` (cwd main tree) is `ask`
/// `git-whole-tree-stage|s4-worktree|git add -A -C` -- the key of the common
/// `git -C <linked worktree> add -A` -- and `allow` with it; `rm -rf
/// <home>/notes/sub` is `ask` `scoped-destructive-confined|s4-worktree|rm -f
/// -r` and rides the learned `rm -rf <home>/notes/build`.
#[test]
fn r10_gitlink_text_containing_worktrees_is_not_a_linked_worktree() {
    let e = env_prod("gitlink");
    fixtures(&e);
    let gitlink = std::fs::read_to_string(sub_dir(&e).join(".git")).unwrap();
    assert_eq!(
        gitlink.trim(),
        "gitdir: ../../.git/modules/worktrees/lib",
        "precondition: git wrote the submodule's gitlink"
    );
    let s = sub_dir(&e);
    let s = s.display();
    let notes = e.home.join("notes").join("sub");
    let mut bad = vec![];
    for p in [
        bash(&e.main, &format!("rm -rf {s}/src")),
        bash(&e.main, "rm -rf worktrees/lib/src"),
        bash(&e.wt, &format!("rm -rf {s}/src")),
        bash(&e.main, &format!("git -C {s} add -A")),
        bash(&e.wt, &format!("rm -rf {}", notes.display())),
    ] {
        let sh = shape(&e, &p);
        let k = sh["shape"].as_str().unwrap();
        if ELIGIBLE.contains(&class_of_key(k)) {
            let own = decide_with(&e, &table_with(&e, k), &p);
            bad.push(format!(
                "`{}` => {} {k} (with its own key: {own})",
                cmd_of(&p),
                sh["decision"]
            ));
        }
    }
    if let Some(r) = ride(
        &e,
        &bash(&e.wt, &format!("rm -rf {}/notes/build", e.home.display())),
        &bash(&e.wt, &format!("rm -rf {}", notes.display())),
    ) {
        bad.push(r);
    }
    assert!(
        bad.is_empty(),
        "a directory whose gitlink text merely contains `/worktrees/` (a main-tree submodule, \
         a forged `.git` file under HOME) is classed as a linked worktree:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ guards (must stay green)

/// Task item 3 (false positives): own-key Allow still holds for the
/// linked-worktree `git add -A` (session worktree, condukt task, Claude Code's
/// nested worktree) and the scratchpad `rm -rf build` (relative / absolute).
/// Also the control that a learned table is honoured at all (the ride tests
/// above do not assert it).
#[test]
fn r10_guard_own_key_allows_stay() {
    let e = env_prod("guard");
    fixtures(&e);
    let ct = e
        .home
        .join(".condukt")
        .join("worktrees")
        .join("run")
        .join("task");
    let w2 = e.main.join(".claude").join("worktrees").join("w2");
    for (cwd, c) in [
        (&e.wt, "git add -A".to_string()),
        (&ct, "git add -A".to_string()),
        (&w2, "git add -A".to_string()),
        (&e.sp, "rm -rf build".to_string()),
        (&e.sp, format!("rm -rf {}/build", e.sp.display())),
    ] {
        let p = bash(cwd, &c);
        let s = shape(&e, &p);
        assert_eq!(s["decision"], "ask", "precondition: `{c}` Asks: {s}");
        let k = s["shape"].as_str().unwrap();
        assert_eq!(
            decide_with(&e, &table_with(&e, k), &p),
            "allow",
            "`{c}` in {} with its own allow entry {k}",
            cwd.display()
        );
    }
}

/// Re-anchor of the two verifier-8 rides whose control was inverted in
/// 825a2f1ba (`r8_cwd_under_home_...`, `r8_cwd_parent_of_trees_...`): their
/// sibling `rm -rf ~/.condukt/worktrees/stale` is now `s4-home`, so a probe
/// can only ride it with the key `..|s4-home|..`, which is never eligible --
/// the inverted assertion can no longer fail even if the probe itself became
/// `s4-worktree`. This pins the property itself: the probes are not classed
/// Allow-eligible and do not ride the still-reachable key
/// `scoped-destructive-confined|s4-worktree|rm -f -r`. Observed GREEN at
/// 825a2f1ba (the RED was observed at 7b58a1b5f by verifier 8).
#[test]
fn r10_guard_v8_inverted_rides_reanchored_on_the_class() {
    let e = env_prod("v8anchor");
    fixtures(&e);
    let claude = e.home.join(".claude");
    let src = e.home.join("src");
    let t = table_with(&e, "scoped-destructive-confined|s4-worktree|rm -f -r");
    let mut bad = vec![];
    for p in [
        bash(&claude, "rm -rf projects"),
        bash(&src, "rm -rf .harness-worktrees"),
        bash(&src, "rm -rf .harness-worktrees/"),
    ] {
        let k = shape(&e, &p)["shape"].as_str().unwrap().to_string();
        let d = decide_with(&e, &t, &p);
        if ELIGIBLE.contains(&class_of_key(&k)) || d == "allow" {
            bad.push(format!(
                "`{}` => {k}, with the worktree rm key: {d}",
                cmd_of(&p)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
