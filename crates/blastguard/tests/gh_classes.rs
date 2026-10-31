// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec tests for gh handling: read-only Allow (R), remote-mutating Ask (M),
//! `gh api` method/field logic, undeterminable Ask (U). Never Deny.

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
            Path::new("/var/tmp").join(format!("bg-gh-classes-{}-{name}", std::process::id()));
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

    fn run(&self, command: &str) -> String {
        self.call(
            "Bash",
            serde_json::json!({ "command": command }),
            &self.repo,
        )
    }

    fn call(&self, tool: &str, input: serde_json::Value, cwd: &Path) -> String {
        let payload = serde_json::json!({
            "session_id": "gh-classes",
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

fn assert_allow(f: &Fx, cmds: &[&str]) {
    for cmd in cmds {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "allow", "`{cmd}` must Allow; got {out:?}");
    }
}

fn assert_ask(f: &Fx, cmds: &[&str]) {
    for cmd in cmds {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "ask", "`{cmd}` must Ask; got {out:?}");
    }
}

#[test]
fn r_read_only_groups_allow() {
    let f = Fx::new("r1");
    assert_allow(
        &f,
        &[
            "gh pr list",
            "gh pr view 12",
            "gh pr status",
            "gh pr diff 12",
            "gh pr checks 12",
            "gh issue list",
            "gh issue view 3",
            "gh issue status",
            "gh run list",
            "gh run view 9",
            "gh run watch 9",
            "gh run download 9",
            "gh workflow list",
            "gh workflow view ci.yml",
            "gh release list",
            "gh release view v1",
            "gh repo view",
            "gh label list",
            "gh gist list",
            "gh gist view abc",
            "gh cache list",
            "gh secret list",
            "gh variable list",
            "gh ruleset list",
            "gh project list",
            "gh codespace list",
            "gh org list",
        ],
    );
}

#[test]
fn r_misc_read_only_allow() {
    let f = Fx::new("r2");
    assert_allow(
        &f,
        &[
            "gh search repos foo",
            "gh search issues bar",
            "gh status",
            "gh auth status",
            "gh help",
            "gh --version",
            "gh config get editor",
            "gh config list",
            "gh extension list",
            "gh browse",
            "gh repo clone owner/repo",
        ],
    );
}

#[test]
fn r_repo_flag_forms_allow() {
    let f = Fx::new("r3");
    assert_allow(
        &f,
        &[
            "gh -R owner/repo pr list",
            "gh --repo owner/repo pr view 1",
            "gh pr list -R owner/repo",
            "gh pr view 1 --repo owner/repo",
            "gh -R owner/repo run list",
            "gh issue list --repo=owner/repo",
        ],
    );
}

#[test]
fn m_mutating_commands_ask_not_deny() {
    let f = Fx::new("m1");
    assert_ask(
        &f,
        &[
            "gh pr create --fill",
            "gh pr merge 12",
            "gh pr close 12",
            "gh pr comment 12 -b hi",
            "gh pr edit 12 --title x",
            "gh pr review 12 --approve",
            "gh issue create --title x",
            "gh issue close 3",
            "gh issue comment 3 -b hi",
            "gh repo create foo",
            "gh repo delete owner/repo --yes",
            "gh repo edit --description x",
            "gh repo fork",
            "gh release create v1",
            "gh release delete v1",
            "gh release upload v1 a.zip",
            "gh label create bug",
            "gh label delete bug",
            "gh auth login",
            "gh auth logout",
            "gh config set editor vim",
            "gh extension install owner/gh-x",
            "gh -R owner/repo pr merge 12",
            "gh pr merge 12 --repo owner/repo",
        ],
    );
}

#[test]
fn m_reason_names_what_changes() {
    let f = Fx::new("m2");
    for (cmd, needle) in [
        ("gh repo delete owner/repo --yes", "repo delete"),
        ("gh pr merge 12", "merge"),
        ("gh release create v1", "release create"),
        ("gh issue close 3", "close"),
    ] {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "ask", "`{cmd}` must Ask; got {out:?}");
        assert!(
            out.to_lowercase().contains(needle),
            "`{cmd}` reason must mention {needle:?}; got {out:?}"
        );
    }
}

#[test]
fn api_without_method_or_fields_allows() {
    let f = Fx::new("a1");
    assert_allow(
        &f,
        &[
            "gh api repos/o/r",
            "gh api repos/o/r/pulls",
            "gh api /user",
            "gh api graphql",
            "gh api repos/o/r --jq .name",
            "gh api repos/o/r --paginate",
        ],
    );
}

#[test]
fn api_explicit_get_allows_even_with_fields() {
    let f = Fx::new("a2");
    assert_allow(
        &f,
        &[
            "gh api -X GET repos/o/r",
            "gh api --method GET repos/o/r",
            "gh api -X GET search/issues -f q=foo",
            "gh api --method GET search/issues -f q=foo",
            "gh api -X GET search/issues -F per_page=5",
            "gh api -XGET repos/o/r",
            "gh api --method=GET repos/o/r",
            "gh api repos/o/r -X GET",
        ],
    );
}

#[test]
fn api_mutating_methods_ask() {
    let f = Fx::new("a3");
    assert_ask(
        &f,
        &[
            "gh api -X POST repos/o/r/issues",
            "gh api -X PUT repos/o/r/topics",
            "gh api -X PATCH repos/o/r",
            "gh api -X DELETE repos/o/r",
            "gh api --method POST repos/o/r/issues",
            "gh api --method DELETE repos/o/r",
            "gh api repos/o/r -X DELETE",
            "gh api -XDELETE repos/o/r",
            "gh api --method=PATCH repos/o/r",
        ],
    );
}

#[test]
fn api_fields_without_get_ask() {
    let f = Fx::new("a4");
    assert_ask(
        &f,
        &[
            "gh api repos/o/r/issues -f title=x",
            "gh api repos/o/r/issues -F title=x",
            "gh api repos/o/r/issues --field title=x",
            "gh api repos/o/r/issues --raw-field title=x",
            "gh api repos/o/r/issues --input body.json",
        ],
    );
}

#[test]
fn u_unknown_group_or_subcommand_asks() {
    let f = Fx::new("u1");
    assert_ask(
        &f,
        &[
            "gh frobnicate",
            "gh frobnicate list",
            "gh pr frobnicate",
            "gh pr",
            "gh issue",
            "gh repo",
            "gh api",
            "gh -R owner/repo",
            "gh -R owner/repo pr",
            "gh \"$SUB\" list",
        ],
    );
}
