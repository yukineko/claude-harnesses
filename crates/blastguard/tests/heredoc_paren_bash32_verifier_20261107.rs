// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent-verifier RED tests (fix/bg32-heredoc-paren, blastguard 0.2.305).
//! Every shape below was OBSERVED on this machine (macOS, /bin/bash 3.2.57,
//! /bin/sh, /bin/zsh; 2026-11-07) with a harmless `touch M` in a fresh temp
//! dir in place of the payload; the hook is then driven with the SAME text
//! carrying `rm -rf /etc` (JSON data, never executed). "RAN" = M created.
//! Outer command `echo`. U = unquoted `$(cat <<'EOF'` ... `EOF` NL `)`.
//!
//! ```text
//! shape (body lines between <<'EOF' and EOF; P = payload)     bash  sh   zsh   hook(0.2.305, a97dfe23)
//! A1 `x) #e"` NL P                                            RAN   RAN  -     allow / allow
//! A2 `x) #e'` NL P                                            RAN   RAN  -     allow / allow
//! A3 `)\'a`  NL P                                             RAN   RAN  -     allow / allow
//! A4 `b)\"`  NL P                                             RAN   RAN  -     allow / allow
//! A5 `x) #')` NL P                                            RAN   RAN  -     allow / allow
//! A6 `x) #")` NL P                                            RAN   RAN  -     allow / allow
//! A7 backtick outer: `x` + backtick ` #e"` NL P               RAN   RAN  RAN   allow / allow
//! A8 `x)<(P` (process substitution right after the close)     RAN   -    -     allow / allow
//! B1 `echo $(echo x) #e"` NL P   (NO heredoc at all)           RAN   RAN  RAN   allow / allow
//! B2 `echo $(echo x) \'a` NL P  (NO heredoc at all)           RAN   RAN  RAN   allow / allow
//! B3 `echo $HOME #e"` NL P       (NO substitution, NO heredoc) RAN   RAN  RAN   allow / allow
//! B4 `echo <(P)`                 (process substitution only)   RAN   -    RAN   allow / allow
//! C1 unquoted `<<EOF` body `)'` NL backtick-P-backtick        -     -    RAN   allow / allow
//! C2 unquoted `<<EOF` body `)'` NL `$(P)`                     -     -    RAN   allow / allow
//! D1 U body `x # it's` NL `hi) ; P`                           RAN   RAN  -     ask   / ask
//! D2 U body `x # say "hi` NL `hi) ; P`                        RAN   RAN  -     ask   / ask
//! D3 U body `x # a`b` NL `hi) ; P` (backtick)                 RAN   RAN  -     ask   / ask
//! ```
//!
//! A* are the same bash 3.2 divergence the fix targets (the body `)` closes
//! the substitution early) where the text AFTER the close holds an
//! unbalanced/escaped quote or a `#` comment with a quote, or `<(`; the new
//! reading Closes at `)` correctly but the outer analysis then misses the next
//! line. B* show the root cause is NOT heredoc-specific: the same hole exists
//! with no heredoc (a quote left open / escaped on a line that also holds an
//! expansion hides the NEXT line from the analysis), pre-existing at a97dfe23.
//! C* are zsh-only (expansion in an unquoted heredoc body is live in zsh;
//! bash/sh error on the lone `'`), pre-existing. D* are ASK where the target
//! is harmful and DENY is wanted (lesser).
//!
//! Hook binary is the REAL one, driven against a fixture repo.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

fn write(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    /// Primary working tree, on `main`; also CLAUDE_PROJECT_DIR.
    repo: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("heredoc_paren_bash32_verifier_20261107")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(base.join("victim")).unwrap();

        git(&repo, &["init", "-q"]);
        write(&repo.join("tracked.txt"), "one\n");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        // A staged change, as a real commit would have.
        write(&repo.join("tracked.txt"), "two\n");
        git(&repo, &["add", "tracked.txt"]);

        Fixture { base, home, repo }
    }

    /// `git -C <repo> commit` prefix.
    #[allow(dead_code)]
    fn commit(&self) -> String {
        format!("git -C {} commit", self.repo.display())
    }

    /// Hook stdout for a Bash `command` (cwd = CLAUDE_PROJECT_DIR = primary).
    fn bash(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "heredoc-paren-bash32",
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `"allow"` (empty stdout), `"deny"`, `"ask"`, or `"other"`.
fn verdict(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "other"
    }
}

/// Run every `(label, command, wanted)` case and fail once, listing every
/// mismatch BY LABEL. `wanted` is `allow`, `ask`, or `not-allow` (ask or deny).
#[track_caller]
fn check_all(f: &Fixture, cases: &[(&str, String, &str)]) {
    let mut bad = Vec::new();
    for (label, cmd, want) in cases {
        let out = f.bash(cmd);
        let got = verdict(&out);
        let ok = match *want {
            "not-allow" => got != "allow",
            w => got == w,
        };
        if !ok {
            bad.push(format!(
                "  [{label}] want {want}, got {got} ({})\n    command: {cmd:?}",
                out.trim()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} cases have the wrong verdict:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

fn run_all(name: &str, tmpl_cases: &[(&str, &str)], want: &str) {
    let f = Fixture::new(name);
    let cases: Vec<(&str, String, &str)> = tmpl_cases
        .iter()
        .map(|(l, t)| (*l, t.replace("{P}", "rm -rf /etc"), want))
        .collect();
    check_all(&f, &cases);
}

#[test]
fn red_a_text_after_early_close_hides_next_line() {
    run_all(
        "a",
        &[
            ("A1 comment+dq", "echo $(cat <<'EOF'\nx) #e\"\n{P}\nEOF\n)"),
            ("A2 comment+sq", "echo $(cat <<'EOF'\nx) #e'\n{P}\nEOF\n)"),
            ("A3 escaped sq", "echo $(cat <<'EOF'\n)\\'a\n{P}\nEOF\n)"),
            ("A4 escaped dq", "echo $(cat <<'EOF'\nb)\\\"\n{P}\nEOF\n)"),
            (
                "A5 comment sq paren",
                "echo $(cat <<'EOF'\nx) #')\n{P}\nEOF\n)",
            ),
            (
                "A6 comment dq paren",
                "echo $(cat <<'EOF'\nx) #\")\n{P}\nEOF\n)",
            ),
            (
                "A7 backtick outer",
                "echo `cat <<'EOF'\nx` #e\"\n{P}\nEOF\n`",
            ),
            (
                "A8 procsub after close",
                "echo $(cat <<'EOF'\nx)<({P}\nEOF\n)",
            ),
        ],
        "deny",
    );
}

#[test]
fn red_b_same_hole_without_any_heredoc() {
    run_all(
        "b",
        &[
            ("B1 sub + comment dq", "echo $(echo x) #e\"\n{P}"),
            ("B2 sub + escaped sq", "echo $(echo x)\\'a\n{P}"),
            ("B3 var + comment dq", "echo $HOME #e\"\n{P}"),
            ("B4 process substitution", "echo <({P})"),
        ],
        "deny",
    );
}

#[test]
fn red_c_zsh_unquoted_heredoc_expansion_after_stray_quote() {
    run_all(
        "c",
        &[
            ("C1 backtick", "echo $(cat <<EOF\n)'\n`{P}`\nEOF\n)"),
            ("C2 dollar-paren", "echo $(cat <<EOF\n)'\n$({P})\nEOF\n)"),
        ],
        "deny",
    );
}

#[test]
fn red_d_comment_with_quote_is_ask_but_deny_wanted() {
    run_all(
        "d",
        &[
            (
                "D1 apostrophe",
                "echo $(cat <<'EOF'\nx # it's\nhi) ; {P}\nEOF\n)",
            ),
            (
                "D2 dq",
                "echo $(cat <<'EOF'\nx # say \"hi\nhi) ; {P}\nEOF\n)",
            ),
            (
                "D3 backtick",
                "echo $(cat <<'EOF'\nx # a`b\nhi) ; {P}\nEOF\n)",
            ),
        ],
        "deny",
    );
}
