// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (user rulings 2026-11-14: 「静的に判定できない blastguard の実装がゴミなので、
//! それをなおす」 / 「必ず静的に判定し、判断する」).
//!
//! An expansion whose value is fixed by the command text alone must be resolved
//! statically and the command judged on the resolved value. In scope:
//! a variable assigned a literal in an earlier statement of the SAME command
//! (`X=lit;`, `X="lit";`, `export X=lit;`), its concatenations (`"$T"bin/tdd`,
//! `${W}/a`), references in later statements joined by `&&` / `;` / newline, and
//! a glob under such a resolved dir. "Judged on the resolved value" is asserted
//! as: the verdict equals the verdict of the same command with the value
//! written out literally, and the reason carries none of the "cannot resolve /
//! only exists at run time / cannot tell" phrasing.
//!
//! Observed against the installed binary
//! `~/.claude/plugins/cache/yukineko/blastguard/0.2.588/bin/blastguard`
//! (2026-11-14), main thread / subagent (payload with `agent_id`):
//! - `W=<wt>; sed -i '' '4s/a/b/' $W/crates/condukt/Cargo.toml` -> ask / deny:
//!   "`sed` rewrites in place `$W/crates/condukt/Cargo.toml`, whose path depends
//!   on an expansion blastguard cannot resolve — it cannot tell whether that
//!   lands inside a system directory, so it refuses to guess". Same for `"$W"`,
//!   `${W}` with `&&`, `export W=`, newline separation. The literal path: allow.
//! - `T=<plug>/tdd/0.1.76/; "$T"bin/tdd red --cmd "..."` -> ask / deny: "the
//!   command word `"$T"bin/tdd` is an expansion whose value only exists at run
//!   time — blastguard cannot tell what program this runs". Same for
//!   `${T}bin/tdd`, `$T/bin/tdd`. The literal: allow.
//! - `B=<bin>/backlog; $B list` -> allow (already resolved: guard below), but
//!   `export B=<bin>/backlog; $B list` -> ask / deny (command word `$B`).
//! - `W=<wt>; $W/crates/condukt/tests/*.rs` (glob AS the command word) -> ask /
//!   deny "the command word `$W/crates/condukt/tests/*.rs` is an expansion
//!   whose value only exists at run time"; the literal equivalent: allow.
//!   The glob in a for-list / as an argument is NOT reported (allow): guards.
//! - `export F=/etc/hosts; sed -i '' 's/a/b/' $F` -> main ASK (cannot resolve),
//!   while `F=/etc/hosts; sed ...` is deny (system harm). The export form under-
//!   classifies system harm: RED.
//! - `python3 - <<'EOF'` with `p='<abs>'; open(p,'w')`: the literal IS resolved
//!   (the reason names the resolved path); excluded, not a reproduction.
//!
//! The subagent form of `S=<abs>; sed -i '' 's/a/b/' $S/full.py` is already RED
//! in `backlog_20261114_sed_inplace_literal_var_path_fp.rs` (backlog 574a751c);
//! here only its main-thread form is added.
//!
//! Controls (not ignored): resolved `/etc/hosts` stays deny; values that exist
//! only at run time (`$(cmd)`, unassigned, conditional assignment, assignment in
//! a subshell or a function, loop variable over `$(cmd)`, reassigned from
//! `$(cmd)`) never become allow.
//!
//! DOES NOT PROVE: verbs other than `sed -i` / command-word execution; any
//! behaviour for values from the process env; that the resolved verdicts are
//! right beyond "equal to the literal spelling".
//!
//! RED written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const UNRESOLVED_MARKERS: &[&str] = &["cannot resolve", "only exists at run time", "cannot tell"];

struct Fx {
    root: PathBuf,
}
impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Fx {
    fn wt(&self) -> String {
        self.root.join("wt").display().to_string()
    }
    /// Trailing slash on purpose: `"$T"bin/tdd` is the observed spelling.
    fn tdd_dir(&self) -> String {
        format!("{}/", self.root.join("plug/tdd/0.1.76").display())
    }
    fn backlog_bin(&self) -> String {
        self.root
            .join("plug/backlog/0.3.122/bin/backlog")
            .display()
            .to_string()
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "bg-staticvar-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let wt = root.join("wt");
    std::fs::create_dir_all(wt.join("crates/condukt/tests")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(wt.join("full.py"), "a\n").unwrap();
    std::fs::write(wt.join("crates/condukt/Cargo.toml"), "a\n").unwrap();
    std::fs::write(wt.join("crates/condukt/tests/a.rs"), "\n").unwrap();
    for bin in [
        "plug/tdd/0.1.76/bin/tdd",
        "plug/backlog/0.3.122/bin/backlog",
    ] {
        let p = root.join(bin);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    Fx { root }
}

fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let home: &Path = &fx.root.join("home");
    let mut p = serde_json::json!({
        "session_id": "static-literal-var-20261114",
        "cwd": fx.wt(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("ab0687739fbba8a8b");
        p["agent_type"] = serde_json::json!("general-purpose");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .current_dir(fx.wt())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// The variable form must be judged exactly like its literal spelling, in both
/// the main thread and a subagent, with no "cannot resolve" phrasing.
fn assert_resolved_like_literal(fx: &Fx, var_cmd: &str, literal_cmd: &str, modes: &[bool]) {
    for &sub in modes {
        let who = if sub { "subagent" } else { "main" };
        let (ld, lr) = run_as(fx, literal_cmd, sub);
        assert_eq!(
            ld, "allow",
            "anti-vacuity: literal spelling must be allow ({who}); cmd={literal_cmd:?} reason={lr}"
        );
        let (vd, vr) = run_as(fx, var_cmd, sub);
        for m in UNRESOLVED_MARKERS {
            assert!(
                !vr.contains(m),
                "({who}) the value is fixed by the command text, yet the reason says {m:?}; cmd={var_cmd:?} decision={vd} reason={vr}"
            );
        }
        assert_eq!(
            vd, ld,
            "({who}) resolved form must be judged like the literal; cmd={var_cmd:?} reason={vr}"
        );
    }
}

const BOTH: &[bool] = &[false, true];

fn sed_cargo_literal(fx: &Fx) -> String {
    format!("sed -i '' '4s/a/b/' {}/crates/condukt/Cargo.toml", fx.wt())
}

fn assert_not_allow(fx: &Fx, cmd: &str) {
    for &sub in BOTH {
        let (d, r) = run_as(fx, cmd, sub);
        assert_ne!(
            d, "allow",
            "value exists only at run time, must not be allow (subagent={sub}); cmd={cmd:?} reason={r}"
        );
    }
}

fn assert_deny(fx: &Fx, cmd: &str) {
    for &sub in BOTH {
        let (d, r) = run_as(fx, cmd, sub);
        assert_eq!(
            d, "deny",
            "resolved /etc/hosts is system harm, must be deny (subagent={sub}); cmd={cmd:?} reason={r}"
        );
    }
}

// ---------------------------------------------------------------- RED: sed -i

#[test]
#[ignore = "RED: 2026-11-14 shape 1 main thread: S=lit; sed -i $S/full.py asks 'cannot resolve' — remove when fixed"]
fn red_sed_scratch_var_main_thread() {
    let fx = fixture("s1");
    let wt = fx.wt();
    assert_resolved_like_literal(
        &fx,
        &format!("S={wt}; sed -i '' 's/a/b/' $S/full.py"),
        &format!("sed -i '' 's/a/b/' {wt}/full.py"),
        &[false],
    );
}

#[test]
#[ignore = "RED: 2026-11-14 shape 2: W=lit; sed -i $W/... is 'cannot resolve' — remove when fixed"]
fn red_sed_worktree_var_semicolon() {
    let fx = fixture("s2");
    let lit = sed_cargo_literal(&fx);
    assert_resolved_like_literal(
        &fx,
        &format!(
            "W={}; sed -i '' '4s/a/b/' $W/crates/condukt/Cargo.toml",
            fx.wt()
        ),
        &lit,
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 W=\"lit\"; sed -i \"$W\"/... is 'cannot resolve' — remove when fixed"]
fn red_sed_quoted_assignment_and_quoted_ref() {
    let fx = fixture("s2q");
    let lit = sed_cargo_literal(&fx);
    assert_resolved_like_literal(
        &fx,
        &format!(
            "W=\"{}\"; sed -i '' '4s/a/b/' \"$W\"/crates/condukt/Cargo.toml",
            fx.wt()
        ),
        &lit,
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 W=lit && sed -i ${W}/... is 'cannot resolve' — remove when fixed"]
fn red_sed_brace_ref_after_and_and() {
    let fx = fixture("s2b");
    let lit = sed_cargo_literal(&fx);
    assert_resolved_like_literal(
        &fx,
        &format!(
            "W={} && sed -i '' '4s/a/b/' ${{W}}/crates/condukt/Cargo.toml",
            fx.wt()
        ),
        &lit,
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 export W=lit; sed -i $W/... is 'cannot resolve' — remove when fixed"]
fn red_sed_export_assignment() {
    let fx = fixture("s2e");
    let lit = sed_cargo_literal(&fx);
    assert_resolved_like_literal(
        &fx,
        &format!(
            "export W={}; sed -i '' '4s/a/b/' $W/crates/condukt/Cargo.toml",
            fx.wt()
        ),
        &lit,
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 W=lit<newline>sed -i $W/... is 'cannot resolve' — remove when fixed"]
fn red_sed_newline_separated() {
    let fx = fixture("s2n");
    let lit = sed_cargo_literal(&fx);
    assert_resolved_like_literal(
        &fx,
        &format!(
            "W={}\nsed -i '' '4s/a/b/' $W/crates/condukt/Cargo.toml",
            fx.wt()
        ),
        &lit,
        BOTH,
    );
}

// ------------------------------------------------------- RED: command word

fn tdd_literal(fx: &Fx) -> String {
    format!(
        "{}bin/tdd red --cmd \"cargo test -p x --test y\"",
        fx.tdd_dir()
    )
}

#[test]
#[ignore = "RED: 2026-11-14 shape 3: T=lit; \"$T\"bin/tdd is 'command word ... only exists at run time' — remove when fixed"]
fn red_command_word_quoted_concat() {
    let fx = fixture("t3");
    let lit = tdd_literal(&fx);
    assert_resolved_like_literal(
        &fx,
        &format!(
            "T={}; \"$T\"bin/tdd red --cmd \"cargo test -p x --test y\"",
            fx.tdd_dir()
        ),
        &lit,
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 T=lit && ${T}bin/tdd is 'command word ... only exists at run time' — remove when fixed"]
fn red_command_word_brace_concat_after_and_and() {
    let fx = fixture("t3b");
    let lit = tdd_literal(&fx);
    assert_resolved_like_literal(
        &fx,
        &format!(
            "T={} && ${{T}}bin/tdd red --cmd \"cargo test -p x --test y\"",
            fx.tdd_dir()
        ),
        &lit,
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 T=lit; $T/bin/tdd is 'command word ... only exists at run time' — remove when fixed"]
fn red_command_word_unquoted_concat() {
    let fx = fixture("t3c");
    let lit = format!(
        "{}/bin/tdd red --cmd \"cargo test -p x --test y\"",
        fx.tdd_dir()
    );
    assert_resolved_like_literal(
        &fx,
        &format!(
            "T={}; $T/bin/tdd red --cmd \"cargo test -p x --test y\"",
            fx.tdd_dir()
        ),
        &lit,
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 shape 4 (export form): export B=lit; $B list is 'command word $B ... run time' — remove when fixed"]
fn red_command_word_export_assignment() {
    let fx = fixture("b4e");
    let bl = fx.backlog_bin();
    assert_resolved_like_literal(
        &fx,
        &format!("export B={bl}; $B list"),
        &format!("{bl} list"),
        BOTH,
    );
}

#[test]
#[ignore = "RED: 2026-11-14 shape 5: W=lit; $W/.../*.rs as command word is 'only exists at run time' while the literal is allow — remove when fixed"]
fn red_glob_under_resolved_dir_as_command_word() {
    let fx = fixture("g5m");
    let wt = fx.wt();
    assert_resolved_like_literal(
        &fx,
        &format!("W={wt}; $W/crates/condukt/tests/*.rs"),
        &format!("{wt}/crates/condukt/tests/*.rs"),
        BOTH,
    );
}

// ------------------------------------------------- RED: system harm via export

#[test]
#[ignore = "RED: 2026-11-14 export F=/etc/hosts; sed -i $F is ask 'cannot resolve' instead of system-harm deny — remove when fixed"]
fn red_export_etc_hosts_is_system_harm_deny() {
    let fx = fixture("c8");
    assert_deny(&fx, "export F=/etc/hosts; sed -i '' 's/a/b/' $F");
}

// --------------------------------------- guards (already resolved: stay so)

#[test]
fn guard_command_word_plain_assignment_resolves() {
    let fx = fixture("b4");
    let bl = fx.backlog_bin();
    assert_resolved_like_literal(
        &fx,
        &format!("B={bl}; $B list"),
        &format!("{bl} list"),
        BOTH,
    );
}

#[test]
fn guard_glob_in_for_list_is_not_a_command_word() {
    let fx = fixture("g5");
    let wt = fx.wt();
    assert_resolved_like_literal(
        &fx,
        &format!(
            "W={wt}; for f in $W/crates/condukt/tests/*.rs; do n=$(basename $f .rs); echo $n; done"
        ),
        &format!("for f in {wt}/crates/condukt/tests/*.rs; do n=$(basename $f .rs); echo $n; done"),
        BOTH,
    );
}

#[test]
fn guard_glob_as_argument_is_not_a_command_word() {
    let fx = fixture("g5b");
    let wt = fx.wt();
    assert_resolved_like_literal(
        &fx,
        &format!("W={wt}; ls $W/crates/condukt/tests/*.rs"),
        &format!("ls {wt}/crates/condukt/tests/*.rs"),
        BOTH,
    );
}

// ---------------------------------------------------------------- controls

#[test]
fn control_resolved_etc_hosts_stays_deny() {
    let fx = fixture("c1");
    assert_deny(&fx, "F=/etc/hosts; sed -i '' 's/a/b/' $F");
}

#[test]
fn control_command_substitution_is_not_allow() {
    let fx = fixture("c2");
    assert_not_allow(&fx, "F=$(cat x); sed -i '' 's/a/b/' $F");
}

#[test]
fn control_unassigned_is_not_allow() {
    let fx = fixture("c3");
    assert_not_allow(&fx, "sed -i '' 's/a/b/' $UNSET_VAR_XYZ/full.py");
}

#[test]
fn control_conditional_assignment_is_not_allow() {
    let fx = fixture("c4");
    let wt = fx.wt();
    assert_not_allow(
        &fx,
        &format!("[ -n \"$Z\" ] && F={wt}/full.py; sed -i '' 's/a/b/' $F"),
    );
}

#[test]
fn control_subshell_assignment_is_not_allow() {
    let fx = fixture("c5");
    let wt = fx.wt();
    assert_not_allow(&fx, &format!("(F={wt}/full.py); sed -i '' 's/a/b/' $F"));
}

#[test]
fn control_function_assignment_is_not_allow() {
    let fx = fixture("c6");
    let wt = fx.wt();
    assert_not_allow(
        &fx,
        &format!("f() {{ F={wt}/full.py; }}; sed -i '' 's/a/b/' $F"),
    );
}

#[test]
fn control_loop_variable_over_substitution_is_not_allow() {
    let fx = fixture("c7");
    assert_not_allow(&fx, "for F in $(cat list); do sed -i '' 's/a/b/' $F; done");
}

#[test]
fn control_reassigned_from_substitution_is_not_allow() {
    let fx = fixture("c10");
    let wt = fx.wt();
    assert_not_allow(
        &fx,
        &format!("F={wt}/full.py; F=$(cat x); sed -i '' 's/a/b/' $F"),
    );
}
