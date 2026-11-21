#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Probe for backlog 6f299fb5: `names_landing_path` treats an empty-literal
//! word (`''`) as "does not name the heredoc landing path" so that
//! `cat > tests/a.rs <<'EOF' ... EOF` followed by `sed -i '' ...` strips the
//! heredoc body as data. Concern: the change must not let the body be stripped
//! (=> Allow) when a following command really can execute / reference the
//! heredoc-written file.
//!
//! PROVES (observed per case, one blastguard process each): for the listed
//! following lines whose target is the heredoc landing file (tests/a.rs or
//! a.sh) combined with an empty-string word, the decision with a
//! `chmod 777 /etc/hosts` body is not allow; and the control (`.rs` heredoc +
//! `sed -i '' ... Cargo.toml`) is allow.
//!
//! DOES NOT PROVE: any other empty-word placement (`""`, `$''`, `\<newline>`,
//! empty word inside `eval`/`xargs`/`env` wrappers), other landing paths or
//! interpreters, that the not-allow comes from the landing-path rule rather
//! than another rule (ask vs deny is not distinguished), nor behaviour with a
//! harmful body other than `chmod 777 /etc/hosts`.
//!
//! Written by an independent test author.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-emptyword-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let proj = dir.join("proj");
    std::fs::create_dir_all(proj.join("tests")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    let st = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&proj)
        .status()
        .expect("git init must be runnable");
    assert!(st.success(), "git init failed");
    Home(dir)
}

fn run_as(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "empty-word-probe-20261121",
        "cwd": home.0.join("proj"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn assert_decision(tag: &str, command: &str, want: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(command, &home);
    assert_eq!(
        d, want,
        "{command:?} must be {want}, got decision={d} reason={r}"
    );
}

fn assert_not_allow(tag: &str, command: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(command, &home);
    assert!(
        d == "ask" || d == "deny",
        "{command:?} must be ask|deny, got decision={d} reason={r}"
    );
}

const BODY: &str = "chmod 777 /etc/hosts\n";

fn rs_heredoc() -> String {
    format!("cat > tests/a.rs <<'EOF'\n{BODY}EOF\n")
}
fn sh_heredoc() -> String {
    format!("cat > a.sh <<'EOF'\n{BODY}EOF\n")
}

macro_rules! not_allow_case {
    ($name:ident, $heredoc:expr, $follow:expr) => {
        #[test]
        fn $name() {
            let cmd = format!("{}{}", $heredoc, $follow);
            assert_not_allow(stringify!($name), &cmd);
        }
    };
}

not_allow_case!(rs_sh_empty, rs_heredoc(), "sh ''");
not_allow_case!(rs_bash_empty, rs_heredoc(), "bash ''");
not_allow_case!(rs_dot_empty, rs_heredoc(), ". ''");
not_allow_case!(rs_source_empty, rs_heredoc(), "source ''");
not_allow_case!(rs_cd_empty_sh_a, rs_heredoc(), "cd '' && sh tests/a.rs");
not_allow_case!(rs_sh_a_empty, rs_heredoc(), "sh tests/a.rs ''");
not_allow_case!(
    rs_git_c_empty_apply,
    rs_heredoc(),
    "git -C '' apply tests/a.rs"
);

not_allow_case!(sh_sh_empty, sh_heredoc(), "sh ''");
not_allow_case!(sh_bash_empty, sh_heredoc(), "bash ''");
not_allow_case!(sh_dot_empty, sh_heredoc(), ". ''");
not_allow_case!(sh_source_empty, sh_heredoc(), "source ''");
not_allow_case!(sh_cd_empty_bash_a, sh_heredoc(), "cd '' && bash a.sh");
not_allow_case!(sh_bash_a_empty, sh_heredoc(), "bash a.sh ''");
not_allow_case!(sh_sh_a_empty, sh_heredoc(), "sh a.sh ''");
not_allow_case!(sh_git_c_empty_apply, sh_heredoc(), "git -C '' apply a.sh");

#[test]
fn ctl_rs_heredoc_then_sed_empty_suffix_cargo_toml_is_allow() {
    let cmd = format!("{}sed -i '' 's/a/b/' Cargo.toml", rs_heredoc());
    assert_decision("ctl", &cmd, "allow");
}
