// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec tests for GitHub Actions: observation is unconditionally Allow (G),
//! execution / configuration is Ask with an "explicit user request" reason,
//! and writes to `.github/workflows/` (W) Ask too. Never Deny.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let root =
            Path::new("/var/tmp").join(format!("bg-gha-workflows-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let base = std::fs::canonicalize(&root).unwrap();
        let f = Fx {
            home: base.join("home"),
            repo: base.join("proj"),
            base,
        };
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::create_dir_all(&f.repo).unwrap();
        f
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.home)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn run(&self, command: &str) -> String {
        self.call(
            "Bash",
            serde_json::json!({ "command": command }),
            &self.repo,
        )
    }

    fn call(&self, tool: &str, input: serde_json::Value, cwd: &Path) -> String {
        let payload = serde_json::json!({
            "session_id": "gha-workflows",
            "cwd": cwd.display().to_string(),
            "tool_name": tool,
            "tool_input": input,
        })
        .to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("TMPDIR")
            .current_dir(&self.base)
            .spawn()
            .expect("binary spawns");
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn decision(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "unparsed"
    }
}

fn assert_gha_ask(out: &str, what: &str) {
    assert_eq!(decision(out), "ask", "{what} must Ask; got {out:?}");
    let r = out.to_lowercase();
    assert!(
        r.contains("explicit") && r.contains("github actions"),
        "{what}: reason must say GitHub Actions needs an explicit user request; got {out:?}"
    );
}

#[test]
fn g_observation_allows() {
    let f = Fx::new("g1");
    for cmd in [
        "gh run list",
        "gh run list --workflow ci.yml --limit 5",
        "gh run view 123",
        "gh run view 123 --log",
        "gh run watch 123",
        "gh run download 123",
        "gh workflow list",
        "gh workflow view ci.yml",
        "gh cache list",
        "gh api repos/o/r/actions/runs",
        "gh api repos/o/r/actions/workflows",
        "gh -R o/r run list",
    ] {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "allow", "`{cmd}` must Allow; got {out:?}");
    }
}

#[test]
fn g_execution_and_configuration_ask_with_explicit_request_reason() {
    let f = Fx::new("g2");
    for cmd in [
        "gh workflow run ci.yml",
        "gh workflow enable ci.yml",
        "gh workflow disable ci.yml",
        "gh run rerun 123",
        "gh run cancel 123",
        "gh run delete 123",
        "gh cache delete 42",
        "gh secret set FOO --body x",
        "gh secret delete FOO",
        "gh variable set FOO --body x",
        "gh variable delete FOO",
        "gh api -X POST repos/o/r/actions/workflows/x/dispatches",
        "gh api --method DELETE repos/o/r/actions/runs/1",
    ] {
        let out = f.run(cmd);
        assert_gha_ask(&out, &format!("`{cmd}`"));
    }
}

fn wf_input(tool: &str, path: &Path) -> serde_json::Value {
    let p = path.display().to_string();
    match tool {
        "Write" => serde_json::json!({ "file_path": p, "content": "on: push\n" }),
        "Edit" => serde_json::json!({
            "file_path": p, "old_string": "on: push", "new_string": "on: pull_request"
        }),
        _ => serde_json::json!({
            "file_path": p,
            "edits": [{ "old_string": "on: push", "new_string": "on: pull_request" }]
        }),
    }
}

#[test]
fn w_file_tools_on_workflow_file_ask_plain_repo() {
    let f = Fx::new("w1");
    let wf_dir = f.repo.join(".github/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    let wf = wf_dir.join("ci.yml");
    std::fs::write(&wf, "on: push\n").unwrap();
    for tool in ["Write", "Edit", "MultiEdit"] {
        let out = f.call(tool, wf_input(tool, &wf), &f.repo);
        assert_gha_ask(&out, &format!("{tool} on .github/workflows/ci.yml"));
    }
    // A new (not yet existing) workflow file is configuration too.
    let new = wf_dir.join("new.yml");
    let out = f.call("Write", wf_input("Write", &new), &f.repo);
    assert_gha_ask(&out, "Write creating .github/workflows/new.yml");
}

#[test]
fn w_file_tools_on_workflow_file_ask_in_git_worktree() {
    let f = Fx::new("w2");
    f.git(&["init", "-q", "-b", "main"]);
    std::fs::write(f.repo.join("a.txt"), "x\n").unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    let wt = f.base.join("wt1");
    f.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"]);
    let wf_dir = wt.join(".github/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    let wf = wf_dir.join("ci.yml");
    std::fs::write(&wf, "on: push\n").unwrap();
    for tool in ["Write", "Edit", "MultiEdit"] {
        let out = f.call(tool, wf_input(tool, &wf), &wt);
        assert_gha_ask(&out, &format!("{tool} in worktree on ci.yml"));
    }
}

#[test]
fn w_bash_writes_into_workflows_ask() {
    let f = Fx::new("w3");
    std::fs::create_dir_all(f.repo.join(".github/workflows")).unwrap();
    std::fs::write(f.repo.join("a.yml"), "on: push\n").unwrap();
    for cmd in [
        "echo x > .github/workflows/ci.yml",
        "echo x >> .github/workflows/ci.yml",
        "echo x | tee .github/workflows/ci.yml",
        "tee .github/workflows/ci.yml < a.yml",
        "cp a.yml .github/workflows/ci.yml",
        "mv a.yml .github/workflows/ci.yml",
    ] {
        let out = f.run(cmd);
        assert_gha_ask(&out, &format!("`{cmd}`"));
    }
}

#[test]
fn w_reading_workflow_files_allows() {
    let f = Fx::new("w4");
    let wf_dir = f.repo.join(".github/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(wf_dir.join("ci.yml"), "on: push\n").unwrap();
    for cmd in [
        "cat .github/workflows/ci.yml",
        "grep push .github/workflows/ci.yml",
        "ls .github/workflows",
    ] {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "allow", "`{cmd}` must Allow; got {out:?}");
    }
}

// Case-insensitivity of `exclude::is_workflow_path`: a case-variant spelling
// is not a bypass on a case-folding filesystem.
const CASE_VARIANTS: [&str; 3] = [
    ".GitHub/Workflows/x.yml",
    ".GITHUB/WORKFLOWS/x.yml",
    ".github/WorkFlows/x.yml",
];

#[test]
fn w_case_variant_file_tools_ask() {
    let f = Fx::new("w5");
    for v in CASE_VARIANTS {
        let p = f.repo.join(v);
        for tool in ["Write", "Edit"] {
            let out = f.call(tool, wf_input(tool, &p), &f.repo);
            assert_gha_ask(&out, &format!("{tool} on case-variant {v}"));
        }
    }
}

#[test]
fn w_case_variant_bash_redirect_asks() {
    let f = Fx::new("w6");
    for v in CASE_VARIANTS {
        for cmd in [format!("echo x > {v}"), format!("echo x >> {v}")] {
            let out = f.run(&cmd);
            assert_gha_ask(&out, &format!("`{cmd}`"));
        }
    }
}

#[test]
fn w_case_variant_in_git_worktree_asks() {
    let f = Fx::new("w7");
    f.git(&["init", "-q", "-b", "main"]);
    std::fs::write(f.repo.join("a.txt"), "x\n").unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    let wt = f.base.join("wt1");
    f.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"]);
    for v in CASE_VARIANTS {
        let p = wt.join(v);
        for tool in ["Write", "Edit"] {
            let out = f.call(tool, wf_input(tool, &p), &wt);
            assert_gha_ask(&out, &format!("{tool} in worktree on case-variant {v}"));
        }
        let cmd = format!("echo x > {v}");
        let out = f.call("Bash", serde_json::json!({ "command": cmd }), &wt);
        assert_gha_ask(&out, &format!("`{cmd}` in worktree"));
    }
}

// Dot-dot spellings of a workflow path: `exclude::is_workflow_path` must
// resolve `..` components, otherwise `x/../workflows` hides the directory.
const DOTDOT_VARIANTS: [&str; 3] = [
    ".github/x/../workflows/ci.yml",
    "a/../.github/workflows/ci.yml",
    ".GitHub/x/../Workflows/ci.yml",
];

#[test]
fn w_dotdot_variant_file_tools_ask() {
    let f = Fx::new("w8");
    for v in DOTDOT_VARIANTS {
        let p = f.repo.join(v);
        for tool in ["Write", "Edit"] {
            let out = f.call(tool, wf_input(tool, &p), &f.repo);
            assert_gha_ask(&out, &format!("{tool} on dot-dot variant {v}"));
        }
    }
}

#[test]
fn w_dotdot_variant_bash_redirect_asks() {
    let f = Fx::new("w9");
    for v in DOTDOT_VARIANTS {
        let cmd = format!("echo x > {v}");
        let out = f.run(&cmd);
        assert_gha_ask(&out, &format!("`{cmd}`"));
    }
}

#[test]
fn w_dotdot_variant_in_git_worktree_asks() {
    let f = Fx::new("w10");
    f.git(&["init", "-q", "-b", "main"]);
    std::fs::write(f.repo.join("a.txt"), "x\n").unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    let wt = f.base.join("wt1");
    f.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"]);
    for v in DOTDOT_VARIANTS {
        let p = wt.join(v);
        for tool in ["Write", "Edit"] {
            let out = f.call(tool, wf_input(tool, &p), &wt);
            assert_gha_ask(&out, &format!("{tool} in worktree on dot-dot variant {v}"));
        }
        let cmd = format!("echo x > {v}");
        let out = f.call("Bash", serde_json::json!({ "command": cmd }), &wt);
        assert_gha_ask(&out, &format!("`{cmd}` in worktree"));
    }
}

// Symlink spellings of a workflow path (user ruling 2026-11-01): blastguard
// must resolve symlinks before judging a write to `.github/workflows`, and
// Ask when resolution is impossible. A purely lexical check lets `wf/ci.yml`
// (with `wf -> .github/workflows`) through.
fn symlink_repo(root: &Path) {
    std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
    std::fs::write(root.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
}

#[test]
fn w_symlinked_dir_file_tools_and_bash_ask() {
    let f = Fx::new("w11");
    f.git(&["init", "-q", "-b", "main"]);
    symlink_repo(&f.repo);
    std::os::unix::fs::symlink(".github/workflows", f.repo.join("wf")).unwrap();
    let p = f.repo.join("wf/ci.yml");
    for tool in ["Write", "Edit"] {
        let out = f.call(tool, wf_input(tool, &p), &f.repo);
        assert_gha_ask(&out, &format!("{tool} on wf/ci.yml via dir symlink"));
    }
    let out = f.run("echo x > wf/ci.yml");
    assert_gha_ask(&out, "`echo x > wf/ci.yml` via dir symlink");
}

#[test]
fn w_symlinked_file_file_tools_ask() {
    let f = Fx::new("w12");
    f.git(&["init", "-q", "-b", "main"]);
    symlink_repo(&f.repo);
    std::os::unix::fs::symlink(".github/workflows/ci.yml", f.repo.join("link.yml")).unwrap();
    let p = f.repo.join("link.yml");
    for tool in ["Write", "Edit"] {
        let out = f.call(tool, wf_input(tool, &p), &f.repo);
        assert_gha_ask(&out, &format!("{tool} on link.yml via file symlink"));
    }
}

#[test]
fn w_symlink_chain_file_tools_ask() {
    let f = Fx::new("w13");
    f.git(&["init", "-q", "-b", "main"]);
    symlink_repo(&f.repo);
    std::os::unix::fs::symlink(".github/workflows", f.repo.join("b")).unwrap();
    std::os::unix::fs::symlink("b", f.repo.join("a")).unwrap();
    let p = f.repo.join("a/ci.yml");
    let out = f.call("Write", wf_input("Write", &p), &f.repo);
    assert_gha_ask(&out, "Write on a/ci.yml via a -> b -> .github/workflows");
}

#[test]
fn w_symlinked_dir_in_git_worktree_asks() {
    let f = Fx::new("w14");
    f.git(&["init", "-q", "-b", "main"]);
    std::fs::write(f.repo.join("a.txt"), "x\n").unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    let wt = f.base.join("wt1");
    f.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"]);
    symlink_repo(&wt);
    std::os::unix::fs::symlink(".github/workflows", wt.join("wf")).unwrap();
    let p = wt.join("wf/ci.yml");
    for tool in ["Write", "Edit"] {
        let out = f.call(tool, wf_input(tool, &p), &wt);
        assert_gha_ask(&out, &format!("{tool} in worktree via dir symlink"));
    }
    let out = f.call(
        "Bash",
        serde_json::json!({ "command": "echo x > wf/ci.yml" }),
        &wt,
    );
    assert_gha_ask(&out, "`echo x > wf/ci.yml` in worktree via dir symlink");
}

#[test]
fn w_symlink_to_unrelated_dir_is_not_a_workflow_write() {
    let f = Fx::new("w15");
    f.git(&["init", "-q", "-b", "main"]);
    symlink_repo(&f.repo);
    std::fs::create_dir_all(f.repo.join("docs")).unwrap();
    std::os::unix::fs::symlink("docs", f.repo.join("docs2")).unwrap();
    let p = f.repo.join("docs2/x.md");
    let out = f.call("Write", wf_input("Write", &p), &f.repo);
    assert!(
        !out.to_lowercase().contains("github actions"),
        "symlink to unrelated dir must not carry the GitHub Actions reason; got {out:?}"
    );
}

// Symlink spellings of blastguard's protected gate/config paths (user ruling
// 2026-11-01): `.claude/settings.json` and `.githooks/<file>` must Ask with the
// same protected-path reason as a direct write, through dir symlinks, file
// symlinks and chains. CLAUDE.md stays unprotected. Each spelling x tool is
// its own test so a RED run shows every case.
fn assert_protected_ask(out: &str, what: &str) {
    assert_eq!(decision(out), "ask", "{what} must Ask; got {out:?}");
    assert!(
        out.contains("protected gate/config path"),
        "{what}: reason must be the protected-path reason; got {out:?}"
    );
}

fn protected_repo(f: &Fx) {
    f.git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(f.repo.join(".claude")).unwrap();
    std::fs::write(f.repo.join(".claude/settings.json"), "{}\n").unwrap();
    std::fs::create_dir_all(f.repo.join(".githooks")).unwrap();
    std::fs::write(f.repo.join(".githooks/pre-commit"), "x\n").unwrap();
}

fn protected_case(name: &str, tool: &str, rel: &str, links: &[(&str, &str)]) {
    let f = Fx::new(name);
    protected_repo(&f);
    for (target, link) in links {
        std::os::unix::fs::symlink(target, f.repo.join(link)).unwrap();
    }
    let what = format!("{tool} on {rel}");
    let out = if tool == "Bash" {
        f.run(&format!("echo x > {rel}"))
    } else {
        let p = f.repo.join(rel);
        f.call(tool, wf_input(tool, &p), &f.repo)
    };
    assert_protected_ask(&out, &what);
}

macro_rules! protected_tests {
    ($($test:ident: $name:literal, $rel:literal, [$(($t:literal, $l:literal)),*];)*) => {$(
        mod $test {
            use super::*;
            #[test]
            fn write() { protected_case(concat!($name, "w"), "Write", $rel, &[$(($t, $l)),*]); }
            #[test]
            fn edit() { protected_case(concat!($name, "e"), "Edit", $rel, &[$(($t, $l)),*]); }
            #[test]
            fn bash() { protected_case(concat!($name, "b"), "Bash", $rel, &[$(($t, $l)),*]); }
        }
    )*};
}

protected_tests! {
    p_direct_settings: "p1", ".claude/settings.json", [];
    p_direct_githooks: "p2", ".githooks/pre-commit", [];
    p_dir_symlink_settings: "p3", "cl/settings.json", [(".claude", "cl")];
    p_dir_symlink_githooks: "p4", "gh2/pre-commit", [(".githooks", "gh2")];
    p_file_symlink_settings: "p5", "st", [(".claude/settings.json", "st")];
    p_file_symlink_githooks: "p6", "hk", [(".githooks/pre-commit", "hk")];
    p_chain_settings: "p7", "ca/settings.json", [(".claude", "cb"), ("cb", "ca")];
    p_chain_githooks: "p8", "ga/pre-commit", [(".githooks", "gb"), ("gb", "ga")];
}

// Fail-closed: an unresolvable path (symlink loop) is "cannot determine" and
// must Ask, never Allow.
#[test]
fn p_symlink_loop_write_asks() {
    let f = Fx::new("p9");
    protected_repo(&f);
    std::os::unix::fs::symlink("loop2", f.repo.join("loop1")).unwrap();
    std::os::unix::fs::symlink("loop1", f.repo.join("loop2")).unwrap();
    let p = f.repo.join("loop1/x");
    let out = f.call("Write", wf_input("Write", &p), &f.repo);
    assert_eq!(
        decision(&out),
        "ask",
        "Write through a symlink loop must Ask; got {out:?}"
    );
}

// ---------------------------------------------------------------------------
// Gap closure (user ruling 2026-11-01), RED tests against 0.2.178.
// ---------------------------------------------------------------------------

// (1) `cd <symlink-dir> && ... ../x`: the kernel resolves `..` AFTER following
// the symlink, so `link3/..` is the link TARGET's parent, not the cwd. A
// lexical collapse of `..` after `cd` lands on the wrong file.
fn cd_dotdot_case(name: &str, links: &[(&str, &str)], mkdirs: &[&str], cmd: &str) -> String {
    let f = Fx::new(name);
    protected_repo(&f);
    symlink_repo(&f.repo);
    for d in mkdirs {
        std::fs::create_dir_all(f.repo.join(d)).unwrap();
    }
    for (target, link) in links {
        std::os::unix::fs::symlink(target, f.repo.join(link)).unwrap();
    }
    f.run(cmd)
}

#[test]
fn q_cd_symlink_dotdot_redirect_settings_asks() {
    let out = cd_dotdot_case(
        "q1",
        &[(".claude/sub", "link3")],
        &[".claude/sub"],
        "cd link3 && echo x > ../settings.json",
    );
    assert_protected_ask(&out, "`cd link3 && echo x > ../settings.json`");
}

#[test]
fn q_cd_symlink_dotdot_append_settings_asks() {
    let out = cd_dotdot_case(
        "q2",
        &[(".claude/sub", "link3")],
        &[".claude/sub"],
        "cd link3 && echo x >> ../settings.json",
    );
    assert_protected_ask(&out, "`cd link3 && echo x >> ../settings.json`");
}

#[test]
fn q_cd_symlink_dotdot_tee_settings_asks() {
    let out = cd_dotdot_case(
        "q3",
        &[(".claude/sub", "link3")],
        &[".claude/sub"],
        "cd link3 && echo x | tee ../settings.json",
    );
    assert_protected_ask(&out, "`cd link3 && echo x | tee ../settings.json`");
}

#[test]
fn q_cd_symlink_dotdot_workflows_asks() {
    let out = cd_dotdot_case(
        "q4",
        &[(".github/workflows/sub", "link")],
        &[".github/workflows/sub"],
        "cd link && echo x > ../ci.yml",
    );
    assert_gha_ask(&out, "`cd link && echo x > ../ci.yml`");
}

// Control: a plain (non-symlink) dir. `../x` from `plain/` is `<repo>/x`,
// outside every protected path, so the protected reason must not appear.
#[test]
fn q_cd_plain_dir_dotdot_is_not_protected() {
    let out = cd_dotdot_case(
        "q5",
        &[],
        &["plain"],
        "cd plain && echo x > ../settings.json",
    );
    assert!(
        !out.contains("protected gate/config path") && !out.to_lowercase().contains("github actions"),
        "plain-dir `../settings.json` is <repo>/settings.json and must not carry a protected reason; got {out:?}"
    );
}

// (2) Dangling file symlink into a system directory. The target name cannot
// exist, and nothing here ever writes: the hook only judges the payload.
const ABSENT_SYS: &str = "/etc/blastguard-symlink2-c5b9c02b-absent.conf";

fn sys_fixture(name: &str, links: &[(&str, &str)]) -> Fx {
    assert!(
        std::fs::symlink_metadata(ABSENT_SYS).is_err(),
        "fixture: {ABSENT_SYS} must not exist"
    );
    let f = Fx::new(name);
    for (target, link) in links {
        std::os::unix::fs::symlink(target, f.repo.join(link)).unwrap();
    }
    f
}

fn assert_system_deny(out: &str, what: &str) {
    assert_eq!(decision(out), "deny", "{what} must Deny; got {out:?}");
    assert!(
        out.contains("writes into a system directory"),
        "{what}: reason must be the system-directory reason; got {out:?}"
    );
}

#[test]
fn s_control_direct_redirect_to_absent_system_file_denies() {
    let f = sys_fixture("s0", &[]);
    assert_system_deny(&f.run(&format!("echo x > {ABSENT_SYS}")), "direct `>`");
    assert_system_deny(&f.run(&format!("echo x >> {ABSENT_SYS}")), "direct `>>`");
}

#[test]
fn s_dangling_file_symlink_redirect_denies() {
    let f = sys_fixture("s1", &[(ABSENT_SYS, "sysln")]);
    assert_system_deny(&f.run("echo x > sysln"), "`echo x > sysln`");
}

#[test]
fn s_dangling_file_symlink_append_denies() {
    let f = sys_fixture("s2", &[(ABSENT_SYS, "sysln")]);
    assert_system_deny(&f.run("echo x >> sysln"), "`echo x >> sysln`");
}

#[test]
fn s_dangling_symlink_chain_redirect_denies() {
    let f = sys_fixture("s3", &[(ABSENT_SYS, "sysb"), ("sysb", "sysa")]);
    assert_system_deny(
        &f.run("echo x > sysa"),
        "`echo x > sysa` (sysa -> sysb -> /etc/..)",
    );
}

#[test]
fn s_dangling_symlink_chain_append_denies() {
    let f = sys_fixture("s4", &[(ABSENT_SYS, "sysb"), ("sysb", "sysa")]);
    assert_system_deny(
        &f.run("echo x >> sysa"),
        "`echo x >> sysa` (sysa -> sysb -> /etc/..)",
    );
}

// Write/Edit have no system-directory rule even for a DIRECT path (observed:
// `Write /etc/<absent>` is Allow on 0.2.178), so the property pinned here is
// parity: the symlink spelling must be judged exactly as the direct one is.
#[test]
fn s_dangling_file_symlink_file_tools_match_direct() {
    let f = sys_fixture("s5", &[(ABSENT_SYS, "sysln")]);
    for tool in ["Write", "Edit"] {
        let direct = f.call(tool, wf_input(tool, Path::new(ABSENT_SYS)), &f.repo);
        let via = f.call(tool, wf_input(tool, &f.repo.join("sysln")), &f.repo);
        assert_eq!(
            decision(&via),
            decision(&direct),
            "{tool} via dangling symlink must be judged like the direct path; direct={direct:?} via={via:?}"
        );
    }
}

// (3) The hop limit must be bounded. If it is not, the loop spins forever;
// this test must FAIL within a bounded time instead of hanging.
#[test]
fn p_symlink_loop_write_is_answered_within_bound() {
    let f = Fx::new("p10");
    protected_repo(&f);
    std::os::unix::fs::symlink("loop2", f.repo.join("loop1")).unwrap();
    std::os::unix::fs::symlink("loop1", f.repo.join("loop2")).unwrap();
    let p = f.repo.join("loop1/x");
    let payload = serde_json::json!({
        "session_id": "gha-workflows",
        "cwd": f.repo.display().to_string(),
        "tool_name": "Write",
        "tool_input": wf_input("Write", &p),
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("CLAUDE_PROJECT_DIR", &f.repo)
        .env("BLASTGUARD_APPROVALS_DIR", f.base.join("store"))
        .env("HOME", &f.home)
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&f.base)
        .spawn()
        .expect("binary spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("symlink loop was not answered within 30s: the hop limit is unbounded");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        decision(&String::from_utf8_lossy(&out.stdout)),
        "ask",
        "symlink loop must Ask"
    );
}
