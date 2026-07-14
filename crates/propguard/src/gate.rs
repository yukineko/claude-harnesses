//! The gate proper: source the task's done_criteria, derive its semantic
//! properties, gather the generated-code diff, and decide whether to block the
//! stop. Two modes:
//!
//!   * `inject` — block once per new diff state and inject the property checklist;
//!     the running subscription agent self-verifies its own code against each
//!     property (no API key, no extra process). Because the hook itself can't
//!     count how many properties actually hold, the first pass treats the diff as
//!     *unverified* (satisfied = 0), which is below any threshold ≥ 1, so it
//!     blocks; once the agent has addressed the checklist the same diff is
//!     allowed.
//!   * `subprocess` — run an independent checker over the properties + diff. The
//!     checker reports one `PROP <id>: PASS|FAIL` line per property; propguard
//!     counts the PASSes and blocks when that count is below `threshold`.
//!
//! The single place the numeric block threshold is enforced is
//! [`below_threshold`]: the stop is blocked iff `satisfied < threshold`.
//!
//! Fail-closed, but bounded and escapable. Environment errors that predate any
//! check (no git repo, no done_criteria, nothing checkable) always allow — the
//! gate never invents a finding. A checker that itself fails (crash / timeout /
//! unparseable output) does NOT allow silently: it blocks up to `max_attempts`
//! with a loud, escapable reason, then gives up loudly, so a broken checker can
//! never become a bypass. A truncated diff (unchecked tail) is treated the same
//! way. A genuine *tool* error still exits 0 via the panic guard in `main`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use globset::{Glob, GlobSetBuilder};
use wait_timeout::ChildExt;

use crate::config::{Config, Mode};
use crate::derive::{derive_properties, source_criteria, Property};

/// **The block threshold.** The stop is blocked iff fewer than `threshold` of
/// the derived properties are satisfied. This is the one enforcement point the
/// task asks for ("閾値未満でブロックする経路"): both modes route through it.
pub fn below_threshold(satisfied: usize, threshold: usize) -> bool {
    satisfied < threshold
}

/// The `already-verified` shortcut predicate. It fires — letting the next stop
/// through without re-running the check — **only** when a prior *passing* check
/// recorded this exact `(diff, properties)` hash as `last_hash`. A below-threshold
/// (genuinely failing) subprocess check records **no** passing hash (see
/// [`decide_from_count`]), so an unfixed failing diff is never auto-allowed and is
/// re-checked on the next round. Inject mode's documented "trust-after-one-block"
/// still records the hash on its first block, so it converges as before.
pub fn already_verified(st: &crate::state::SessionState, hash: &str) -> bool {
    !st.last_hash.is_empty() && st.last_hash == hash
}

/// What the gate decided. `tag` is a short label for the JSONL log.
pub enum Decision {
    Allow {
        tag: &'static str,
        attempts: u32,
        last_hash: String,
    },
    Block {
        reason: String,
        tag: &'static str,
        files: Vec<String>,
        properties: Vec<&'static str>,
        attempts: u32,
        last_hash: String,
    },
}

/// Files that changed *and* are worth checking (match include, not exclude).
pub fn checkable_files(cfg: &Config, changed: &[String]) -> Vec<String> {
    let inc = build_set(&cfg.include);
    let exc = build_set(&cfg.exclude);
    changed
        .iter()
        .filter(|f| {
            inc.as_ref().map(|s| s.is_match(f)).unwrap_or(true)
                && !exc.as_ref().map(|s| s.is_match(f)).unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn build_set(globs: &[String]) -> Option<globset::GlobSet> {
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for g in globs {
        if let Ok(glob) = Glob::new(g) {
            b.add(glob);
            any = true;
        }
    }
    if !any {
        return None;
    }
    b.build().ok()
}

fn hash_props(diff: &str, props: &[Property]) -> String {
    let mut h = DefaultHasher::new();
    diff.hash(&mut h);
    for p in props {
        p.id.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

/// Effective threshold: the configured threshold, clamped to the number of
/// properties actually derived so it can never be permanently unsatisfiable.
fn effective_threshold(cfg: &Config, n_props: usize) -> usize {
    cfg.threshold.min(n_props).max(1)
}

/// Core decision. `st` is the loaded prior session state.
pub fn evaluate(cfg: &Config, root: &Path, st: &crate::state::SessionState) -> Decision {
    // 1. Source the task's done_criteria. No criteria ⇒ nothing to formalize.
    let Some(criteria) = source_criteria(cfg, root) else {
        return allow("no-criteria", st);
    };

    // 2. Derive the semantic properties (deterministic, capped 3–5).
    let props = derive_properties(&criteria, cfg.min_properties, cfg.max_properties);
    if props.is_empty() {
        return allow("no-properties", st);
    }
    let threshold = effective_threshold(cfg, props.len());

    // 3. Gather the generated-code diff to check the properties against.
    let Some(changed) = crate::git::changed_files(root) else {
        return allow("no-git", st);
    };
    let files = checkable_files(cfg, &changed);
    if files.len() < cfg.min_changed_files {
        return allow("no-code-changes", st);
    }
    let crate::git::DiffText {
        text: diff,
        truncated,
    } = crate::git::diff_text(root, &files, cfg.max_diff_bytes);
    if diff.trim().is_empty() {
        return allow("empty-diff", st);
    }

    // Attempt counter resets after an idle gap (a fresh turn).
    let prior_attempts = if now() - st.last_ts > cfg.reset_after_secs {
        0
    } else {
        st.attempts
    };

    // Truncation guard (fail closed, bounded): the tail was dropped and is
    // unchecked, so neither the checker nor the "already-verified" convergence
    // can honestly certify the whole change. Block rather than let it slip.
    if truncated {
        return decide_truncated(cfg, &props, files, prior_attempts);
    }

    let hash = hash_props(&diff, &props);

    // Same (diff, properties) we already forced a *passing* check of → already
    // verified. Only a PASS (or inject's trust-after-one-block) records the hash,
    // so a below-threshold subprocess failure does NOT short-circuit here.
    if already_verified(st, &hash) {
        return allow("already-verified", st);
    }

    match cfg.mode {
        Mode::Inject => {
            // The hook can't itself judge whether each property holds, so a new
            // diff is unverified: satisfied = 0, which is below any threshold ≥ 1.
            decide_from_count(
                cfg,
                CheckOutcome::Verified {
                    satisfied: 0,
                    findings: None,
                },
                &props,
                threshold,
                files,
                hash,
                prior_attempts,
                &criteria,
            )
        }
        Mode::Subprocess => {
            let outcome = run_checker(cfg, &criteria, &props, &diff);
            decide_from_count(
                cfg,
                outcome,
                &props,
                threshold,
                files,
                hash,
                prior_attempts,
                &criteria,
            )
        }
    }
}

/// The outcome of trying to establish how many properties hold.
pub enum CheckOutcome {
    /// A count of satisfied properties was established (0 in inject mode's first
    /// pass; a parsed PASS count in subprocess mode). `findings` carries the
    /// checker's per-property verdict text, if any.
    Verified {
        satisfied: usize,
        findings: Option<String>,
    },
    /// The checker itself failed (crash / timeout / unusable output). Never the
    /// same as "checked and satisfied" — must not become a silent bypass.
    Error(String),
}

/// Turn a `CheckOutcome` into a `Decision`, enforcing the block threshold.
/// Split out from `evaluate` so the threshold logic is unit-testable without
/// git or a real checker subprocess.
#[allow(clippy::too_many_arguments)]
pub fn decide_from_count(
    cfg: &Config,
    outcome: CheckOutcome,
    props: &[Property],
    threshold: usize,
    files: Vec<String>,
    hash: String,
    prior_attempts: u32,
    criteria: &str,
) -> Decision {
    match outcome {
        CheckOutcome::Error(e) => {
            // Fail closed but bounded: block up to max_attempts, then give up
            // loudly so a permanently broken checker can't trap the turn.
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                eprintln!(
                    "propguard: WARNING checker still unavailable after {max} attempt(s) \
                     ({e}) — allowing the stop with properties UNVERIFIED. Fix checker_cmd \
                     (see `propguard status`) or set PROPGUARD_DISABLE=1.",
                    max = cfg.max_attempts,
                );
                return Decision::Allow {
                    tag: "checker-error-giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
            // The checker never ran, so NOTHING was evaluated. Report no
            // per-property violations: attributing the full derived prop_ids
            // here would pollute the property_id-keyed fleet-correlation store
            // with unchecked ids counted as real per-property violations
            // (CA-propguard-03). Mirrors the below-threshold path, which only
            // reports ids actually established as violated.
            Decision::Block {
                reason: checker_unavailable_reason(&e, attempts, cfg.max_attempts),
                tag: "checker-unavailable",
                files,
                properties: Vec::new(),
                attempts,
                last_hash: String::new(),
            }
        }
        CheckOutcome::Verified {
            satisfied,
            findings,
        } => {
            // ---- THE THRESHOLD ENFORCEMENT POINT ----
            if !below_threshold(satisfied, threshold) {
                // Enough properties hold: allow, and record the hash so the same
                // (diff, properties) is not re-checked.
                return Decision::Allow {
                    tag: "properties-satisfied",
                    attempts: 0,
                    last_hash: hash,
                };
            }
            // Below threshold → block (bounded by max_attempts).
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                return Decision::Allow {
                    tag: "giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
            let reason = block_reason(
                cfg,
                criteria,
                props,
                satisfied,
                threshold,
                &files,
                findings.as_deref(),
                attempts,
            );
            // Only record the hash as a "verified" marker when the block is
            // inject mode's trust-after-one-block: the hook can't count, so the
            // same diff is trusted next round. A subprocess check DID count and
            // found the diff failing — recording it here would let the very next
            // identical failing stop through via `already_verified` before
            // max_attempts (CA-propguard-01). Leave it empty so the checker
            // re-runs and blocks again until the properties are actually fixed.
            let last_hash = match cfg.mode {
                Mode::Inject => hash,
                Mode::Subprocess => String::new(),
            };
            // Report only the properties that are actually violated (did not
            // get an explicit PASS verdict) to overwatch's fleet-correlation
            // signal — reporting every derived property, including ones the
            // subprocess checker PASSed, would pollute that signal with
            // non-violations (CA-propguard-01). This only changes which ids
            // are recorded; the pass/block decision above is untouched.
            let violated = unsatisfied_prop_ids(props, findings.as_deref());
            Decision::Block {
                reason,
                tag: "below-threshold",
                files,
                properties: violated,
                attempts,
                last_hash,
            }
        }
    }
}

/// Which property ids are actually violated (did not receive an explicit PASS
/// verdict), for reporting to overwatch on a below-threshold block. In
/// subprocess mode `findings` carries the checker's per-property `PROP <id>:
/// PASS|FAIL` text; only ids that were NOT confirmed PASS on their own
/// anchored verdict line are considered violated (CA-propguard-01). In inject
/// mode there is no per-property verdict yet (satisfied is always 0 on the
/// first pass), so every derived property is still open and all are reported.
fn unsatisfied_prop_ids(props: &[Property], findings: Option<&str>) -> Vec<&'static str> {
    let Some(out) = findings else {
        return props.iter().map(|p| p.id).collect();
    };
    let lower = out.to_lowercase();
    props
        .iter()
        .filter(|p| {
            let id = p.id.to_lowercase();
            !lower
                .lines()
                .any(|line| verdict_for_id(line, &id) == Some(true))
        })
        .map(|p| p.id)
        .collect()
}

/// Pure mapper: escalate an isolated checker-outage give-up to a fail-closed
/// `Block` when the outage is *systemic* (recurring across tasks/sessions),
/// per the fleet-outage-vs-isolated-flake design. Any decision other than the
/// `checker-error-giveup` `Allow` is returned unchanged for either flag value
/// — this function only ever rewrites that one specific give-up, never any
/// other Allow tag (e.g. `properties-satisfied`, `giveup`, `truncated-giveup`)
/// and never an existing Block.
///
/// `systemic_outage == false` (the common, isolated case — a checker flaked
/// on just this task/session) keeps the input `Allow` unchanged: propguard
/// must never fail-halt the whole fleet over one task's transient checker
/// error. Only a *confirmed fleet-wide* outage (the caller has already
/// checked recurrence across distinct tasks/sessions) flips to `Block`.
pub fn escalate_giveup_on_systemic(decision: Decision, systemic_outage: bool) -> Decision {
    match decision {
        Decision::Allow {
            tag: "checker-error-giveup",
            ..
        } if systemic_outage => Decision::Block {
            reason: "propguard: FLEET-WIDE checker outage confirmed (recurring across \
                     multiple tasks/sessions) — holding the stop to avoid shipping \
                     UNVERIFIED code. Fix checker_cmd (see `propguard status`) or set \
                     PROPGUARD_DISABLE=1 to bypass."
                .to_string(),
            tag: "checker-outage-systemic",
            files: vec![],
            properties: vec![],
            attempts: 0,
            last_hash: String::new(),
        },
        other => other,
    }
}

fn allow(tag: &'static str, st: &crate::state::SessionState) -> Decision {
    Decision::Allow {
        tag,
        attempts: 0,
        last_hash: st.last_hash.clone(),
    }
}

/// A truncated diff has an unchecked tail. Fail closed but bounded, then give up
/// loudly — same shape as reviewgate's truncation guard.
fn decide_truncated(
    cfg: &Config,
    _props: &[Property],
    files: Vec<String>,
    prior_attempts: u32,
) -> Decision {
    let attempts = prior_attempts + 1;
    if attempts > cfg.max_attempts {
        eprintln!(
            "propguard: WARNING diff still exceeds max_diff_bytes ({max_bytes} B) after \
             {max} attempt(s) — allowing the stop with the truncated tail UNCHECKED. Split the \
             change, raise max_diff_bytes, or set PROPGUARD_DISABLE=1.",
            max_bytes = cfg.max_diff_bytes,
            max = cfg.max_attempts,
        );
        return Decision::Allow {
            tag: "truncated-giveup",
            attempts: 0,
            last_hash: String::new(),
        };
    }
    // The truncated tail was never checked, so no property was actually
    // evaluated as violated. Report no per-property violations: attributing
    // the full derived prop_ids here would pollute the property_id-keyed
    // fleet-correlation store the same way CA-propguard-03 does
    // (CA-propguard-04).
    Decision::Block {
        reason: truncated_reason(cfg, &files, attempts, cfg.max_attempts),
        tag: "diff-truncated",
        files,
        properties: Vec::new(),
        attempts,
        last_hash: String::new(),
    }
}

fn file_list(files: &[String]) -> String {
    let mut s = String::new();
    for f in files.iter().take(40) {
        s.push_str("  ");
        s.push_str(f);
        s.push('\n');
    }
    if files.len() > 40 {
        s.push_str(&format!("  … (+{} more)\n", files.len() - 40));
    }
    s
}

fn property_list(props: &[Property]) -> String {
    let mut s = String::new();
    for (i, p) in props.iter().enumerate() {
        s.push_str(&format!(
            "  {}. [{}] {}\n     → {}\n",
            i + 1,
            p.id,
            p.title,
            p.check_hint
        ));
    }
    s
}

/// The block reason handed back to the agent when fewer than `threshold`
/// properties are (known to be) satisfied. In inject mode `findings` is None and
/// `satisfied` is 0 (the diff is unverified); in subprocess mode `findings`
/// carries the checker's per-property verdicts.
#[allow(clippy::too_many_arguments)]
fn block_reason(
    _cfg: &Config,
    criteria: &str,
    props: &[Property],
    satisfied: usize,
    threshold: usize,
    files: &[String],
    findings: Option<&str>,
    attempt: u32,
) -> String {
    let findings_block = match findings {
        Some(f) if !f.trim().is_empty() => {
            format!(
                "--- チェッカーの判定 ---\n{}\n------------------------\n\n",
                f.trim()
            )
        }
        _ => String::new(),
    };
    format!(
        "🧪 propguard: 生成コードが満たすべき semantic property が閾値に達していません \
         (round {attempt}). satisfied={satisfied} < threshold={threshold}.\n\n\
         done_criteria から導出した検査対象プロパティ:\n{props}\n\
         対象ファイル ({n} files):\n{list}\n\
         {findings}\
         各プロパティについて自分の生成コードを検証し、成り立たないものを修正してから完了してください。\
         少なくとも {threshold} 個が成り立つことを確認し、結果を簡潔に報告すること \
         (誤検知だと判断したものは理由を述べて構いません)。\n\n\
         元の done_criteria:\n  {criteria}\n\n\
         このチェックを1回だけスキップ: project root に `.propguard-skip` を作成 (理由を1行)。\
         完全に無効化: 環境変数 PROPGUARD_DISABLE=1。",
        attempt = attempt,
        satisfied = satisfied,
        threshold = threshold,
        props = property_list(props),
        n = files.len(),
        list = file_list(files),
        findings = findings_block,
        criteria = criteria.trim(),
    )
}

fn checker_unavailable_reason(err: &str, attempt: u32, max: u32) -> String {
    format!(
        "🚧 propguard: 独立プロパティチェッカーを実行できませんでした (round {attempt}/{max}).\n\n\
         checker_cmd がエラー / タイムアウト / 解析不能でした:\n  {err}\n\n\
         これは「プロパティ充足」ではありません。壊れたチェッカーを無言で通過させると gate が\
         バイパスになるため、この停止を一時的にブロックしています。{max}回連続で失敗した場合は\
         警告を出して通過を許可します (永久にはブロックしません)。\n\n\
         前に進むには次のいずれか:\n\
         - checker_cmd を修正する (`propguard status` で解決済みコマンドを確認)。\n\
         - このチェックを1回だけスキップ: project root に `.propguard-skip` を作成 (理由を1行)。\n\
         - propguard を完全に無効化: 環境変数 PROPGUARD_DISABLE=1。",
        attempt = attempt,
        max = max,
        err = err.trim(),
    )
}

fn truncated_reason(cfg: &Config, files: &[String], attempt: u32, max: u32) -> String {
    format!(
        "🚧 propguard: 変更差分が大きすぎてプロパティ検査用に切り詰められました (round {attempt}/{max}).\n\n\
         diff が max_diff_bytes ({max_bytes} B) を超えたため末尾が検査対象から欠落しています。\
         欠落分は検査されていないため、この停止を無言で許可すると未検査の変更が gate をすり抜けます。\
         {max}回連続で解消しなければ警告を出して通過を許可します (永久にはブロックしません)。\n\n\
         対象ファイル ({n} files):\n{list}\
         前に進むには次のいずれか:\n\
         - 変更を小さく分割し、それぞれが max_diff_bytes に収まるようにする。\n\
         - max_diff_bytes を引き上げる (現在 {max_bytes} B)。\n\
         - このチェックを1回だけスキップ: `.propguard-skip` を作成。完全に無効化: PROPGUARD_DISABLE=1。",
        attempt = attempt,
        max = max,
        max_bytes = cfg.max_diff_bytes,
        n = files.len(),
        list = file_list(files),
    )
}

// ---------------------------------------------------------------------------
// subprocess mode: an independent checker reports per-property PASS/FAIL.
// ---------------------------------------------------------------------------

/// Run `checker_cmd`, feeding it the properties + diff on stdin and reading a
/// `PROP <id>: PASS|FAIL` verdict per property on stdout.
fn run_checker(cfg: &Config, criteria: &str, props: &[Property], diff: &str) -> CheckOutcome {
    let prompt = format!(
        "あなたは独立したプロパティ検査官です。以下の done_criteria から導出された semantic property が、\
         提示された git diff の生成コードで成り立つかを 1 つずつ判定してください。\n\n\
         done_criteria:\n{criteria}\n\n\
         プロパティ:\n{props}\n\
         各プロパティについて、次の形式で厳密に1行ずつ出力してください (他の行は無視されます):\n\
         PROP <id>: PASS   または   PROP <id>: FAIL — 理由\n\n\
         --- diff ---\n{diff}\n",
        criteria = criteria.trim(),
        props = property_list(props),
        diff = diff,
    );

    let mut cmd = build_command(&cfg.checker_cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // CA-propguard-004: when `checker_cmd` is shell-wrapped (contains shell
    // metacharacters → `harness_core::shell::command` spawns `sh -c "..."` /
    // `cmd /C "..."`), the direct child we get back is the *shell*, not the
    // real checker. If the shell execs or backgrounds a grandchild (or a
    // pipeline of several processes), killing only the direct child on
    // timeout never reaches that real process — it can keep running (and
    // keep the stdout pipe open, see CA-propguard-005) forever. Put the
    // child in its own process group on Unix so the whole tree the shell
    // spawned can be killed together via a single group-kill on timeout.
    // Best-effort on platforms without process groups (Windows): we still
    // kill the direct child; a grandchild there may survive, but we don't
    // regress any existing guarantee (none existed before this fix either).
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return CheckOutcome::Error(format!("spawn: {e}")),
    };
    // Write stdin from a background thread rather than inline. If the checker
    // writes enough stdout before draining stdin (its stdout pipe fills up
    // while our stdin pipe is also full), a synchronous write_all here would
    // block forever *before* we ever reach `wait_timeout` below — meaning
    // `cfg.checker_timeout_secs` would provide zero protection against that
    // deadlock. Doing the write on its own thread lets the main thread reach
    // `wait_timeout` immediately, so the overall call is always bounded by the
    // configured timeout regardless of how the child interleaves its I/O. The
    // thread is detached: if the child is killed on timeout, the write simply
    // errors out (broken pipe) and the thread exits; nothing to join here
    // since we don't want the join itself to be able to block.
    if let Some(mut stdin) = child.stdin.take() {
        let prompt_bytes = prompt.into_bytes();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&prompt_bytes);
        });
    }

    let timeout = Duration::from_secs(cfg.checker_timeout_secs);
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            // CA-propguard-005: the *immediate* child (possibly the shell
            // wrapper) having exited does not mean stdout's write end is
            // closed — a backgrounded/detached grandchild can still hold the
            // pipe's write end open, and a bare `read_to_string` here would
            // then block indefinitely: a second hang vector `wait_timeout`
            // above does nothing to bound. Do the read on its own thread and
            // join it with the same timeout budget so this call can never
            // hang past a reasonable bound even if the pipe stays open.
            let out = read_stdout_bounded(child.stdout.take(), timeout);
            if !status.success() && out.trim().is_empty() {
                return CheckOutcome::Error(format!("exit {:?}", status.code()));
            }
            parse_checker_output(&out, props)
        }
        Ok(None) => {
            kill_checker_tree(&mut child);
            let _ = child.wait();
            CheckOutcome::Error("timed out".to_string())
        }
        Err(e) => CheckOutcome::Error(format!("wait: {e}")),
    }
}

/// Kill the checker's whole process tree on timeout, not just the direct
/// child. On Unix, `run_checker` puts the child in its own process group
/// (see `cmd.process_group(0)` above), so a negative-pid kill targets the
/// group — the shell *and* whatever it exec'd/backgrounded — in one call.
/// Falls back to killing just the direct child where that isn't available
/// (non-Unix, or if the group id can't be determined), which is no worse
/// than the pre-fix behavior.
fn kill_checker_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: `kill` is a plain libc syscall; passing a negative pid
        // targets the process group whose id equals the child's pid (valid
        // because we created that group via `process_group(0)` at spawn
        // time). This is best-effort cleanup on a timeout path — any error
        // (e.g. the group already gone) is intentionally ignored, mirroring
        // the pre-existing `let _ = child.kill()` behavior it replaces.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    // Always also signal the direct child through the standard API: this is
    // a harmless no-op if the group kill above already reaped it, and it is
    // the only mechanism at all on non-Unix platforms.
    let _ = child.kill();
}

/// Read `stdout` to completion, but never block past `timeout`: the read
/// happens on a background thread and we join it with a bound instead of
/// calling `read_to_string` inline. If the join times out (pipe still open
/// because some lingering process holds the write end), we give up and
/// return whatever was read so far instead of hanging — the thread itself
/// is detached and leaked in that case, matching the fail-soft, bounded-call
/// contract the rest of `run_checker` already has via `wait_timeout`.
fn read_stdout_bounded(stdout: Option<std::process::ChildStdout>, timeout: Duration) -> String {
    use std::sync::mpsc;
    let Some(mut so) = stdout else {
        return String::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut out = String::new();
        let _ = so.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    rx.recv_timeout(timeout).unwrap_or_default()
}

/// If `line` (already lowercased) is *this* property's own verdict line — i.e.
/// it is anchored `PROP <id>[:…]` after trimming — return its verdict:
/// `Some(true)` for PASS, `Some(false)` for FAIL/anything-not-PASS. Returns
/// `None` when the line is not this property's verdict line (so a different
/// property's explanation that merely mentions this id can never win).
fn verdict_for_id(line: &str, id: &str) -> Option<bool> {
    // Must start with the `prop` keyword (after leading whitespace).
    let rest = line.trim_start().strip_prefix("prop")?;
    // A separator (whitespace) must follow the keyword before the id.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    // The id must be the next token, on a boundary (end, ':' or whitespace) so
    // "determinism" does not match "determinism-foo".
    let after = rest.trim_start().strip_prefix(id)?;
    match after.chars().next() {
        None => {}
        Some(c) if c == ':' || c.is_whitespace() => {}
        _ => return None,
    }
    // Read the verdict from this property's own line only. PASS iff it says
    // "pass" and not "fail".
    Some(line.contains("pass") && !line.contains("fail"))
}

/// Parse `PROP <id>: PASS|FAIL` lines. A property is counted satisfied only when
/// its id is explicitly reported PASS on its OWN anchored verdict line. Output
/// that mentions none of the derived property ids is unusable → Error (fail
/// closed), never silently "all pass".
pub fn parse_checker_output(out: &str, props: &[Property]) -> CheckOutcome {
    let lower = out.to_lowercase();
    let mut satisfied = 0usize;
    let mut seen_any = false;
    for p in props {
        let id = p.id.to_lowercase();
        // Find this property's OWN verdict line and read its PASS/FAIL. The line
        // must be *anchored* to `PROP <id>` (after trimming), not merely mention
        // the id somewhere: another property's PASS explanation can name this id
        // in prose (e.g. "... this also confirms determinism holds ..."), and an
        // unanchored substring match would let that PASS override this property's
        // real FAIL verdict (CA-propguard-02).
        for line in lower.lines() {
            if let Some(verdict) = verdict_for_id(line, &id) {
                seen_any = true;
                if verdict {
                    satisfied += 1;
                }
                break;
            }
        }
    }
    if !seen_any {
        return CheckOutcome::Error(format!(
            "checker output named none of the {} derived properties",
            props.len()
        ));
    }
    CheckOutcome::Verified {
        satisfied,
        findings: Some(out.trim().to_string()),
    }
}

fn build_command(cmdline: &str) -> Command {
    let needs_shell = cmdline.contains(|c| "|&;<>(){}$`\\\"'*?".contains(c));
    if needs_shell {
        harness_core::shell::command(cmdline)
    } else {
        let mut parts = cmdline.split_whitespace();
        let prog = parts.next().unwrap_or("claude");
        let mut c = Command::new(prog);
        c.args(parts);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::CATALOG;

    fn props_by_ids(ids: &[&str]) -> Vec<Property> {
        ids.iter()
            .map(|id| *CATALOG.iter().find(|p| p.id == *id).unwrap())
            .collect()
    }

    fn cfg_default() -> Config {
        Config::default() // threshold 3, max_attempts 2
    }

    // ── include/exclude filtering ──────────────────────────────────────────
    #[test]
    fn include_exclude_filtering() {
        let cfg = Config {
            include: vec!["**/*.rs".to_string()],
            exclude: vec!["**/target/**".to_string()],
            ..Config::default()
        };
        let changed = vec![
            "src/main.rs".to_string(),
            "README.md".to_string(),
            "target/x.rs".to_string(),
        ];
        assert_eq!(
            checkable_files(&cfg, &changed),
            vec!["src/main.rs".to_string()]
        );
    }

    // ── the threshold enforcement point ────────────────────────────────────

    /// At or above the threshold → ALLOW, and the hash is recorded.
    #[test]
    fn at_threshold_allows_and_records_hash() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Verified {
                satisfied: 3,
                findings: None,
            },
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "hashabc".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Allow { tag, last_hash, .. } => {
                assert_eq!(tag, "properties-satisfied");
                assert_eq!(
                    last_hash, "hashabc",
                    "a satisfied check must record the hash"
                );
            }
            Decision::Block { .. } => panic!("satisfied >= threshold must allow"),
        }
    }

    /// Below the threshold → BLOCK, naming the properties.
    #[test]
    fn below_threshold_blocks() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Verified {
                satisfied: 1,
                findings: Some("PROP error-path: FAIL — panics".to_string()),
            },
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "hashabc".to_string(),
            0,
            "handle errors",
        );
        match d {
            Decision::Block {
                tag,
                reason,
                properties,
                ..
            } => {
                assert_eq!(tag, "below-threshold");
                assert!(properties.contains(&"error-path"));
                assert!(reason.contains("threshold=3"));
                assert!(
                    reason.contains("PROPGUARD_DISABLE"),
                    "must name an escape hatch"
                );
            }
            Decision::Allow { .. } => panic!("satisfied < threshold must block"),
        }
    }

    /// Inject mode's first pass (satisfied = 0) is below any threshold ≥ 1 → block.
    #[test]
    fn inject_first_pass_blocks_as_unverified() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Verified {
                satisfied: 0,
                findings: None,
            },
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            0,
            "dc",
        );
        assert!(matches!(d, Decision::Block { .. }));
    }

    /// Bounded: after max_attempts consecutive below-threshold stops, give up.
    #[test]
    fn below_threshold_gives_up_after_max_attempts() {
        let cfg = cfg_default(); // max_attempts = 2
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Verified {
                satisfied: 0,
                findings: None,
            },
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            cfg.max_attempts,
            "dc",
        );
        match d {
            Decision::Allow { tag, .. } => assert_eq!(tag, "giveup"),
            Decision::Block { .. } => panic!("must give up so the turn is never trapped"),
        }
    }

    // ── checker error must not become a silent bypass ──────────────────────
    #[test]
    fn checker_error_blocks_it_does_not_allow() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Error("spawn: boom".to_string()),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Block { tag, reason, .. } => {
                assert_eq!(tag, "checker-unavailable");
                assert!(reason.contains("PROPGUARD_DISABLE"));
            }
            Decision::Allow { .. } => panic!("checker error must block (fail-closed), not allow"),
        }
    }

    // ── CA-propguard-03: a checker-unavailable Block must NOT attribute
    //    per-property violations. The checker never ran, so nothing was
    //    actually evaluated; stuffing the full derived prop_ids into
    //    `Block.properties` pollutes the property_id-keyed fleet-correlation
    //    store with unchecked ids counted as real per-property violations. ────
    #[test]
    fn checker_unavailable_block_reports_no_property_violations() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Error("spawn: boom".to_string()),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Block {
                tag, properties, ..
            } => {
                assert_eq!(tag, "checker-unavailable");
                assert!(
                    properties.is_empty(),
                    "a checker-unavailable block evaluated no property — it must not \
                     report any per-property violation to the correlation store, got {properties:?}"
                );
            }
            Decision::Allow { .. } => panic!("checker error must block (fail-closed)"),
        }
    }

    #[test]
    fn checker_error_gives_up_after_max_attempts_but_never_traps() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Error("still broken".to_string()),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            cfg.max_attempts,
            "dc",
        );
        match d {
            Decision::Allow { tag, .. } => assert_eq!(tag, "checker-error-giveup"),
            Decision::Block { .. } => panic!("must give up after max_attempts"),
        }
    }

    // ── isolated vs systemic checker-outage escalation ─────────────────────

    fn checker_error_giveup() -> Decision {
        Decision::Allow {
            tag: "checker-error-giveup",
            attempts: 0,
            last_hash: String::new(),
        }
    }

    #[test]
    fn escalate_giveup_isolated_stays_allow() {
        // Not systemic: the isolated give-up case must pass through unchanged.
        let d = escalate_giveup_on_systemic(checker_error_giveup(), false);
        match d {
            Decision::Allow { tag, .. } => assert_eq!(tag, "checker-error-giveup"),
            Decision::Block { .. } => panic!("isolated give-up must stay Allow"),
        }
    }

    #[test]
    fn escalate_giveup_systemic_becomes_block() {
        let d = escalate_giveup_on_systemic(checker_error_giveup(), true);
        match d {
            Decision::Block {
                tag,
                reason,
                attempts,
                last_hash,
                files,
                properties,
            } => {
                assert_eq!(tag, "checker-outage-systemic");
                assert!(reason.contains("PROPGUARD_DISABLE"));
                assert!(reason.to_lowercase().contains("fleet"));
                assert_eq!(attempts, 0);
                assert!(last_hash.is_empty());
                assert!(files.is_empty());
                assert!(properties.is_empty());
            }
            Decision::Allow { .. } => panic!("systemic outage must fail-closed to Block"),
        }
    }

    #[test]
    fn escalate_giveup_non_giveup_decision_passes_through_both_flags() {
        // A normal Block (e.g. below-threshold) must not be touched by this
        // mapper regardless of the systemic flag.
        let block = || Decision::Block {
            reason: "some other block".to_string(),
            tag: "below-threshold",
            files: vec![],
            properties: vec![],
            attempts: 1,
            last_hash: String::new(),
        };
        match escalate_giveup_on_systemic(block(), true) {
            Decision::Block { tag, .. } => assert_eq!(tag, "below-threshold"),
            Decision::Allow { .. } => panic!("non-giveup decision must not be rewritten"),
        }
        match escalate_giveup_on_systemic(block(), false) {
            Decision::Block { tag, .. } => assert_eq!(tag, "below-threshold"),
            Decision::Allow { .. } => panic!("non-giveup decision must not be rewritten"),
        }

        // An Allow with a different tag (e.g. properties-satisfied) must not
        // be rewritten either, even when systemic_outage is true.
        let other_allow = || Decision::Allow {
            tag: "properties-satisfied",
            attempts: 0,
            last_hash: "h".to_string(),
        };
        match escalate_giveup_on_systemic(other_allow(), true) {
            Decision::Allow { tag, .. } => assert_eq!(tag, "properties-satisfied"),
            Decision::Block { .. } => panic!("non-giveup Allow must not be rewritten"),
        }
        match escalate_giveup_on_systemic(other_allow(), false) {
            Decision::Allow { tag, .. } => assert_eq!(tag, "properties-satisfied"),
            Decision::Block { .. } => panic!("non-giveup Allow must not be rewritten"),
        }
    }

    // ── checker output parsing ─────────────────────────────────────────────
    #[test]
    fn parse_counts_only_explicit_pass() {
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let out = "PROP error-path: PASS\nPROP output-schema: FAIL — schema changed\nPROP determinism: PASS";
        match parse_checker_output(out, &props) {
            CheckOutcome::Verified { satisfied, .. } => assert_eq!(satisfied, 2),
            CheckOutcome::Error(e) => panic!("should parse: {e}"),
        }
    }

    #[test]
    fn parse_unrelated_output_is_error_not_all_pass() {
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        // Output that names none of the property ids must fail closed, not be
        // mistaken for "everything passed".
        match parse_checker_output("looks good to me!", &props) {
            CheckOutcome::Error(_) => {}
            CheckOutcome::Verified { .. } => {
                panic!("unusable checker output must be an Error (fail closed), not all-pass")
            }
        }
    }

    #[test]
    fn unspawnable_checker_is_error() {
        let cfg = Config {
            checker_cmd: "propguard-no-such-binary-xyzzy".to_string(),
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);
        match run_checker(&cfg, "dc", &props, "diff") {
            CheckOutcome::Error(_) => {}
            CheckOutcome::Verified { .. } => panic!("an unspawnable checker must be an Error"),
        }
    }

    // ── stdin/stdout deadlock regression (fix-propguard-003) ───────────────
    //
    // A checker that writes a lot of stdout *before* draining stdin can
    // deadlock a caller that writes stdin synchronously on the main thread:
    // both the child's stdout pipe and our stdin pipe fill up (~64KB OS pipe
    // buffers) and neither side can make progress. Because that write used to
    // happen before `wait_timeout`, `checker_timeout_secs` gave zero
    // protection. This test uses a real "checker" that floods stdout without
    // reading stdin at all, with a diff large enough to fill the stdin pipe
    // buffer, and asserts `run_checker` still returns (as a timeout/error)
    // within a small configured timeout instead of hanging indefinitely.
    #[test]
    fn checker_stdout_flood_without_draining_stdin_does_not_hang() {
        // Print far more than a typical pipe buffer (~64KB) to stdout, then
        // exit, *without* ever reading stdin. If stdin were written
        // synchronously before wait_timeout, and the diff below is bigger
        // than the stdin pipe buffer too, the parent would block on
        // write_all while this child blocks on write (stdout full and
        // un-drained) — a classic two-pipe deadlock.
        let cfg = Config {
            checker_cmd: "yes X | head -c 5000000".to_string(),
            checker_timeout_secs: 2,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);
        // Bigger than a pipe buffer, so the old synchronous stdin write would
        // itself block once the child's stdout side backs up.
        let big_diff = "line of diff content\n".repeat(20_000);

        let start = std::time::Instant::now();
        let outcome = run_checker(&cfg, "dc", &props, &big_diff);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "run_checker must be bounded by checker_timeout_secs, took {elapsed:?}"
        );
        // Whatever the verdict (timeout error, or a fast exit that never
        // needed stdin at all), it must not be silently "all pass" via a
        // hang-then-succeed path; either shape is acceptable evidence the
        // call returned instead of hanging.
        match outcome {
            CheckOutcome::Error(_) | CheckOutcome::Verified { .. } => {}
        }
    }

    // ── shell-wrapped timeout must kill the real grandchild (CA-propguard-004) ──
    //
    // When `checker_cmd` contains shell metacharacters, `build_command` runs it
    // via `harness_core::shell::command` (`sh -c "..."`), so the direct child
    // `run_checker` gets back is the *shell*, not the real checker. Before this
    // fix, the timeout path only called `child.kill()` on that shell — a
    // grandchild the shell backgrounds/execs never got signaled and could keep
    // running after `run_checker` returned. Assert that a shell-wrapped
    // checker_cmd which backgrounds a long-running child does not leave that
    // child alive once the timeout has fired.
    #[cfg(unix)]
    #[test]
    fn shell_wrapped_timeout_kills_the_real_backgrounded_checker() {
        let marker = std::env::temp_dir().join(format!(
            "propguard-ca004-marker-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&marker);

        // `;` is a shell metacharacter, so this goes through the shell path.
        // The backgrounded `sleep 30 &` is the real long-running checker
        // process; it writes its pid to `marker` so the test can verify it
        // is actually gone (not just that `run_checker` returned) after the
        // timeout fires.
        let cfg = Config {
            checker_cmd: format!("sleep 30 & echo $! > {}; wait $!", marker.display()),
            checker_timeout_secs: 1,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);

        let start = std::time::Instant::now();
        let outcome = run_checker(&cfg, "dc", &props, "diff");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "run_checker must be bounded by checker_timeout_secs even when shell-wrapped, took {elapsed:?}"
        );
        assert!(
            matches!(outcome, CheckOutcome::Error(_)),
            "a timed-out shell-wrapped checker must be reported as an Error"
        );

        // Give the OS a brief moment to actually reap the killed process
        // before we check, then confirm the grandchild `sleep` is gone.
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(pid_str) = std::fs::read_to_string(&marker) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                // SAFETY: signal 0 is a pure existence/permission probe.
                let alive = unsafe { libc::kill(pid, 0) == 0 };
                assert!(
                    !alive,
                    "the backgrounded grandchild (pid {pid}) must be killed on timeout, not just the shell"
                );
            }
        }
        let _ = std::fs::remove_file(&marker);
    }

    // ── bounded stdout read after immediate-child exit (CA-propguard-005) ──
    //
    // After `wait_timeout` reports the immediate child has exited, reading
    // its stdout must not be able to hang even if a lingering process still
    // holds the pipe's write end open. This test shell-wraps a checker_cmd
    // whose immediate process exits right away, but backgrounds a child that
    // inherits the stdout fd and keeps it open well past `checker_timeout_secs`;
    // without a bounded read this would hang `run_checker` indefinitely.
    #[cfg(unix)]
    #[test]
    fn lingering_stdout_holder_does_not_hang_the_read() {
        let cfg = Config {
            // The immediate shell process exits promptly ("exit 0"), but a
            // backgrounded subshell inherits the stdout fd and sleeps well
            // beyond the timeout, keeping the pipe's write end open.
            checker_cmd: "(sleep 5 &) ; exit 0".to_string(),
            checker_timeout_secs: 1,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);

        let start = std::time::Instant::now();
        let outcome = run_checker(&cfg, "dc", &props, "diff");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(4),
            "reading stdout after the immediate child exits must not hang past a reasonable \
             bound even if a lingering process keeps the pipe open, took {elapsed:?}"
        );
        match outcome {
            CheckOutcome::Error(_) | CheckOutcome::Verified { .. } => {}
        }
    }

    // ── truncation guard ───────────────────────────────────────────────────
    #[test]
    fn truncated_diff_blocks_then_gives_up() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_truncated(&cfg, &props, vec!["src/x.rs".to_string()], 0);
        match d {
            Decision::Block {
                tag,
                last_hash,
                reason,
                ..
            } => {
                assert_eq!(tag, "diff-truncated");
                assert!(last_hash.is_empty(), "must not certify an unchecked tail");
                assert!(reason.contains("max_diff_bytes"));
            }
            Decision::Allow { .. } => panic!("a truncated diff must block, not silently allow"),
        }
        let g = decide_truncated(&cfg, &props, vec!["src/x.rs".to_string()], cfg.max_attempts);
        assert!(matches!(g, Decision::Allow { tag, .. } if tag == "truncated-giveup"));
    }

    // ── CA-propguard-04: a diff-truncated Block must NOT attribute
    //    per-property violations. The truncated tail was never checked, so no
    //    property was actually evaluated as violated; reporting the full
    //    prop_ids pollutes the property_id-keyed fleet-correlation store the
    //    same way CA-propguard-03 does. ──────────────────────────────────────
    #[test]
    fn truncated_block_reports_no_property_violations() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_truncated(&cfg, &props, vec!["src/x.rs".to_string()], 0);
        match d {
            Decision::Block {
                tag, properties, ..
            } => {
                assert_eq!(tag, "diff-truncated");
                assert!(
                    properties.is_empty(),
                    "a diff-truncated block checked nothing (the tail was dropped) — it must \
                     not report any per-property violation to the correlation store, got {properties:?}"
                );
            }
            Decision::Allow { .. } => panic!("a truncated diff must block"),
        }
    }

    #[test]
    fn effective_threshold_is_clamped_to_property_count() {
        let cfg = Config {
            threshold: 5,
            ..Config::default()
        };
        // Only 2 properties derived → threshold can't exceed 2.
        assert_eq!(effective_threshold(&cfg, 2), 2);
    }

    #[test]
    fn below_threshold_is_the_single_comparison() {
        assert!(below_threshold(0, 3));
        assert!(below_threshold(2, 3));
        assert!(!below_threshold(3, 3));
        assert!(!below_threshold(4, 3));
    }

    // ── CA-propguard-01: a failing subprocess check must NOT arm the
    //    "already-verified" shortcut, so the next identical failing diff is
    //    re-checked (fail-closed) rather than auto-allowed. ─────────────────────
    fn state_with_hash(h: &str) -> crate::state::SessionState {
        crate::state::SessionState {
            attempts: 1,
            last_hash: h.to_string(),
            last_ts: now(),
        }
    }

    /// Subprocess mode, below threshold: the Block must not record the diff hash
    /// as a passing marker, so the SAME unfixed diff on the next round is NOT
    /// short-circuited by `already_verified` and the checker re-runs.
    #[test]
    fn subprocess_block_does_not_arm_already_verified() {
        let cfg = Config {
            mode: Mode::Subprocess,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        // Round 1: checker says satisfied=1 (< threshold 3) => Block.
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Verified {
                satisfied: 1,
                findings: Some("PROP error-path: FAIL".to_string()),
            },
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "HASH_FAIL".to_string(),
            0,
            "dc",
        );
        let recorded = match d {
            Decision::Block { tag, last_hash, .. } => {
                assert_eq!(tag, "below-threshold");
                last_hash
            }
            Decision::Allow { .. } => panic!("below threshold must block"),
        };
        // Round 2: persisted state carries whatever the Block recorded. The same
        // unfixed diff (HASH_FAIL) must NOT be treated as already-verified.
        let st = state_with_hash(&recorded);
        assert!(
            !already_verified(&st, "HASH_FAIL"),
            "a failing subprocess check must not auto-allow the next identical diff"
        );
    }

    /// Inject mode's documented "trust-after-one-block": the first (satisfied=0)
    /// block DOES record the hash, so the same diff is trusted next round.
    #[test]
    fn inject_block_arms_trust_after_one_block() {
        let cfg = Config {
            mode: Mode::Inject,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            CheckOutcome::Verified {
                satisfied: 0,
                findings: None,
            },
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "HASH_INJECT".to_string(),
            0,
            "dc",
        );
        let recorded = match d {
            Decision::Block { last_hash, .. } => last_hash,
            Decision::Allow { .. } => panic!("inject first pass must block"),
        };
        let st = state_with_hash(&recorded);
        assert!(
            already_verified(&st, "HASH_INJECT"),
            "inject mode must trust the same diff after one block"
        );
    }

    // ── CA-propguard-01 (2026 audit round): a subprocess below-threshold Block
    //    must report only the UNSATISFIED properties to overwatch, not every
    //    derived property — PASSed properties polluting the fleet-correlation
    //    signal is itself a defect distinct from the hash-arming bug above. ────
    #[test]
    fn below_threshold_block_reports_only_unsatisfied_properties() {
        let cfg = Config {
            mode: Mode::Subprocess,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let outcome = CheckOutcome::Verified {
            satisfied: 2,
            findings: Some(
                "PROP error-path: PASS\n\
                 PROP output-schema: FAIL — schema changed\n\
                 PROP determinism: PASS"
                    .to_string(),
            ),
        };
        let d = decide_from_count(
            &cfg,
            outcome,
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "HASH".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Block { properties, .. } => {
                assert_eq!(
                    properties,
                    vec!["output-schema"],
                    "PASSed properties must not be recorded as fleet violations"
                );
            }
            Decision::Allow { .. } => panic!("below threshold must block"),
        }
    }

    // ── CA-propguard-02: a property's verdict must be read from its OWN verdict
    //    line, not from another property's explanation that mentions its id. ─────
    #[test]
    fn parse_anchors_to_the_propertys_own_verdict_line() {
        let props = props_by_ids(&["idempotence", "determinism", "output-schema"]);
        let out = "\
PROP idempotence: PASS — this also confirms determinism holds and output-schema is untouched\n\
PROP determinism: FAIL — hidden RNG dependency\n\
PROP output-schema: PASS";
        match parse_checker_output(out, &props) {
            CheckOutcome::Verified { satisfied, .. } => {
                // idempotence PASS + output-schema PASS = 2; determinism is FAIL
                // and must never be counted satisfied via line 1's mention.
                assert_eq!(
                    satisfied, 2,
                    "determinism was reported FAIL and must not be counted satisfied"
                );
            }
            CheckOutcome::Error(e) => panic!("should parse: {e}"),
        }
    }
}
