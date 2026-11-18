#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S4 B.5 follow-up rulings of 2026-11-18 (user), written by an independent
//! test author (not the S4 implementer; CLAUDE.md §2(a)).
//!
//!   T1  A program file that is git-tracked AND unmodified gets NO B.5
//!       opinion: the legacy verdict stands (Allow for benign legacy).
//!       "Unmodified" = clean in the work tree (`git status` shows no change)
//!       AND not written by an earlier part of the same command. Covers
//!       interpreter file runs (`python3 scripts/x.py`, `bash scripts/x.sh`,
//!       `node x.js`) and direct exec (`./scripts/x.sh`). NOT covered (stays
//!       Undetermined = main Ask / subagent Deny): `python3 -m pytest`, an
//!       untracked file, a tracked file with uncommitted modifications, a
//!       tracked file written earlier in the same command (judged by the `&&`
//!       rule, else Undetermined), a path outside the repo, and anything git
//!       cannot answer.
//!   T2  `bash <&'$fd'` (program read from a file descriptor) is Undetermined:
//!       main Ask, subagent Deny. (The differential corpus
//!       `shell_parser_differential_corpus_20261115.rs` still lists the #1791
//!       twin `bash <&'$fd'` in its Allow sanity list; it is re-anchored by
//!       another agent and deliberately NOT edited here.)
//!   T3  `bash -n x.sh` / `sh -n x.sh` (noexec, syntax check only) get no B.5
//!       opinion -> Allow when benign.
//!
//! Combined `-n` forms are the test author's call (the ruling left them open):
//!   * `bash -nx x.sh` -> Allow. `-x` only traces; noexec still holds and no
//!     command is run, so the case is the T3 case with a harmless extra flag.
//!   * `bash -ni x.sh` -> Undetermined. bash IGNORES `-n` for interactive
//!     shells (bash(1): "-n ... This is ignored by interactive shells"), so
//!     with `-i` the noexec exemption would be a false premise.
//!   * `bash x.sh -n` -> Undetermined: `-n` after the program file is an
//!     argument to the script, not a bash option.
//!   * `bash -n -c 'BODY'` -> the literal body is judged as if it ran (benign
//!     Allow, System-harm Deny). Exempting `-c` bodies would make the verdict
//!     depend on the option parser recognising every noexec-cancelling form
//!     (`-i`, `+n`, `-o`/`+o` spellings); judging the body costs nothing for a
//!     benign body and keeps System harm Deny through any wrapper (standing
//!     ruling: system harm always Deny).
//!
//! Harness: same as `s4_b5_layer_20261118.rs` (hermetic env via `env_clear`,
//! `PATH=/usr/bin:/bin` so the hook's own `git` probes work, fixture base from
//! `support/neutral_base.rs` — the location-neutral base, deliberately NOT
//! `/tmp`; `support/fixed_tmp.rs` documents why `/tmp` would decide verdicts).
//! T1 builds a REAL git repo as the project dir: committed files, an untracked
//! file, a modified tracked file, a staged-only modification, an ignored file,
//! a committed symlink whose target lies outside the repo, and a second clean
//! repo next to the project ("outside the repo").
//!
//! Assertion vocabulary: `allow` (both modes Allow), `undetermined` (main
//! exactly Ask, subagent Deny), `deny` (both modes Deny).
//!
//! DOES NOT PROVE: which rule produced a verdict or its reason text; the
//! behaviour for a tracked file whose COMMITTED body is harmful (legacy
//! verdict is not pinned here); submodules, linked worktrees, sparse checkout
//! or `assume-unchanged` files; T1 with cwd in a repo subdirectory; T2 for
//! descriptor numbers other than the literal `'$fd'` spelling.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn other(&self) -> PathBuf {
        self.base.join("src").join("other")
    }
    fn outside(&self) -> PathBuf {
        self.base.join("outside")
    }
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let mut cmd = git_env::pinned(dir);
    cmd.args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(p: &Path, body: &str, exec: bool) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
    if exec {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Plain project dir (no git), as in `s4_b5_layer_20261118.rs`.
fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "s4-b5-tn-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("home").join("Library").join("Caches")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

/// The project dir is a real git repo:
///   committed+clean : scripts/x.py, scripts/x.sh, x.js, scripts/run.sh (755),
///                     scripts/mod.sh, scripts/staged.sh, scripts/out.sh
///                     (symlink -> <base>/outside/evil.sh, outside the repo)
///   modified (work tree only)   : scripts/mod.sh
///   modified and staged         : scripts/staged.sh
///   untracked                   : scripts/new.sh, scripts/new.py
///   ignored                     : scripts/ign.sh
/// Plus a second clean repo `<base>/src/other` with committed `x.sh`.
fn repo_fixture(tag: &str) -> Fx {
    let fx = fixture(tag);
    let p = fx.proj();
    git(&p, &["init", "-q"]);
    write(&p.join("scripts/x.py"), "print(1)\n", false);
    write(&p.join("scripts/x.sh"), "echo hi\n", false);
    write(&p.join("x.js"), "console.log(1)\n", false);
    write(&p.join("scripts/run.sh"), "#!/bin/sh\necho hi\n", true);
    write(&p.join("scripts/mod.sh"), "echo hi\n", false);
    write(&p.join("scripts/staged.sh"), "echo hi\n", false);
    write(&fx.outside().join("evil.sh"), "echo hi\n", true);
    std::os::unix::fs::symlink(fx.outside().join("evil.sh"), p.join("scripts/out.sh")).unwrap();
    write(&p.join(".gitignore"), "scripts/ign.sh\n", false);
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "init"]);
    write(&p.join("scripts/mod.sh"), "echo changed\n", false);
    write(&p.join("scripts/staged.sh"), "echo changed\n", false);
    git(&p, &["add", "scripts/staged.sh"]);
    write(&p.join("scripts/new.sh"), "echo hi\n", false);
    write(&p.join("scripts/new.py"), "print(1)\n", false);
    write(&p.join("scripts/ign.sh"), "echo hi\n", false);

    let o = fx.other();
    std::fs::create_dir_all(&o).unwrap();
    git(&o, &["init", "-q"]);
    write(&o.join("x.sh"), "echo hi\n", false);
    git(&o, &["add", "-A"]);
    git(&o, &["commit", "-q", "-m", "init"]);

    // Fixture self-check: the states the tests rely on are the states git
    // reports (a fixture that silently drifted would test nothing).
    let st = git_stdout(&p, &["status", "--porcelain", "--ignored"]);
    for want in [
        " M scripts/mod.sh",
        "M  scripts/staged.sh",
        "?? scripts/new.sh",
        "?? scripts/new.py",
        "!! scripts/ign.sh",
    ] {
        assert!(
            st.lines().any(|l| l == want),
            "fixture status lacks {want:?}:\n{st}"
        );
    }
    for clean in [
        "scripts/x.py",
        "scripts/x.sh",
        "x.js",
        "scripts/run.sh",
        "scripts/out.sh",
    ] {
        assert!(
            !st.contains(clean),
            "fixture: {clean} must be clean, status:\n{st}"
        );
    }
    assert!(git_stdout(&o, &["status", "--porcelain"]).is_empty());
    fx
}

/// Run the hook once; `(decision, reason)`. Same ordering contract as
/// `s4_b5_layer_20261118.rs`: non-zero exit panics, then empty stdout =
/// Allow, then non-JSON panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "s4-b5-tracked-noexec-20261118",
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
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

fn expect_allow(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, &fx.proj(), command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

fn expect_undetermined(fx: &Fx, command: &str) {
    let (dm, rm) = run_as(fx, &fx.proj(), command, false);
    assert_eq!(
        dm, "ask",
        "main: {command:?} expected Ask (Undetermined), got {dm} ({rm})"
    );
    let (ds, rs) = run_as(fx, &fx.proj(), command, true);
    assert_eq!(
        ds, "deny",
        "subagent: {command:?} expected Deny (Undetermined), got {ds} ({rs})"
    );
}

fn expect_deny(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, &fx.proj(), command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

// ---------------------------------------------------------------------------
// T1 — tracked AND unmodified program file: no B.5 opinion, legacy stands.
// ---------------------------------------------------------------------------

#[test]
fn t1_python3_tracked_clean_allows() {
    expect_allow(&repo_fixture("py"), "python3 scripts/x.py");
}

#[test]
fn t1_bash_tracked_clean_allows() {
    expect_allow(&repo_fixture("bash"), "bash scripts/x.sh");
}

#[test]
fn t1_sh_tracked_clean_allows() {
    expect_allow(&repo_fixture("sh"), "sh scripts/x.sh");
}

#[test]
fn t1_node_tracked_clean_allows() {
    expect_allow(&repo_fixture("node"), "node x.js");
}

#[test]
fn t1_dot_slash_tracked_clean_allows() {
    expect_allow(&repo_fixture("dotslash"), "./scripts/run.sh");
}

#[test]
fn t1_relative_dir_tracked_clean_allows() {
    expect_allow(&repo_fixture("reldir"), "scripts/run.sh");
}

#[test]
fn t1_bash_dot_slash_spelling_tracked_clean_allows() {
    expect_allow(&repo_fixture("bashdot"), "bash ./scripts/x.sh");
}

/// Data operands after the program do not change the T1 reading (R9).
#[test]
fn t1_python3_tracked_clean_with_data_operand_allows() {
    expect_allow(&repo_fixture("pydata"), "python3 scripts/x.py d.txt");
}

/// The absolute spelling of a tracked clean file inside the repo.
#[test]
fn t1_absolute_path_tracked_clean_allows() {
    let fx = repo_fixture("abs");
    let cmd = format!("bash {}", fx.proj().join("scripts/x.sh").display());
    expect_allow(&fx, &cmd);
}

// -- not covered by T1 -------------------------------------------------------

#[test]
fn t1_python3_m_pytest_undetermined() {
    expect_undetermined(&repo_fixture("pytest"), "python3 -m pytest");
}

#[test]
fn t1_untracked_sh_undetermined() {
    expect_undetermined(&repo_fixture("untrsh"), "bash scripts/new.sh");
}

#[test]
fn t1_untracked_py_undetermined() {
    expect_undetermined(&repo_fixture("untrpy"), "python3 scripts/new.py");
}

/// An ignored file is untracked.
#[test]
fn t1_ignored_untracked_undetermined() {
    expect_undetermined(&repo_fixture("ign"), "bash scripts/ign.sh");
}

/// An absent file is not a tracked clean file.
#[test]
fn t1_absent_file_in_repo_undetermined() {
    expect_undetermined(&repo_fixture("absent"), "bash scripts/nope.sh");
}

#[test]
fn t1_tracked_modified_undetermined() {
    expect_undetermined(&repo_fixture("mod"), "bash scripts/mod.sh");
}

/// A staged modification shows in `git status`: not "unmodified".
#[test]
fn t1_tracked_staged_modification_undetermined() {
    expect_undetermined(&repo_fixture("staged"), "bash scripts/staged.sh");
}

/// R5: `;` between write and run -> Undetermined, tracked or not.
#[test]
fn t1_tracked_appended_then_run_semicolon_undetermined() {
    expect_undetermined(
        &repo_fixture("appsemi"),
        "echo x >> scripts/x.sh; bash scripts/x.sh",
    );
}

/// R5: an in-place edit then run -> Undetermined even across `&&`.
#[test]
fn t1_tracked_sed_edit_then_run_undetermined() {
    expect_undetermined(
        &repo_fixture("sed"),
        "sed -i s/a/b/ scripts/x.sh && bash scripts/x.sh",
    );
}

/// Direct exec of a tracked file truncated/rewritten earlier with `;`.
#[test]
fn t1_tracked_overwritten_then_direct_exec_undetermined() {
    expect_undetermined(
        &repo_fixture("owdot"),
        "echo x > scripts/run.sh; ./scripts/run.sh",
    );
}

/// `&&`-joined heredoc write into a tracked path is judged by the `&&` rule:
/// a System-harm body is Deny (not laundered by the file being tracked).
#[test]
fn t1_tracked_heredoc_harm_then_run_denies() {
    expect_deny(
        &repo_fixture("hdharm"),
        "cat > scripts/x.sh <<'EOF' && bash scripts/x.sh\nchmod 777 /etc/hosts\nEOF",
    );
}

/// A clean tracked file in ANOTHER repo is outside this repo.
#[test]
fn t1_other_repo_tracked_clean_undetermined() {
    let fx = repo_fixture("other");
    let cmd = format!("bash {}", fx.other().join("x.sh").display());
    expect_undetermined(&fx, &cmd);
}

/// Same, by a relative `../` spelling.
#[test]
fn t1_other_repo_relative_undetermined() {
    expect_undetermined(&repo_fixture("otherrel"), "bash ../other/x.sh");
}

/// A committed, clean symlink whose target is outside the repo: what runs is
/// an untracked file outside the repo.
#[test]
fn t1_tracked_symlink_to_outside_undetermined() {
    expect_undetermined(&repo_fixture("symout"), "bash scripts/out.sh");
}

/// git cannot answer: the project dir is not a git repo at all.
#[test]
fn t1_no_repo_undetermined() {
    let fx = fixture("norepo");
    write(&fx.proj().join("scripts/x.sh"), "echo hi\n", false);
    expect_undetermined(&fx, "bash scripts/x.sh");
}

// ---------------------------------------------------------------------------
// T2 — program read from a file descriptor.
// ---------------------------------------------------------------------------

#[test]
fn t2_bash_dup_fd_literal_undetermined() {
    expect_undetermined(&fixture("fd"), "bash <&'$fd'");
}

#[test]
fn t2_bash_dup_fd_assigned_semicolon_undetermined() {
    expect_undetermined(&fixture("fdsemi"), "fd=3; bash <&'$fd'");
}

#[test]
fn t2_bash_dup_fd_assigned_and_undetermined() {
    expect_undetermined(&fixture("fdand"), "fd=0 && bash <&'$fd'");
}

/// The double-quoted twin (corpus #1791 control) stays Undetermined.
#[test]
fn t2_bash_dup_fd_double_quoted_undetermined() {
    expect_undetermined(&fixture("fddq"), "fd=3; bash <&\"$fd\"");
}

// ---------------------------------------------------------------------------
// T3 — noexec syntax check.
// ---------------------------------------------------------------------------

#[test]
fn t3_bash_n_allows() {
    expect_allow(&fixture("bashn"), "bash -n x.sh");
}

#[test]
fn t3_sh_n_allows() {
    expect_allow(&fixture("shn"), "sh -n x.sh");
}

/// Also inside a repo, for an untracked file (T3 does not depend on T1).
#[test]
fn t3_bash_n_untracked_in_repo_allows() {
    expect_allow(&repo_fixture("bashnrepo"), "bash -n scripts/new.sh");
}

/// Author's call: `-x` only traces; noexec holds.
#[test]
fn t3_bash_nx_allows() {
    expect_allow(&fixture("bashnx"), "bash -nx x.sh");
}

/// Author's call: interactive shells ignore `-n`.
#[test]
fn t3_bash_ni_undetermined() {
    expect_undetermined(&fixture("bashni"), "bash -ni x.sh");
}

/// `-n` after the program file is a script argument, not noexec.
#[test]
fn t3_n_after_file_undetermined() {
    expect_undetermined(&fixture("nafter"), "bash x.sh -n");
}

/// Author's call: a literal `-c` body is judged as if it ran.
#[test]
fn t3_bash_n_c_benign_allows() {
    expect_allow(&fixture("bashnc"), "bash -n -c 'echo hi'");
}

/// Author's call: System harm stays Deny through `-n`.
#[test]
fn t3_bash_n_c_harm_denies() {
    expect_deny(&fixture("bashncharm"), "bash -n -c 'chmod 777 /etc/hosts'");
}
