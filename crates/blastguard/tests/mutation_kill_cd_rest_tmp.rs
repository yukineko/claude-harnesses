#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Kill tests for mutation survivors in `literal_cd_and_rest`, `opener_consumer`
//! and the `/private/tmp` truncation relaxation (`temp_disposable_redirect_target`
//! and its caller), judged end to end through the hook binary.
//!
//! Each `k_*` test asserts the SAFE verdict (not Allow) for one input a single
//! guard exists to refuse; each `c_*` test is the Allow control for the same
//! fixture shape, so a `k_*` that goes RED is attributable to the guard alone
//! and not to a fixture that never reached the relaxed path.
//!
//! Temp fixtures live under `/private/tmp` (the fixed temp root the relaxation
//! is about); the project and home dirs live under the location-neutral base.

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

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    /// unique dir under /private/tmp (canonical spelling)
    tmp: PathBuf,
    session: String,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let uniq = format!("{}-{n}-{nanos}-{name}", std::process::id());
        let base = neutral_base::neutral_base()
            .join("mutation_kill_cd_rest_tmp")
            .join(&uniq);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        let tmp = PathBuf::from(format!("/private/tmp/bg-mutkill-{uniq}"));
        std::fs::create_dir_all(&tmp).unwrap();
        Fx {
            base,
            home,
            proj,
            tmp,
            session: format!("mutkill-{uniq}"),
        }
    }

    /// Run the hook with payload cwd `cwd`; the hook process also runs IN
    /// `cwd` (as Claude Code runs it in the session directory).
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
            .current_dir(cwd);
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
        println!("[{command:?}] cwd={} => {:?}", cwd.display(), s.trim());
        classify(&s)
    }

    fn write(&self, p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// `<tmp>/d/sub/f` and `<tmp>/d/sub2/f`; returns `<tmp>/d`.
    fn tmp_d(&self) -> PathBuf {
        let d = self.tmp.join("d");
        self.write(&d.join("sub/f"), "x\n");
        self.write(&d.join("sub2/f"), "x\n");
        d
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
        let _ = std::fs::remove_dir_all(&self.base);
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

// ---- literal_cd_and_rest: the tied `cd D && REST` shape ----------------------

/// Control: the one shape `literal_cd_and_rest` accepts is Allow, and the same
/// relative `rm` behind a NON-tying `cd` is not (so the tied reading is what
/// decides every `k_cd_*` below).
#[test]
fn c_cd_tied_rest_allows_and_untied_does_not() {
    let f = Fx::new("c-cd");
    let d = f.tmp_d();
    want(
        "cd <tmp>/d && rm -rf sub",
        f.run(&f.proj, &format!("cd {} && rm -rf sub", d.display())),
        V::Allow,
    );
    want(
        "relative cd from <tmp>: cd d && rm -rf sub (absolute control)",
        f.run(&f.tmp, &format!("cd {} && rm -rf sub", d.display())),
        V::Allow,
    );
}

/// LA: `D` must be absolute.
#[test]
fn k_cd_relative_dir_is_not_allow() {
    let f = Fx::new("la");
    f.tmp_d();
    not_allow("cd d && rm -rf sub", f.run(&f.tmp, "cd d && rm -rf sub"));
}

/// LB: every character of `D` must be in the literal set (here `:`, `*`).
#[test]
fn k_cd_dir_with_non_literal_char_is_not_allow() {
    let f = Fx::new("lb");
    for name in ["a:b", "w*", "q=r"] {
        let d = f.tmp.join(name);
        f.write(&d.join("sub/f"), "x\n");
        let cmd = format!("cd {} && rm -rf sub", d.display());
        not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
    }
}

/// L3a: no component of `D` may be `.`.
#[test]
fn k_cd_dir_with_dot_component_is_not_allow() {
    let f = Fx::new("l3a");
    f.tmp_d();
    let cmd = format!("cd {}/./d && rm -rf sub", f.tmp.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

/// LS1..LS4, LAMP: a separator in `REST` lets a later command run whether or
/// not the `cd` succeeded (or in another shell), so its relative operand may
/// land in the ORIGINAL cwd.
#[test]
fn k_cd_rest_semicolon_is_not_allow() {
    let f = Fx::new("ls1");
    let d = f.tmp_d();
    let cmd = format!("cd {} && rm -rf sub; rm -rf sub2", d.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

#[test]
fn k_cd_rest_pipe_is_not_allow() {
    let f = Fx::new("ls2");
    let d = f.tmp_d();
    let cmd = format!("cd {} && rm -rf sub | rm -rf sub2", d.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

#[test]
fn k_cd_rest_newline_is_not_allow() {
    let f = Fx::new("ls3");
    let d = f.tmp_d();
    let cmd = format!("cd {} && rm -rf sub\nrm -rf sub2", d.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

#[test]
fn k_cd_rest_carriage_return_is_not_allow() {
    let f = Fx::new("ls4");
    let d = f.tmp_d();
    let cmd = format!("cd {} && rm -rf sub\rrm -rf sub2", d.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

#[test]
fn k_cd_rest_background_ampersand_is_not_allow() {
    let f = Fx::new("lamp");
    let d = f.tmp_d();
    let cmd = format!("cd {} && rm -rf sub & rm -rf sub2", d.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

// ---- opener_consumer: only a permitted `cd` head is split off ---------------

fn heredoc(prefix: &str, target: &Path) -> String {
    format!(
        "{prefix}cat > {}/notes.txt <<'EOF'\nrm -rf ~/Documents\nEOF",
        target.display()
    )
}

#[test]
fn c_opener_permitted_cd_head_body_is_data() {
    let f = Fx::new("c-op");
    let cmd = heredoc(&format!("cd {} && ", f.tmp.display()), &f.tmp);
    want(
        "cd <tmp> && cat > notes.txt <<'EOF'",
        f.run(&f.proj, &cmd),
        V::Allow,
    );
}

/// O1: a permitted NON-cd head (`mkdir -p`) is not split.
#[test]
fn k_opener_non_cd_head_is_not_split() {
    let f = Fx::new("o1");
    let cmd = heredoc(&format!("mkdir -p {} && ", f.tmp.display()), &f.tmp);
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

/// O2: a `cd` head that is not a permitted literal (quoted) is not split.
#[test]
fn k_opener_non_literal_cd_head_is_not_split() {
    let f = Fx::new("o2");
    let cmd = heredoc(&format!("cd \"{}\" && ", f.tmp.display()), &f.tmp);
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

// ---- temp_disposable_redirect_target and its caller -------------------------

#[test]
fn c_tmp_plain_existing_file_allows() {
    let f = Fx::new("c-tmp");
    f.write(&f.tmp.join("f.txt"), "p\n");
    want(
        "echo a > <tmp>/f.txt",
        f.run(&f.proj, &format!("echo a > {}/f.txt", f.tmp.display())),
        V::Allow,
    );
}

/// T1a: a glob target.
#[test]
fn k_tmp_glob_target_is_not_allow() {
    let f = Fx::new("t1a");
    f.write(&f.tmp.join("f*"), "p\n");
    let cmd = format!("echo a > {}/f*", f.tmp.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
}

/// T1b: an expansion in the target.
#[test]
fn k_tmp_expansion_target_is_not_allow() {
    let f = Fx::new("t1b");
    f.write(&f.tmp.join("f.txt"), "p\n");
    for cmd in [
        format!("echo a > {}/$X/f.txt", f.tmp.display()),
        format!("echo a > {}/$(echo f.txt)", f.tmp.display()),
        format!("echo a > {}/`echo f.txt`", f.tmp.display()),
    ] {
        not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
    }
}

/// T2: a `.git` component in the spelling, including one `..` collapses away.
#[test]
fn k_tmp_git_component_target_is_not_allow() {
    let f = Fx::new("t2");
    f.write(&f.tmp.join("r/.git/config"), "[core]\n");
    f.write(&f.tmp.join("f.txt"), "p\n");
    for cmd in [
        format!("echo a > {}/r/.git/config", f.tmp.display()),
        format!("echo a > {}/.git/../f.txt", f.tmp.display()),
    ] {
        not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
    }
}

/// T3/T6: protected-path targets under tmp, spelled directly and relative to
/// a payload cwd inside the protected directory.
#[test]
fn k_tmp_protected_target_is_not_allow() {
    let f = Fx::new("t3");
    f.write(&f.tmp.join(".claude/settings.json"), "{}\n");
    f.write(&f.tmp.join(".githooks/pre-commit"), "#!/bin/sh\n");
    f.write(&f.tmp.join("x/.git/hooks/pre-commit"), "#!/bin/sh\n");
    for cmd in [
        format!("echo a > {}/.claude/settings.json", f.tmp.display()),
        format!("echo a > {}/.githooks/pre-commit", f.tmp.display()),
        format!("echo a > {}/x/.git/hooks/pre-commit", f.tmp.display()),
    ] {
        not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
    }
    not_allow(
        "cwd <tmp>/.claude: echo a > settings.json",
        f.run(&f.tmp.join(".claude"), "echo a > settings.json"),
    );
    not_allow(
        "cwd <tmp>/.githooks: echo a > pre-commit",
        f.run(&f.tmp.join(".githooks"), "echo a > pre-commit"),
    );
}

/// T5/T6: a symlink under tmp whose real path is in a `.git` dir or protected.
#[test]
fn k_tmp_symlink_to_git_or_protected_is_not_allow() {
    let f = Fx::new("t5");
    f.write(&f.tmp.join("r/.git/info/exclude"), "\n");
    f.write(&f.tmp.join(".claude/settings.json"), "{}\n");
    std::os::unix::fs::symlink(f.tmp.join("r/.git"), f.tmp.join("gl")).unwrap();
    std::os::unix::fs::symlink(f.tmp.join(".claude"), f.tmp.join("cl")).unwrap();
    for cmd in [
        format!("echo a > {}/gl/info/exclude", f.tmp.display()),
        format!("echo a > {}/cl/settings.json", f.tmp.display()),
    ] {
        not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
    }
}

/// T7: an existing file under a `.git` entry inside tmp that git itself does
/// not read as a work tree (`core.bare = true`), so the recoverability probe
/// answers NotRepo -> Unrecoverable and only the `.git`-entry walk refuses it.
#[test]
fn k_tmp_file_under_dot_git_entry_is_not_allow() {
    let f = Fx::new("t7");
    let repo = f.tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "core.bare", "true"]);
    f.write(&repo.join("w.txt"), "work\n");
    let cmd = format!("echo a > {}/w.txt", repo.display());
    not_allow(&cmd.clone(), f.run(&f.proj, &cmd));
    // Control: the same file with no `.git` entry anywhere is Allow.
    f.write(&f.tmp.join("plain/w.txt"), "work\n");
    want(
        "echo a > <tmp>/plain/w.txt",
        f.run(
            &f.proj,
            &format!("echo a > {}/plain/w.txt", f.tmp.display()),
        ),
        V::Allow,
    );
}
