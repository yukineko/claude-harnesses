#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `cd D && CMD` must be judged no stricter than CMD with its relative operands
//! written as `D/<operand>`, when D is a literal absolute existing directory
//! (no `.`/`..`, no expansion) joined to CMD only by `&&`. Every RED case
//! (`r*`) is paired with a CONTROL (`c*`) running the absolute spelling in the
//! same fixture and asserting Allow, so a RED is attributable to the `cd`
//! spelling alone. `g*` cases are fail-closed guards: undeterminable or
//! non-tying input must never be Allow.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
enum V {
    Allow,
    Ask,
    Deny,
}

fn git(dir: &Path, args: &[&str]) {
    let out = git_env::pinned(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    hw: PathBuf,
    wt: PathBuf,
    outside: PathBuf,
    session: String,
}

impl Fx {
    /// `<base>/src/proj` is a git repo (tracked README.md, real `target/`),
    /// `<base>/src/.harness-worktrees/s` a linked worktree of it (with a real
    /// `target/`), `<base>/src/outside` a plain non-repo dir with `target/`.
    fn new(name: &str) -> Fx {
        // Per-run unique (pid + nonce) so overlapping runs never share a fixture.
        let nonce = SEQ.fetch_add(1, Ordering::SeqCst);
        let base = neutral_base::neutral_base()
            .join("cd_and_chain_equivalence")
            .join(format!("{name}-{}-{nonce}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let parent = base.join("src");
        let proj = parent.join("proj");
        let hw = parent.join(".harness-worktrees");
        let outside = parent.join("outside");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&hw).unwrap();
        std::fs::create_dir_all(outside.join("target")).unwrap();
        git(&proj, &["init", "-q"]);
        std::fs::write(proj.join("README.md"), "x\n").unwrap();
        git(&proj, &["add", "README.md"]);
        git(&proj, &["commit", "-q", "-m", "init"]);
        let wt = hw.join("s");
        git(
            &proj,
            &["worktree", "add", "-q", "-b", "wt-s", wt.to_str().unwrap()],
        );
        std::fs::create_dir_all(proj.join("target")).unwrap();
        std::fs::create_dir_all(wt.join("target")).unwrap();
        let session = format!(
            "cdeq-{name}-{}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        Fx {
            base,
            home,
            proj,
            hw,
            wt,
            outside,
            session,
        }
    }

    fn run(&self, cwd: &Path, command: &str) -> V {
        let payload = serde_json::json!({
            "session_id": self.session,
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", cwd)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        println!("[{command}] cwd={} => {:?}", cwd.display(), s.trim());
        classify(&s)
    }
}

fn classify(s: &str) -> V {
    if s.trim().is_empty() {
        return V::Allow;
    }
    let v: serde_json::Value = serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("unparseable hook output {s:?}: {e}"));
    match v["hookSpecificOutput"]["permissionDecision"].as_str() {
        Some("allow") => V::Allow,
        Some("ask") => V::Ask,
        Some("deny") => V::Deny,
        other => panic!("unknown permissionDecision {other:?} in {s}"),
    }
}

#[track_caller]
fn want(case: &str, got: V, expected: V) {
    assert_eq!(got, expected, "{case}: expected {expected:?}, got {got:?}");
}

#[track_caller]
fn not_allow(case: &str, got: V) {
    assert_ne!(got, V::Allow, "{case}: must NOT be Allow");
}

// ---- R1: linked worktree ------------------------------------------------------

#[test]
fn c1_worktree_absolute_forms_allow() {
    let f = Fx::new("c1");
    want(
        "abs redirect",
        f.run(&f.proj, &format!("echo a > {}/README.md", f.wt.display())),
        V::Allow,
    );
    want(
        "abs rm target",
        f.run(&f.proj, &format!("rm -rf {}/target", f.wt.display())),
        V::Allow,
    );
}

#[test]
fn r1a_cd_worktree_then_redirect_is_allow() {
    let f = Fx::new("r1a");
    let got = f.run(
        &f.proj,
        &format!("cd {} && echo a > README.md", f.wt.display()),
    );
    want("cd <wt> && echo a > README.md", got, V::Allow);
}

#[test]
fn r1b_cd_worktree_then_rm_target_is_allow() {
    let f = Fx::new("r1b");
    let got = f.run(&f.proj, &format!("cd {} && rm -rf target", f.wt.display()));
    want("cd <wt> && rm -rf target", got, V::Allow);
}

// ---- R2: /private/tmp ---------------------------------------------------------

struct TmpFx {
    dir: PathBuf,
}
impl TmpFx {
    fn new(tag: &str) -> TmpFx {
        let dir = PathBuf::from(format!(
            "/private/tmp/cdeq-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        TmpFx { dir }
    }
}
impl Drop for TmpFx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn c2_tmp_absolute_form_allow() {
    let f = Fx::new("c2");
    let t = TmpFx::new("c2");
    want(
        "abs rm in /private/tmp",
        f.run(&f.proj, &format!("rm -rf {}/sub", t.dir.display())),
        V::Allow,
    );
}

#[test]
fn r2_cd_tmpdir_then_rm_sub_is_allow() {
    let f = Fx::new("r2");
    let t = TmpFx::new("r2");
    let got = f.run(&f.proj, &format!("cd {} && rm -rf sub", t.dir.display()));
    want("cd /private/tmp/<d> && rm -rf sub", got, V::Allow);
}

// ---- R3: the repo (payload cwd) ----------------------------------------------

#[test]
fn c3_repo_absolute_form_allow() {
    let f = Fx::new("c3");
    want(
        "abs rm <proj>/target",
        f.run(&f.proj, &format!("rm -rf {}/target", f.proj.display())),
        V::Allow,
    );
}

#[test]
fn r3_cd_project_then_rm_target_is_allow() {
    let f = Fx::new("r3");
    let got = f.run(
        &f.proj,
        &format!("cd {} && rm -rf target", f.proj.display()),
    );
    want("cd <proj> && rm -rf target", got, V::Allow);
}

// ---- G1: dot-dot components ---------------------------------------------------

#[test]
fn g1_dotdot_cd_is_not_allow() {
    let f = Fx::new("g1");
    // outside/target is not a recoverable place for this payload cwd.
    let got = f.run(
        &f.proj,
        &format!(
            "cd {}/../../.. && rm -rf {}/target",
            f.wt.display(),
            f.outside.display()
        ),
    );
    not_allow("cd <wt>/../../.. && rm -rf <outside>/target", got);
    let t = TmpFx::new("g1");
    let name = t.dir.file_name().unwrap().to_str().unwrap().to_string();
    let got = f.run(
        &f.proj,
        &format!("cd {}/.. && rm -rf {name}", t.dir.display()),
    );
    not_allow("cd <tmpdir>/.. && rm -rf <name>", got);
}

// ---- G2: unresolved variable --------------------------------------------------

#[test]
fn g2_unresolved_variable_is_not_allow() {
    let f = Fx::new("g2");
    not_allow(
        "cd \"$WT\" && rm -rf target",
        f.run(&f.proj, "cd \"$WT\" && rm -rf target"),
    );
    not_allow(
        "cd $WT && echo a > README.md",
        f.run(&f.proj, "cd $WT && echo a > README.md"),
    );
}

// ---- G3: protected targets stay Deny -----------------------------------------

#[test]
fn g3_protected_targets_stay_deny() {
    let f = Fx::new("g3");
    want(
        "cd <wt> && rm -rf .git",
        f.run(&f.proj, &format!("cd {} && rm -rf .git", f.wt.display())),
        V::Deny,
    );
    want(
        "cd <wt> && rm <HOME>/.claude/settings.json",
        f.run(
            &f.proj,
            &format!(
                "cd {} && rm {}/.claude/settings.json",
                f.wt.display(),
                f.home.display()
            ),
        ),
        V::Deny,
    );
}

// ---- G4: separators that do not tie the cd to the command --------------------

#[test]
fn g4_non_tying_separators_are_not_allow() {
    let f = Fx::new("g4");
    // cwd is a non-repo dir: its own `target` is not a recoverable place.
    let cwd = f.outside.clone();
    not_allow(
        "control rm -rf <cwd>/target",
        f.run(&cwd, &format!("rm -rf {}/target", cwd.display())),
    );
    let wt = f.wt.display();
    for sep in [";", "||", "|", "&"] {
        let cmd = format!("cd {wt} {sep} rm -rf target");
        not_allow(&cmd.clone(), f.run(&cwd, &cmd));
    }
    let cmd = format!("(cd {wt}) && rm -rf target");
    not_allow(&cmd.clone(), f.run(&cwd, &cmd));
    let _ = (&f.hw, &f.base);
}

// ---- G5: unresolvable cd target ----------------------------------------------

#[test]
fn g5_nonexistent_cd_target_is_not_allow() {
    let f = Fx::new("g5");
    let got = f.run(
        &f.proj,
        &format!("cd {}/does-not-exist && rm -rf target", f.wt.display()),
    );
    not_allow("cd <wt>/does-not-exist && rm -rf target", got);
}

// ---- R4: heredoc body after cd ------------------------------------------------

fn commit_heredoc(prefix: &str, body_line: &str) -> String {
    format!(
        "{prefix}git commit -q -F - <<'EOF'\nsubject\n\n{body_line}\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\nEOF"
    )
}

#[test]
fn c4a_heredoc_commit_without_cd_allow() {
    let f = Fx::new("c4a");
    let cmd = commit_heredoc("", "body (paren) here");
    want("heredoc commit, no cd", f.run(&f.wt, &cmd), V::Allow);
}

#[test]
fn r4b_cd_heredoc_commit_trailer_only_is_allow() {
    let f = Fx::new("c4b");
    let cmd = commit_heredoc(&format!("cd {} && ", f.wt.display()), "body here");
    want(
        "cd <wt> && heredoc commit, no paren",
        f.run(&f.proj, &cmd),
        V::Allow,
    );
}

#[test]
fn c4b_cd_heredoc_commit_plain_body_allow() {
    let f = Fx::new("c4bp");
    let cmd = format!(
        "cd {} && git commit -q -F - <<'EOF'\nsubject\n\nbody (paren) here\nEOF",
        f.wt.display()
    );
    want(
        "cd <wt> && heredoc commit, plain body",
        f.run(&f.proj, &cmd),
        V::Allow,
    );
}

#[test]
fn r4_cd_heredoc_commit_with_paren_is_allow() {
    let f = Fx::new("r4");
    let cmd = commit_heredoc(&format!("cd {} && ", f.wt.display()), "body (paren) here");
    want(
        "cd <wt> && heredoc commit, paren body",
        f.run(&f.proj, &cmd),
        V::Allow,
    );
}

// ---- G6: real redirect onto a protected path stays Deny ------------------------

#[test]
fn g6_cd_cat_redirect_onto_protected_path_matches_no_cd() {
    let f = Fx::new("g6");
    let body = format!(
        "cat > {}/.claude/settings.json <<'EOF'\n{{}}\nEOF",
        f.home.display()
    );
    let plain = f.run(&f.proj, &body);
    let with_cd = f.run(&f.proj, &format!("cd {} && {body}", f.wt.display()));
    assert_ne!(
        plain,
        V::Allow,
        "no-cd redirect onto protected path must NOT be Allow"
    );
    assert_ne!(
        with_cd,
        V::Allow,
        "cd <wt> && redirect onto protected path must NOT be Allow"
    );
    assert_eq!(with_cd, plain, "cd form must match no-cd form");
}

// ---- fixture isolation: inherited GIT_* must not reach the repo under test ------

const GIT_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Plain git call with every inherited `GIT_*` repo-locating variable removed;
/// returns stdout. Independent of the `git` helper under test, but pinned to
/// `dir` the same way (`-C`, and `GIT_CEILING_DIRECTORIES` at its parent so a
/// `dir` without `.git` fails instead of discovering an enclosing repo).
fn clean_git(dir: &Path, args: &[&str]) -> String {
    let real = dir
        .canonicalize()
        .unwrap_or_else(|e| panic!("clean git dir {} is unusable: {e}", dir.display()));
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(&real)
        .args(args)
        .current_dir(&real)
        .env("GIT_CEILING_DIRECTORIES", real.parent().unwrap())
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com");
    for k in GIT_ENV {
        c.env_remove(k);
    }
    let out = c.output().expect("git runs");
    assert!(
        out.status.success(),
        "clean git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn repo_state(outer: &Path) -> String {
    format!(
        "HEAD={}\nbranches:\n{}\nworktrees:\n{}",
        clean_git(outer, &["rev-parse", "HEAD"]).trim(),
        clean_git(outer, &["branch", "--list"]),
        clean_git(outer, &["worktree", "list", "--porcelain"]),
    )
}

/// The fixture's `git` helper must not follow an inherited `GIT_DIR` (as set
/// when the suite runs under a git hook): the fixture repo is under the
/// fixture base, never whatever repo the environment points at. The child
/// re-runs one existing `Fx::new` test with `GIT_DIR` aimed at a scratch outer
/// repo; that outer repo must come back unchanged.
#[test]
fn fixture_git_ignores_inherited_git_dir() {
    let nonce = SEQ.fetch_add(1, Ordering::SeqCst);
    let outer = neutral_base::neutral_base()
        .join("cd_and_chain_equivalence")
        .join(format!("gitenv-outer-{}-{nonce}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outer);
    std::fs::create_dir_all(&outer).unwrap();
    clean_git(&outer, &["init", "-q"]);
    std::fs::write(outer.join("f.txt"), "o\n").unwrap();
    clean_git(&outer, &["add", "f.txt"]);
    clean_git(&outer, &["commit", "-q", "-m", "outer"]);
    let before = repo_state(&outer);

    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "c3_repo_absolute_form_allow", "--test-threads=1"])
        .env("GIT_DIR", outer.join(".git"))
        .output()
        .expect("child test binary runs");

    let after = repo_state(&outer);
    assert_eq!(
        before,
        after,
        "outer repo was mutated by the fixture git helper following inherited GIT_DIR.\n\
         --- before ---\n{before}\n--- after ---\n{after}\n\
         --- child stdout ---\n{}\n--- child stderr ---\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(
        child.status.success(),
        "child test run failed: {:?}\n{}",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
}
