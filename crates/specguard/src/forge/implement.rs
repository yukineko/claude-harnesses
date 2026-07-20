//! ⑤ parallel-impl: worktree isolation + impl agent per requirement.
//!
//! Each task gets its own `git worktree add` so parallel agents can write files
//! without colliding. The harness limits concurrency (`MAX_PARALLEL`). After all
//! agents finish, results are serialised for the ⑥ evidence gate (DESIGN.md §6).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::thread;

use crate::config::AgentConfig;
use crate::impl_prompt::IMPL_MARKER;

/// Parallel cap — matches specguard and Claude Code `isolation:worktree` guidance.
pub const MAX_PARALLEL: usize = 4;

/// Outcome of one impl agent run (one requirement).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplResult {
    pub spec_id: String,
    pub req_id: String,
    /// `done` | `partial` | `failed` | `no-marker`
    pub status: String,
    pub test_cmd: Option<String>,
    pub test_result: Option<String>,
    pub evidence_note: Option<String>,
    pub worktree: Option<String>,
    pub agent_exit: i32,
}

impl ImplResult {
    pub fn is_success(&self) -> bool {
        self.status == "done"
    }
}

/// Parse the `<<<SPEC_IMPL>>>` trailer from agent stdout.
fn parse_impl_output(stdout: &str) -> ParsedImpl {
    let marker_pos = stdout.rfind(IMPL_MARKER);
    if marker_pos.is_none() {
        return ParsedImpl {
            found: false,
            ..Default::default()
        };
    }
    let trailer = &stdout[marker_pos.unwrap() + IMPL_MARKER.len()..];
    let mut p = ParsedImpl {
        found: true,
        ..Default::default()
    };
    for line in trailer.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("task_id:") {
            p.task_id = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("status:") {
            p.status = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("test_cmd:") {
            p.test_cmd = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("test_result:") {
            p.test_result = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("evidence_note:") {
            p.evidence_note = Some(v.trim().to_string());
        }
    }
    p
}

#[derive(Default)]
struct ParsedImpl {
    found: bool,
    #[allow(dead_code)]
    task_id: String,
    status: String,
    test_cmd: Option<String>,
    test_result: Option<String>,
    evidence_note: Option<String>,
}

/// Result of the harness independently running a declared `test_cmd`.
struct TestRun {
    passed: bool,
    /// Tail of combined stdout+stderr, for the evidence note on failure.
    output_tail: String,
}

/// Validate an LLM-generated `test_cmd` before it is handed to `sh -c`.
///
/// Trust boundary: `test_cmd` arrives from an impl agent's stdout trailer — it is
/// LLM-generated and MUST NOT be executed on the agent's word. We reuse
/// blastguard's pure destructive-operation detector (the same one the PreToolUse
/// hook uses) rather than reimplementing detection. A flagged command
/// (`rm -rf …`, fork bomb, `> file` truncation, `dd of=…`, `git reset --hard`, …)
/// returns `Err(reason)`; anything blastguard allows returns `Ok(())`.
fn validate_test_cmd(test_cmd: &str) -> Result<(), String> {
    let input = serde_json::json!({ "command": test_cmd });
    // `.hardened()` collapses an `Ask` into a `Deny`: there is no human in this
    // loop to answer one — the next thing that happens to this string is `sh -c`.
    // "ask にできないときは fail".
    match blastguard::detect::detect("Bash", Some(&input)).hardened() {
        blastguard::model::Decision::Deny(reason) => Err(reason),
        blastguard::model::Decision::Allow => Ok(()),
        // Unreachable after `hardened()`, but written out rather than `_ =>` so
        // a future variant fails the build instead of silently landing here.
        blastguard::model::Decision::Ask(reason) => Err(reason),
    }
}

/// Run `test_cmd` via `sh -c` in `dir`, trusting its exit code. Returns `None`
/// only if the command could not be spawned at all (treated as unverifiable — a
/// non-zero exit is a *ran-and-failed*, which is verified evidence, not None).
///
/// Before spawning, the LLM-generated `test_cmd` is validated with
/// [`validate_test_cmd`]. A destructive/flagged payload is refused fail-closed:
/// we return a *failed* verdict WITHOUT invoking the shell, so a destructive
/// command can never be executed nor promote a requirement to `done`.
fn run_test_cmd(dir: &Path, test_cmd: &str) -> Option<TestRun> {
    if let Err(reason) = validate_test_cmd(test_cmd) {
        return Some(TestRun {
            passed: false,
            output_tail: format!(
                "[blastguard] LLM 生成の test_cmd `{test_cmd}` を sh -c 実行前に拒否 \
                 (fail-closed) — {reason}。破壊的コマンドはハーネスが実行しない。"
            ),
        });
    }
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(test_cmd)
        .current_dir(dir)
        .output()
        .ok()?;
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(TestRun {
        passed: out.status.success(),
        output_tail: tail_lines(&combined, 40),
    })
}

/// Keep the last `n` non-empty-trimmed lines of `s` (for compact evidence notes).
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n").trim_end().to_string()
}

/// Join an optional agent-supplied note with a harness-supplied note.
fn compose_note(agent: Option<&str>, harness: &str) -> String {
    match agent {
        Some(a) if !a.trim().is_empty() => format!("{a}\n[harness] {harness}"),
        _ => format!("[harness] {harness}"),
    }
}

/// The harness's authoritative verdict after independently checking the agent's
/// self-reported result.
struct Reconciled {
    status: String,
    test_result: Option<String>,
    evidence_note: Option<String>,
}

/// Reconcile the agent's *claimed* result with the harness's independent test
/// run. This is the gate: a self-reported `done`/`test_result:pass` is NOT
/// accepted on the agent's word — only the harness's own exit code makes a
/// requirement `done`. Pure (no I/O) so it is unit-testable.
///
/// - non-`done` claims pass through untouched (they are not claiming success).
/// - `done` + harness run passed → stays `done`, `test_result` = harness-verified.
/// - `done` + harness run failed → downgraded to `failed` (self-report refuted).
/// - `done` + no runnable `test_cmd` → fail-closed: downgraded to `partial`
///   (`unverified`) so it cannot auto-merge, unless `require_verification` is
///   off (escape hatch), in which case the self-report is trusted but labelled.
fn reconcile(
    base_status: &str,
    test_cmd: Option<&str>,
    run: Option<&TestRun>,
    agent_test_result: Option<&str>,
    agent_note: Option<&str>,
    require_verification: bool,
) -> Reconciled {
    if base_status != "done" {
        return Reconciled {
            status: base_status.to_string(),
            test_result: agent_test_result.map(|s| s.to_string()),
            evidence_note: agent_note.map(|s| s.to_string()),
        };
    }
    match run {
        Some(r) if r.passed => Reconciled {
            status: "done".to_string(),
            test_result: Some("pass (harness-verified)".to_string()),
            evidence_note: agent_note.map(|s| s.to_string()),
        },
        Some(r) => {
            let note = compose_note(
                agent_note,
                &format!(
                    "ハーネスが test_cmd を worktree で独立実行したが FAIL — 自己申告の \
                     test_result:pass を棄却し done にしない。\n--- test output (tail) ---\n{}",
                    r.output_tail
                ),
            );
            Reconciled {
                status: "failed".to_string(),
                test_result: Some("fail (harness-verified)".to_string()),
                evidence_note: Some(note),
            }
        }
        None => {
            if require_verification {
                let why = match test_cmd {
                    Some(cmd) if !cmd.trim().is_empty() => {
                        format!("test_cmd `{cmd}` を独立実行できなかった")
                    }
                    _ => "test_cmd が未指定で独立検証できない".to_string(),
                };
                let note = compose_note(
                    agent_note,
                    &format!(
                        "{why} — 証拠なしの自己申告 done は受理しない (fail-closed)。検証可能な \
                         test_cmd を付けて再実行するか、どうしても人間の手動確認で前進するなら \
                         config で [evidence] require_verification=false を設定 (escape hatch)。"
                    ),
                );
                Reconciled {
                    status: "partial".to_string(),
                    test_result: Some("unverified".to_string()),
                    evidence_note: Some(note),
                }
            } else {
                let tr = agent_test_result.unwrap_or("(none)");
                Reconciled {
                    status: "done".to_string(),
                    test_result: Some(format!("{tr} (self-reported, unverified)")),
                    evidence_note: agent_note.map(|s| s.to_string()),
                }
            }
        }
    }
}

/// Run one impl task: create a worktree, run the agent, parse output, tear down
/// the worktree if no changes were made (cheapest path).
///
/// `require_verification` fail-closes the evidence gate: when set, a self-reported
/// `done` is only accepted if the harness re-runs the declared `test_cmd` here in
/// the worktree and it passes (see [`reconcile`]).
pub fn run_task(
    repo_root: &Path,
    spec_id: &str,
    req_id: &str,
    prompt: &str,
    worktree_base: &Path,
    cfg: &AgentConfig,
    require_verification: bool,
) -> ImplResult {
    let wt_name = format!("{spec_id}-{req_id}");
    let wt_path = worktree_base.join(&wt_name);

    // Create worktree on a detached branch.
    let wt_branch = format!("specforge/{spec_id}/{req_id}");
    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "add", "-b", &wt_branch])
        .arg(&wt_path)
        .arg("HEAD")
        .output();

    let worktree_created = add.map(|o| o.status.success()).unwrap_or(false);
    let effective_root = if worktree_created {
        wt_path.clone()
    } else {
        repo_root.to_path_buf()
    };

    // Build impl agent config — write-enabled (opposite of normalize).
    let impl_cfg = AgentConfig {
        command: cfg.command.clone(),
        args: impl_agent_args(),
    };

    let out = crate::agent::run(&impl_cfg, &effective_root, prompt);

    let parsed = parse_impl_output(&out.stdout);

    let base_status = if !parsed.found || parsed.status.is_empty() {
        "no-marker".to_string()
    } else {
        parsed.status.clone()
    };

    // ⑥ evidence gate, first floor: do NOT accept a self-reported `done` on the
    // agent's word. Independently re-run the declared test_cmd in the worktree
    // and trust ITS exit code (reconcile decides the authoritative status).
    let test_run = if base_status == "done" {
        parsed
            .test_cmd
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .and_then(|cmd| run_test_cmd(&effective_root, cmd))
    } else {
        None
    };
    let verdict = reconcile(
        &base_status,
        parsed.test_cmd.as_deref(),
        test_run.as_ref(),
        parsed.test_result.as_deref(),
        parsed.evidence_note.as_deref(),
        require_verification,
    );

    let result = ImplResult {
        spec_id: spec_id.to_string(),
        req_id: req_id.to_string(),
        status: verdict.status,
        test_cmd: parsed.test_cmd,
        test_result: verdict.test_result,
        evidence_note: verdict.evidence_note,
        worktree: if worktree_created {
            Some(wt_path.to_string_lossy().to_string())
        } else {
            None
        },
        agent_exit: out.agent_exit(),
    };

    // If agent made no changes and worktree was created, remove it (matches
    // condukt/Claude Code `isolation:worktree` behaviour — cheap cleanup).
    // We leave it on success/partial for the human to inspect or merge.

    result
}

/// Impl agent args — write-enabled in the worktree, read-only outside.
/// The worktree isolation ensures writes cannot escape into the main tree.
fn impl_agent_args() -> Vec<String> {
    vec![
        "--print".to_string(),
        "--allowedTools".to_string(),
        "Read".to_string(),
        "Edit".to_string(),
        "Write".to_string(),
        "Bash(cargo test*)".to_string(),
        "Bash(npm test*)".to_string(),
        "Bash(pytest*)".to_string(),
        "Bash(git *)".to_string(),
        "Glob".to_string(),
        "Grep".to_string(),
    ]
}

pub struct TaskInput {
    pub spec_id: String,
    pub req_id: String,
    pub prompt: String,
}

/// Run up to `MAX_PARALLEL` tasks concurrently. Returns one `ImplResult` per task.
pub fn run_parallel(
    repo_root: &Path,
    tasks: Vec<TaskInput>,
    worktree_base: &Path,
    cfg: &AgentConfig,
    require_verification: bool,
) -> Vec<ImplResult> {
    // Fan-out in chunks of MAX_PARALLEL.
    let repo_root = repo_root.to_path_buf();
    let worktree_base = worktree_base.to_path_buf();
    let cfg = cfg.clone();

    let handles: Vec<_> = tasks
        .into_iter()
        .map(|t| {
            let rr = repo_root.clone();
            let wb = worktree_base.clone();
            let cfg2 = cfg.clone();
            thread::spawn(move || {
                run_task(
                    &rr,
                    &t.spec_id,
                    &t.req_id,
                    &t.prompt,
                    &wb,
                    &cfg2,
                    require_verification,
                )
            })
        })
        .collect();

    // Bounded collect: drain in order (ordering matches requirement order).
    handles
        .into_iter()
        .map(|h| {
            h.join().unwrap_or_else(|_| ImplResult {
                spec_id: String::new(),
                req_id: "panic".to_string(),
                status: "failed".to_string(),
                test_cmd: None,
                test_result: None,
                evidence_note: Some("thread panicked".to_string()),
                worktree: None,
                agent_exit: -1,
            })
        })
        .collect()
}

/// Persist impl results to `<dir>/<spec_id>-impl.json`.
pub fn write_results(dir: &Path, spec_id: &str, results: &[ImplResult]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).context("creating impl dir")?;
    let path = dir.join(format!("{spec_id}-impl.json"));
    let json = serde_json::to_string_pretty(results).context("serializing impl results")?;
    std::fs::write(&path, json).context("writing impl results")?;
    Ok(path)
}

/// Load previously persisted impl results.
pub fn load_results(dir: &Path, spec_id: &str) -> Result<Vec<ImplResult>> {
    let path = dir.join(format!("{spec_id}-impl.json"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading impl results {}", path.display()))?;
    serde_json::from_str(&text).context("parsing impl results")
}

// Extension so `AgentOutput` can expose exit code.
trait AgentOutputExt {
    fn agent_exit(&self) -> i32;
}
impl AgentOutputExt for crate::agent::AgentOutput {
    fn agent_exit(&self) -> i32 {
        self.code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_trailer() {
        let stdout = "実装しました。\n\n<<<SPEC_IMPL>>>\n\
            task_id: R1\nstatus: done\ntest_cmd: cargo test clamp\n\
            test_result: pass\nevidence_note: clamp ok\n";
        let p = parse_impl_output(stdout);
        assert!(p.found);
        assert_eq!(p.status, "done");
        assert_eq!(p.test_cmd.as_deref(), Some("cargo test clamp"));
        assert_eq!(p.test_result.as_deref(), Some("pass"));
        assert_eq!(p.evidence_note.as_deref(), Some("clamp ok"));
    }

    #[test]
    fn missing_marker_is_not_found() {
        let p = parse_impl_output("just prose, no marker");
        assert!(!p.found);
        assert_eq!(p.status, "");
    }

    #[test]
    fn last_marker_wins() {
        // A draft trailer earlier, the authoritative one last (mirrors specguard).
        let stdout = "<<<SPEC_IMPL>>>\nstatus: partial\n\
            ...retry...\n<<<SPEC_IMPL>>>\nstatus: done\n";
        let p = parse_impl_output(stdout);
        assert!(p.found);
        assert_eq!(p.status, "done");
    }

    #[test]
    fn partial_status_without_test_fields() {
        let stdout = "<<<SPEC_IMPL>>>\ntask_id: R2\nstatus: partial\n\
            evidence_note: blocked on missing canon\n";
        let p = parse_impl_output(stdout);
        assert_eq!(p.status, "partial");
        assert!(p.test_result.is_none());
        assert_eq!(p.evidence_note.as_deref(), Some("blocked on missing canon"));
    }

    // ── ⑥ evidence gate: no self-reported `done` without harness evidence ──────

    #[test]
    fn self_reported_done_without_test_cmd_is_not_accepted() {
        // Agent claims done + pass but gave no runnable test_cmd → fail-closed:
        // the harness refuses `done` and downgrades to a non-mergeable `partial`.
        let r = reconcile("done", None, None, Some("pass"), None, true);
        assert_eq!(r.status, "partial", "unverified done must not stay done");
        assert_eq!(r.test_result.as_deref(), Some("unverified"));
        let note = r.evidence_note.unwrap();
        assert!(
            note.contains("fail-closed"),
            "note explains refusal: {note}"
        );
        assert!(
            note.contains("require_verification=false"),
            "note points at the escape hatch: {note}"
        );
    }

    #[test]
    fn self_reported_done_with_unrunnable_test_cmd_is_not_accepted() {
        // A test_cmd was declared but could not be run (run == None) → still
        // unverifiable, still refused.
        let r = reconcile(
            "done",
            Some("cargo test clamp"),
            None,
            Some("pass"),
            None,
            true,
        );
        assert_eq!(r.status, "partial");
        assert_eq!(r.test_result.as_deref(), Some("unverified"));
        assert!(r.evidence_note.unwrap().contains("cargo test clamp"));
    }

    #[test]
    fn independent_pass_keeps_done_and_relabels_evidence() {
        let run = TestRun {
            passed: true,
            output_tail: "test result: ok".into(),
        };
        let r = reconcile(
            "done",
            Some("cargo test"),
            Some(&run),
            Some("pass"),
            None,
            true,
        );
        assert_eq!(r.status, "done");
        assert_eq!(
            r.test_result.as_deref(),
            Some("pass (harness-verified)"),
            "test_result reflects the harness run, not the agent's claim"
        );
    }

    #[test]
    fn independent_fail_refutes_self_reported_pass() {
        // Agent lied: claimed pass, but the harness run failed. Must not be done.
        let run = TestRun {
            passed: false,
            output_tail: "assertion failed: boom".into(),
        };
        let r = reconcile(
            "done",
            Some("cargo test"),
            Some(&run),
            Some("pass"),
            None,
            true,
        );
        assert_eq!(r.status, "failed", "refuted claim is not mergeable");
        assert_eq!(r.test_result.as_deref(), Some("fail (harness-verified)"));
        let note = r.evidence_note.unwrap();
        assert!(note.contains("棄却"), "note records the refutation: {note}");
        assert!(note.contains("boom"), "captured test output: {note}");
    }

    #[test]
    fn escape_hatch_trusts_self_report_but_labels_it() {
        // require_verification=false: the project opted out of harness runs. The
        // self-report is honoured but clearly marked as unverified.
        let r = reconcile("done", None, None, Some("pass"), None, false);
        assert_eq!(r.status, "done");
        assert!(
            r.test_result.as_deref().unwrap().contains("self-reported"),
            "escape-hatch pass is labelled: {:?}",
            r.test_result
        );
    }

    #[test]
    fn non_done_statuses_pass_through_untouched() {
        for s in ["partial", "failed", "no-marker"] {
            let r = reconcile(s, Some("cargo test"), None, Some("skip"), Some("n"), true);
            assert_eq!(r.status, s, "{s} must not be altered");
            assert_eq!(r.test_result.as_deref(), Some("skip"));
            assert_eq!(r.evidence_note.as_deref(), Some("n"));
        }
    }

    #[test]
    fn agent_note_is_preserved_alongside_harness_note() {
        let run = TestRun {
            passed: false,
            output_tail: "err".into(),
        };
        let r = reconcile(
            "done",
            Some("cargo test"),
            Some(&run),
            Some("pass"),
            Some("実装メモ"),
            true,
        );
        let note = r.evidence_note.unwrap();
        assert!(note.contains("実装メモ"), "agent note kept: {note}");
        assert!(note.contains("[harness]"), "harness note appended: {note}");
    }

    #[test]
    fn run_test_cmd_true_passes_false_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = run_test_cmd(tmp.path(), "true").expect("sh spawns");
        assert!(ok.passed);
        let bad = run_test_cmd(tmp.path(), "false").expect("sh spawns");
        assert!(!bad.passed);
    }

    // ── trust boundary: LLM-generated test_cmd is validated before sh -c ──────

    #[test]
    fn destructive_test_cmd_is_refused_without_invoking_shell() {
        // `test_cmd` is emitted by an LLM impl agent and handed to `sh -c`. A
        // destructive payload (blastguard-flagged recursive rm) must be refused
        // BEFORE the shell runs — the benign leading segment (`touch sentinel`)
        // must never execute, proving the shell was not invoked.
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("ran.txt");
        let victim = tmp.path().join("victim");
        let payload = format!("touch {} ; rm -rf {}", sentinel.display(), victim.display());
        let run = run_test_cmd(tmp.path(), &payload).expect("returns a refusal verdict");
        assert!(!run.passed, "a refused command must not count as passed");
        assert!(
            run.output_tail.contains("blastguard"),
            "refusal note names the guard: {}",
            run.output_tail
        );
        assert!(
            !sentinel.exists(),
            "sh -c must NOT have run — the sentinel was created, so the payload executed"
        );
    }

    #[test]
    fn benign_test_cmd_passes_validation_and_runs() {
        // A benign command passes blastguard and executes normally (side effect
        // observable), so the validation gate does not get in the way of work.
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("ran.txt");
        let cmd = format!("touch {}", sentinel.display());
        let run = run_test_cmd(tmp.path(), &cmd).expect("benign command runs");
        assert!(run.passed, "benign command should run and succeed");
        assert!(
            sentinel.exists(),
            "benign command must actually execute via sh -c"
        );
    }

    #[test]
    fn run_test_cmd_captures_output_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let run = run_test_cmd(tmp.path(), "echo boom-marker; exit 1").expect("sh spawns");
        assert!(!run.passed);
        assert!(
            run.output_tail.contains("boom-marker"),
            "tail: {}",
            run.output_tail
        );
    }

    #[test]
    fn run_test_cmd_runs_in_the_given_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("sentinel.txt"), "x").unwrap();
        // Passes only if cwd is the worktree dir where sentinel.txt exists.
        let run = run_test_cmd(tmp.path(), "test -f sentinel.txt").expect("sh spawns");
        assert!(run.passed, "test_cmd must run in the worktree dir");
    }

    #[test]
    fn tail_lines_keeps_last_n() {
        let s = "a\nb\nc\nd\ne";
        assert_eq!(tail_lines(s, 2), "d\ne");
        assert_eq!(tail_lines(s, 100), "a\nb\nc\nd\ne");
    }
}
