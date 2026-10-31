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
