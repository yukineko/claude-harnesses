#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 11, 2026-11-28, against 9e4cd1e34 / blastguard 0.2.1000):
//! reach paths of the askrules judge after the round-10 narrowing (Worktree =
//! strictly inside the cwd's own linked checkout, decided by `gitlink_kind`;
//! `-C` refused; `worktree` dropped from GIT_SUBCOMMANDS). Written by an
//! independent verifier, not the implementer (CLAUDE.md §2(a)). Black-box:
//! spawns the built binary with a sandbox HOME and a learned table whose ONLY
//! allow entry is the key `blastguard shape` prints for a benign sibling.
//!
//! Fixtures: the round-10 production layout (HOME is an ancestor of the main
//! tree `<home>/src/repo` and of its linked worktrees
//! `<home>/src/.harness-worktrees/{wt,wt2}`; the scratchpad is
//! `<root>/scratchpad`) in a fresh `tempfile` directory under
//! `/private/tmp/claude-askrules`.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! entry (the table is written by hand); that a session would create the
//! forged layout on its own; anything about shapes not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000e1";
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

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} in {}", dir.display());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) {
    git_out(dir, args);
}

fn env_prod(name: &str) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v11-{name}-"))
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
    let sp = root.join("scratchpad");
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

/// A real repository in the scratchpad (the benign sibling's cwd).
fn sp_repo(e: &Env) -> PathBuf {
    e.sp.join("r")
}

/// A scratchpad directory whose `.git` is a gitlink FILE naming the MAIN
/// tree's real git dir (no commondir, no back-link: git still uses it).
fn sp_fake(e: &Env) -> PathBuf {
    e.sp.join("fake")
}

/// A scratchpad directory whose `.git` is a SYMLINK to the main tree's git dir.
fn sp_link(e: &Env) -> PathBuf {
    e.sp.join("lnk")
}

fn fixtures(e: &Env) {
    let r = sp_repo(e);
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::write(r.join("src").join("a.rs"), "a").unwrap();
    git(&r, &["init", "-q"]);
    git(&r, &["add", "src"]);
    git(&r, &["commit", "-q", "-m", "i"]);
    std::fs::write(r.join("new.rs"), "n").unwrap();
    std::fs::create_dir_all(r.join("build")).unwrap();

    let f = sp_fake(e);
    std::fs::create_dir_all(&f).unwrap();
    std::fs::write(
        f.join(".git"),
        format!("gitdir: {}\n", e.main.join(".git").display()),
    )
    .unwrap();
    std::fs::write(f.join("junk.txt"), "j").unwrap();

    let l = sp_link(e);
    std::fs::create_dir_all(&l).unwrap();
    std::os::unix::fs::symlink(e.main.join(".git"), l.join(".git")).unwrap();
    std::fs::write(l.join("junk.txt"), "j").unwrap();

    // Another session's uncommitted, STAGED work in the main tree.
    std::fs::write(e.main.join("staged.rs"), "staged work").unwrap();
    git(&e.main, &["add", "staged.rs"]);

    std::fs::create_dir_all(e.wt.join("build")).unwrap();
    std::fs::create_dir_all(e.sp.join("build")).unwrap();

    // A scratchpad repository whose persisted `core.worktree` is the MAIN
    // tree, and another session's untracked work in the main tree.
    let cw = sp_cw(e);
    std::fs::create_dir_all(&cw).unwrap();
    git(&cw, &["init", "-q"]);
    git(&cw, &["config", "core.worktree", e.main.to_str().unwrap()]);
    std::fs::write(e.main.join("untracked_wip.rs"), "wip").unwrap();
}

/// A scratchpad repository whose `core.worktree` names the main tree.
fn sp_cw(e: &Env) -> PathBuf {
    e.sp.join("cw")
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
/// text (`None` when no ride).
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
            "probe `{}` (cwd {}) was {before} without a table but Allow once the sibling `{}` \
             (cwd {}) was learned (key {k}, sibling with it: {sib_own}); probe shape {}",
            cmd_of(probe),
            probe["cwd"].as_str().unwrap_or(""),
            cmd_of(sibling),
            sibling["cwd"].as_str().unwrap_or(""),
            shape(e, probe)
        )
    })
}

/// Measurement helper (asserts nothing). `<where> <cmd>`.
#[test]
#[ignore]
fn probe_dump() {
    let e = env_prod("probe");
    fixtures(&e);
    let probes = std::env::var("V11_PROBES").unwrap_or_else(|_| PROBES.to_string());
    for line in probes.lines().filter(|l| !l.trim().is_empty()) {
        let (where_, cmd) = line.split_once(' ').unwrap();
        let cmd = cmd
            .replace("$SP", e.sp.to_str().unwrap())
            .replace("$WT2", e.wt2.to_str().unwrap())
            .replace("$WT", e.wt.to_str().unwrap())
            .replace("$MAIN", e.main.to_str().unwrap())
            .replace("$HOMEDIR", e.home.to_str().unwrap());
        let (r, f, l, cw) = (sp_repo(&e), sp_fake(&e), sp_link(&e), sp_cw(&e));
        let cwd = match where_ {
            "C" => &cw,
            "M" => &e.main,
            "S" => &e.sp,
            "R" => &r,
            "F" => &f,
            "L" => &l,
            "O" => &e.wt2,
            _ => &e.wt,
        };
        let p = if where_ == "X" {
            json!({"hook_event_name": "PreToolUse", "session_id": SID, "cwd": e.sp,
                   "tool_name": "Write", "tool_input": {"file_path": cmd,
                   "content": format!("gitdir: {}\n", e.main.join(".git").display())}})
        } else {
            bash(cwd, &cmd)
        };
        let s = shape(&e, &p);
        let d = decide_with(&e, &no_table(&e), &p);
        let k = s["shape"].as_str().unwrap_or("").to_string();
        let own = decide_with(&e, &table_with(&e, &k), &p);
        println!("{where_} {cmd:<70} => {d:<5} own={own:<5} {k}");
    }
}

const PROBES: &str = r#"
R git config core.worktree $MAIN
W git config core.worktree $MAIN
C git clean -fdx
C git clean -fd
C git add -A
C git commit -a -m x
R git clean -fd
X $SP/new1/.git
X $SP/fake/.git
S mkdir -p $SP/n2
S ln -s $MAIN/.git $SP/n2/.git
S git init -q n3
M git worktree add $SP/w5
F git commit -am x
L git commit -a -m x
R git commit -am x
R git add -A
F git add -A
L git add -A
R git add .
F git add .
R git commit -a -m x
F git commit -a -m x
R git clean -fdx
F git clean -fdx
R git reset --hard
F git reset --hard
R git checkout .
F git checkout .
R git stash -u
F git stash -u
R git restore .
F git restore .
R git rm -r src
F git rm -r -q src
R rm -rf build
S rm -rf build
S rm -rf r
W git add -A
W git add .
W git commit -a -m x
W git stash -u
W git checkout .
R git config core.fsmonitor x
W git config core.fsmonitor x
"#;

// ------------------------------------------------ scratchpad repo class

/// Spec "Git writes take the class of the repository they act on" (third
/// round, rule 5) and fourth round rule 3 (`GIT_DIR` / `--git-dir` naming
/// `<repo>/.git` classes `<repo>`; `--work-tree` / `-c core.worktree=<abs>`
/// class the tree they name). `shape::class_of_repo` returns `Scratchpad` as
/// soon as the cwd's SPELLING and realpath are under a scratchpad:
///
/// ```text
/// if lexical == TargetClass::Scratchpad && physical == TargetClass::Scratchpad {
///     return if real_spelling { TargetClass::Scratchpad } else { .. };
/// }
/// ```
///
/// before `repo_kind_root` / `gitlink_kind` is consulted, and never looks at
/// the git dir or the work tree git will actually use. So a scratchpad
/// directory whose `.git` is a SYMLINK to the main tree's git dir (`lnk`), or
/// a gitlink FILE naming it (`fake`), or a scratchpad repository whose
/// persisted `core.worktree` is the main tree (`cw`), share the key of a
/// plain scratchpad repository (`r`). Observed at 9e4cd1e34 (probe_dump):
/// `git commit -a -m x` / `git add -A` in lnk/fake/cw are `ask`
/// `git-whole-tree-stage|s4-scratchpad|..`, `git clean -fdx` in cw is `ask`
/// `scoped-destructive-confined|s4-scratchpad|git clean -d -f -x`, each equal
/// to the key of the same command in `r`, and `allow` with it. The setup
/// needs no approval either: `ln -s <main>/.git <sp>/n2/.git` and `git config
/// core.worktree <main>` (cwd a scratchpad repo) were both detector `allow`.
/// Precondition (observed below with real git): the commit lands on the MAIN
/// tree's branch and drops its files; the clean deletes the main tree's
/// untracked work.
#[test]
fn r11_scratchpad_repo_class_ignores_the_git_dir_and_work_tree_git_uses() {
    // Precondition: the harm, on a throw-away copy of the layout.
    let h = env_prod("harm");
    fixtures(&h);
    let main_git = std::fs::canonicalize(h.main.join(".git")).unwrap();
    for d in [sp_link(&h), sp_fake(&h)] {
        let gd = git_out(&d, &["rev-parse", "--absolute-git-dir"]);
        assert_eq!(
            std::fs::canonicalize(gd.trim()).unwrap(),
            main_git,
            "precondition: git in {} uses the main tree's git dir",
            d.display()
        );
    }
    let head0 = git_out(&h.main, &["rev-parse", "HEAD"]);
    git(&sp_link(&h), &["commit", "-q", "-a", "-m", "x"]);
    let head1 = git_out(&h.main, &["rev-parse", "HEAD"]);
    assert_ne!(
        head0, head1,
        "precondition: `git commit -a` in the scratchpad moves the main tree's branch"
    );
    let files = git_out(&h.main, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        !files.contains("src/lib.rs"),
        "precondition: the commit drops the main tree's files: {files:?}"
    );
    assert!(h.main.join("untracked_wip.rs").exists());
    git(&sp_cw(&h), &["clean", "-q", "-fdx"]);
    assert!(
        !h.main.join("untracked_wip.rs").exists(),
        "precondition: `git clean -fdx` in the core.worktree scratchpad repo deletes the main \
         tree's untracked work"
    );

    let e = env_prod("sprepo");
    fixtures(&e);
    let r = sp_repo(&e);
    let mut bad = vec![];
    for (sib, probe, cwds) in [
        (
            "git commit -a -m x",
            "git commit -a -m x",
            vec![sp_link(&e), sp_fake(&e), sp_cw(&e)],
        ),
        (
            "git add -A",
            "git add -A",
            vec![sp_link(&e), sp_fake(&e), sp_cw(&e)],
        ),
        ("git clean -fdx", "git clean -fdx", vec![sp_cw(&e)]),
    ] {
        for c in cwds {
            if let Some(x) = ride(&e, &bash(&r, sib), &bash(&c, probe)) {
                bad.push(x);
            }
        }
    }
    assert!(
        bad.is_empty(),
        "a learned git write of a plain scratchpad repository is honoured in a scratchpad \
         directory whose git dir / work tree is the MAIN tree's:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ relative-paths worktree (FP)

/// Task item 3 (false positives) / spec Components 1 ("A linked worktree is
/// decided as git decides"). git >= 2.48 writes RELATIVE links for a worktree
/// made with `--relative-paths` (`extensions.relativeWorktrees`): the
/// `<gitdir>/gitdir` back-link is relative to the git dir. `gitlink_kind`
/// canonicalises it against the hook PROCESS cwd
/// (`std::fs::canonicalize(back.trim())`), not against the git dir, so the
/// back-link never matches and a real linked worktree is `Unknown`. Expected:
/// `git add -A` at that worktree's root is Allow with its own learned entry,
/// like the absolute-link worktree (`r10_guard_own_key_allows_stay`).
#[test]
fn r11_fp_relative_paths_worktree_is_a_linked_worktree() {
    let e = env_prod("relwt");
    fixtures(&e);
    let rel = e.home.join("src").join(".harness-worktrees").join("rel");
    git(
        &e.main,
        &[
            "worktree",
            "add",
            "-q",
            "--relative-paths",
            rel.to_str().unwrap(),
        ],
    );
    let back = std::fs::read_to_string(
        e.main
            .join(".git")
            .join("worktrees")
            .join("rel")
            .join("gitdir"),
    )
    .unwrap();
    assert!(
        !back.trim().starts_with('/'),
        "precondition: git wrote a relative back-link: {back:?}"
    );
    std::fs::write(rel.join("new.rs"), "n").unwrap();
    let p = bash(&rel, "git add -A");
    let s = shape(&e, &p);
    assert_eq!(s["decision"], "ask", "precondition: `git add -A` Asks: {s}");
    let k = s["shape"].as_str().unwrap();
    assert_eq!(
        decide_with(&e, &table_with(&e, k), &p),
        "allow",
        "`git add -A` at the root of a --relative-paths linked worktree with its own allow \
         entry {k} (the absolute-link worktree is Allow with git-whole-tree-stage|s4-worktree|git \
         add -A)"
    );
}

// ------------------------------------------------ detector side

/// Detector side (askrules is not involved: no table). `git config
/// core.worktree <main tree>` persists a redirection of every later git
/// write's work tree onto the main tree (the precondition of
/// `r11_scratchpad_repo_class_ignores_the_git_dir_and_work_tree_git_uses`),
/// like `core.hooksPath` (`git-config-hookspath`, Deny) and the transient
/// `-c core.worktree=` which the shape already classes by the tree it names.
/// Observed at 9e4cd1e34: `allow` with cwd a scratchpad repository and with
/// cwd this session's own linked worktree (where it writes the SHARED config
/// in the main tree's git dir). Expectation (verifier's analogy to
/// core.hooksPath, confirm when fixing): not Allow.
#[test]
#[ignore = "detector, filed separately"]
fn r11_detector_git_config_core_worktree_is_allow() {
    let e = env_prod("cfgwt");
    fixtures(&e);
    let m = e.main.display().to_string();
    let mut bad = vec![];
    for cwd in [sp_repo(&e), e.wt.clone()] {
        let p = bash(&cwd, &format!("git config core.worktree {m}"));
        let d = decide_with(&e, &no_table(&e), &p);
        if d == "allow" {
            bad.push(format!("cwd {} `{}` => {d}", cwd.display(), cmd_of(&p)));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ guards (must stay green)

/// Own-key Allows stay (task item 3), and the siblings of the r11 ride test
/// are honoured with their own entry (so its rides are not vacuous). Also:
/// the spellings that name another git dir / work tree on the line itself
/// (`GIT_DIR=`, `--git-dir`, `--work-tree`, `-c core.worktree=`, `env
/// GIT_DIR=`) do not ride the plain scratchpad repository's key.
#[test]
fn r11_guard_own_keys_and_line_level_redirections() {
    let e = env_prod("guard");
    fixtures(&e);
    let r = sp_repo(&e);
    // (cwd, command, expected Allow with its own entry)
    for (cwd, c, want_allow) in [
        (e.wt.clone(), "git add -A", true),
        (e.sp.clone(), "rm -rf build", true),
        // re-anchored 2026-11-28 (leader ruling): scratchpad git removed from learned Allow
        // Before: Allow with its own entry. After: NOT Allow (scope cut: a
        // git command in a scratchpad repository is never Allow-learnable).
        (r.clone(), "git add -A", false),
        // re-anchored 2026-11-28 (leader ruling): scratchpad git removed from learned Allow
        (r.clone(), "git commit -a -m x", false),
        // re-anchored 2026-11-28 (leader ruling): scratchpad git removed from learned Allow
        (r.clone(), "git clean -fdx", false),
    ] {
        let p = bash(&cwd, c);
        let s = shape(&e, &p);
        assert_eq!(s["decision"], "ask", "precondition: `{c}` Asks: {s}");
        let k = s["shape"].as_str().unwrap();
        assert_eq!(
            decide_with(&e, &table_with(&e, k), &p) == "allow",
            want_allow,
            "`{c}` in {} with its own allow entry {k} (want Allow: {want_allow})",
            cwd.display()
        );
    }
    let m = e.main.display().to_string();
    let mut bad = vec![];
    // Round-10 property pinned on the CLASS (the r10 ride test now passes by
    // the `-C` refusal alone, whatever another worktree is classed): with
    // cwd = this session's worktree, another session's worktree is never an
    // Allow-eligible class.
    let w2 = e.wt2.display().to_string();
    for c in [
        format!("rm -rf {w2}/build"),
        format!("rm -rf {w2}/src"),
        format!("git -C {w2} add -A"),
        format!("git -C {w2} clean -fdx"),
    ] {
        let k = shape(&e, &bash(&e.wt, &c))["shape"]
            .as_str()
            .unwrap()
            .to_string();
        let class = k.split('|').nth(1).unwrap_or("");
        if ["s4-none", "s4-worktree", "s4-scratchpad"].contains(&class) {
            bad.push(format!("`{c}` (cwd wt) => {k}"));
        }
    }
    for (sib, probe) in [
        (
            "git add -A".to_string(),
            format!("GIT_DIR={m}/.git git add -A"),
        ),
        (
            "git add -A".to_string(),
            format!("env GIT_DIR={m}/.git git add -A"),
        ),
        (
            "git add -A".to_string(),
            format!("git --git-dir={m}/.git add -A"),
        ),
        (
            "git clean -fdx".to_string(),
            format!("git --work-tree={m} clean -fdx"),
        ),
        (
            "git clean -fdx".to_string(),
            format!("git -c core.worktree={m} clean -fdx"),
        ),
    ] {
        // re-anchored 2026-11-28 (leader ruling): scratchpad git removed from learned Allow
        // The scratchpad sibling's key is no longer Allow-eligible, so the
        // ride from `r` alone became vacuous; the same line-level
        // redirections are also ridden from the session's own linked
        // worktree, where the sibling key IS Allow-eligible.
        for cwd in [r.clone(), e.wt.clone()] {
            let probe = bash(&cwd, &probe);
            if decide_with(&e, &no_table(&e), &probe) == "allow" {
                continue; // detector side, not a ride
            }
            let sibling = bash(&cwd, &sib);
            if shape(&e, &sibling)["decision"] != "ask" {
                // `git clean -fdx` in the own worktree is a detector Allow
                // (no key is learned for it): no sibling to ride.
                continue;
            }
            if let Some(x) = ride(&e, &sibling, &probe) {
                bad.push(x);
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
