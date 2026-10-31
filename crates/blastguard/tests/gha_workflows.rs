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
