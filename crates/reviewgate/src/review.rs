//! The gate proper: gather the reviewable diff, decide whether to block the
//! stop, and produce the model-facing reason. Two modes:
//!
//!   * `inject` — block once per new diff state and inject a review rubric; the
//!     running subscription agent reviews its own changes.
//!   * `subprocess` — run an independent reviewer over the diff and block only
//!     when it reports issues, injecting just those findings.
//!
//! Convergence: we hash the reviewable diff. A stop whose diff matches the last
//! one we forced a review of is allowed (already reviewed). A changed diff costs
//! one more round, capped by `max_attempts`. Environment errors that predate any
//! review (no git repo, nothing reviewable) always allow. A reviewer that itself
//! fails (subprocess crash / timeout / unusable output) does NOT allow silently:
//! it blocks up to `max_attempts` times with a loud, escapable reason, then
//! gives up loudly — a broken reviewer must never become a bypass.
//!
//! Truncation guard: if the diff exceeds `max_diff_bytes` it is cut to fit and
//! the tail is dropped. A dropped tail is *unreviewed* — the reviewer never saw
//! it and the hash below can't cover it — so a truncated diff must NOT be
//! silently allowed (that would let the tail bypass the gate). We block it up to
//! `max_attempts` with a loud, escapable reason (split the change, raise
//! `max_diff_bytes`, `.reviewgate-skip`, or `REVIEWGATE_DISABLE=1`), then give up
//! loudly with a distinct tag so the turn is never permanently trapped.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use globset::{Glob, GlobSetBuilder};
use harness_core::verdict::Verdict;
use wait_timeout::ChildExt;

use crate::config::{Config, Mode};

/// What the gate decided. `tag` is a short label for the JSONL log.
pub enum Decision {
    /// Let the stop through. Carries the per-session state to persist and a log tag.
    Allow {
        tag: &'static str,
        attempts: u32,
        last_hash: String,
    },
    /// Block the stop; inject `reason`. Carries state to persist.
    Block {
        reason: String,
        tag: &'static str,
        files: Vec<String>,
        attempts: u32,
        last_hash: String,
    },
}

/// Files that changed *and* are worth reviewing (match include, not exclude).
pub fn reviewable_files(cfg: &Config, changed: &[String]) -> Vec<String> {
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

fn hash_diff(diff: &str) -> String {
    let mut h = DefaultHasher::new();
    diff.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

/// Core decision. `st` is the loaded prior session state.
pub fn evaluate(cfg: &Config, root: &Path, st: &crate::state::SessionState) -> Decision {
    let changed = match crate::git::changed_files(root) {
        // No git scope: nothing to review, allow (unchanged behavior).
        crate::git::ChangeScan::NotRepo => return allow("no-git", st),
        // A git command errored inside a real repo: the changed set is
        // UNDETERMINED. Don't silently allow (that's the fail-open this closes,
        // and it's the same treatment the reviewer-outage path already gets) —
        // block, bounded & escapable so the turn is never trapped.
        crate::git::ChangeScan::Failed => {
            let prior_attempts = if now() - st.last_ts > cfg.reset_after_secs {
                0
            } else {
                st.attempts
            };
            return decide_scan_failed(cfg, prior_attempts);
        }
        crate::git::ChangeScan::Files(v) => v,
    };
    let files = reviewable_files(cfg, &changed);
    if files.len() < cfg.min_changed_files {
        return allow("no-reviewable-changes", st);
    }

    let crate::git::DiffText {
        text: diff,
        truncated,
    } = crate::git::diff_text(root, &files, cfg.max_diff_bytes);
    if diff.trim().is_empty() {
        return allow("empty-diff", st);
    }

    // attempt counter resets after an idle gap (a fresh turn).
    let prior_attempts = if now() - st.last_ts > cfg.reset_after_secs {
        0
    } else {
        st.attempts
    };

    // Truncation guard (fail closed, bounded): the diff was larger than
    // max_diff_bytes and the tail was dropped. That tail is unreviewed, and the
    // hash below can't cover it, so neither the subprocess reviewer nor the
    // inject-mode "already-reviewed" convergence can honestly certify the whole
    // change. Don't silently allow — block (bounded, escapable) so the tail can't
    // slip through unreviewed. Checked before the hash short-circuit precisely
    // because that short-circuit would otherwise wave a truncated diff through.
    if truncated {
        return decide_truncated(cfg, files, prior_attempts);
    }

    let hash = hash_diff(&diff);

    // Same diff we already forced a review of → the agent reviewed exactly this.
    if !st.last_hash.is_empty() && st.last_hash == hash {
        return allow("already-reviewed", st);
    }

    match cfg.mode {
        Mode::Inject => {
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                return Decision::Allow {
                    tag: "giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
            let reason = inject_reason(cfg, &files, attempts);
            Decision::Block {
                reason,
                tag: "blocked-inject",
                files,
                attempts,
                last_hash: hash,
            }
        }
        Mode::Subprocess => {
            let result = run_reviewer(cfg, &diff);
            decide_subprocess(cfg, result, files, hash, prior_attempts)
        }
    }
}

/// Turn a reviewer [`Verdict`] into a `Decision`. Split out from `evaluate` so
/// the decision logic (especially the fail-closed undetermined path) is
/// unit-testable without spawning a real reviewer subprocess.
fn decide_subprocess(
    cfg: &Config,
    result: Verdict,
    files: Vec<String>,
    hash: String,
    prior_attempts: u32,
) -> Decision {
    match result {
        // Fail *closed* (but bounded): a reviewer that crashed, timed out, or
        // produced unparseable output is NOT the same as "reviewed and clean".
        // Silently allowing here would turn a broken reviewer into a bypass —
        // exactly the hole this gate exists to close. Instead we block the stop
        // with a loud, actionable reason for up to `max_attempts` consecutive
        // stops (giving transient failures — load, timeout — a chance to
        // recover), then give up *loudly* so a permanently broken reviewer can
        // never trap the turn. Escape hatches (`.reviewgate-skip`,
        // REVIEWGATE_DISABLE=1) remain available throughout and are named in the
        // reason, satisfying the never-break-a-turn invariant. `Undetermined`
        // (the reviewer could not run to a conclusion) resolves here exactly
        // like the old `Error` arm did — never to Allow/Clean.
        Verdict::Undetermined(r) => {
            let e = r.as_str();
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                eprintln!(
                    "reviewgate: WARNING reviewer still unavailable after {max} attempt(s) \
                     ({e}) — allowing the stop UNREVIEWED. Fix reviewer_cmd (see \
                     `reviewgate status`) or set REVIEWGATE_DISABLE=1.",
                    max = cfg.max_attempts,
                    e = e,
                );
                return Decision::Allow {
                    tag: "reviewer-error-giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
            eprintln!(
                "reviewgate: WARNING reviewer command failed ({e}) — blocking the stop \
                 (reviewer unavailable, this is NOT a clean review). Attempt {attempts}/{max}.",
                e = e,
                attempts = attempts,
                max = cfg.max_attempts,
            );
            // Don't record the hash: keep re-checking whether the reviewer
            // recovered on the next stop; the attempt counter above bounds it.
            Decision::Block {
                reason: reviewer_unavailable_reason(e, attempts, cfg.max_attempts),
                tag: "reviewer-unavailable",
                files,
                attempts,
                last_hash: String::new(),
            }
        }
        Verdict::Clean(_) => Decision::Allow {
            tag: "clean",
            attempts: 0,
            last_hash: hash,
        },
        Verdict::Violation(r) => {
            let findings = r.as_str();
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                return Decision::Allow {
                    tag: "giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
            let reason = subprocess_reason(&files, findings, attempts, cfg.max_attempts);
            Decision::Block {
                reason,
                tag: "blocked-review",
                files,
                attempts,
                last_hash: hash,
            }
        }
    }
}

fn allow(tag: &'static str, st: &crate::state::SessionState) -> Decision {
    Decision::Allow {
        tag,
        attempts: 0,
        last_hash: st.last_hash.clone(),
    }
}

/// The diff was truncated to fit `max_diff_bytes`, so its tail is unreviewed.
/// Fail *closed* (but bounded): block the stop with a loud, escapable reason for
/// up to `max_attempts` consecutive stops (giving the agent a chance to split the
/// change so the diff fits), then give up *loudly* with a distinct tag so a
/// permanently-too-large diff never traps the turn and is never mistaken for a
/// clean review. Escape hatches (raise `max_diff_bytes`, `.reviewgate-skip`,
/// `REVIEWGATE_DISABLE=1`) stay available throughout and are named in the reason,
/// satisfying the never-break-a-turn invariant. Split out from `evaluate` so it
/// is unit-testable without spawning git.
fn decide_truncated(cfg: &Config, files: Vec<String>, prior_attempts: u32) -> Decision {
    let attempts = prior_attempts + 1;
    if attempts > cfg.max_attempts {
        eprintln!(
            "reviewgate: WARNING diff still exceeds max_diff_bytes ({max_bytes} B) after \
             {max} attempt(s) — allowing the stop with the truncated tail UNREVIEWED. Split the \
             change into smaller commits, raise max_diff_bytes, or set REVIEWGATE_DISABLE=1.",
            max_bytes = cfg.max_diff_bytes,
            max = cfg.max_attempts,
        );
        return Decision::Allow {
            tag: "truncated-giveup",
            attempts: 0,
            last_hash: String::new(),
        };
    }
    Decision::Block {
        reason: truncated_reason(cfg, &files, attempts, cfg.max_attempts),
        tag: "diff-truncated",
        files,
        attempts,
        // Deliberately don't record a hash: the hash can't cover the dropped
        // tail, so we must keep re-checking until the diff fits (or we give up
        // above) rather than let the "already-reviewed" path certify it.
        last_hash: String::new(),
    }
}

/// A `git` command errored inside a real repo, so the changed set is
/// UNDETERMINED — we have no reviewable diff we can trust. Fail *closed* (but
/// bounded), modeled exactly on the reviewer-outage / truncation guards: block
/// the stop with a loud, escapable reason for up to `max_attempts` consecutive
/// stops (giving a transient git error — lock contention, a slow mount — a
/// chance to clear), then give up *loudly* with a distinct tag so a persistently
/// broken git can never trap the turn. Escape hatches (`.reviewgate-skip`,
/// `REVIEWGATE_DISABLE=1`) stay available throughout and are named in the
/// reason, satisfying the never-break-a-turn invariant. No hash is recorded
/// (there is no diff to certify). Split out so it is unit-testable without git.
fn decide_scan_failed(cfg: &Config, prior_attempts: u32) -> Decision {
    let attempts = prior_attempts + 1;
    if attempts > cfg.max_attempts {
        eprintln!(
            "reviewgate: WARNING git scan still failing after {max} attempt(s) — allowing the \
             stop with the change set UNDETERMINED (unreviewed). Fix the git error (see \
             `reviewgate status`) or set REVIEWGATE_DISABLE=1.",
            max = cfg.max_attempts,
        );
        return Decision::Allow {
            tag: "git-scan-failed-giveup",
            attempts: 0,
            last_hash: String::new(),
        };
    }
    Decision::Block {
        reason: scan_failed_reason(attempts, cfg.max_attempts),
        tag: "git-scan-failed",
        files: vec![],
        attempts,
        // No diff to hash: keep re-checking on the next stop until git recovers
        // (or we give up above), never certify an undetermined change set.
        last_hash: String::new(),
    }
}

/// The git-scan failed: tell the agent the change set is undetermined and hand
/// it every way forward, so a broken git can neither slip an unreviewed change
/// through nor permanently trap the turn.
fn scan_failed_reason(attempt: u32, max: u32) -> String {
    format!(
        "🚧 reviewgate: 変更内容を特定できませんでした — `git` コマンドが失敗しました (round {attempt}/{max}).\n\n\
         git repo ではあるものの `git diff` / `git status` がエラー (spawn 失敗 / 非ゼロ終了) を返したため、\
         何が変更されたか判定できません。空の diff を「変更なし」と解釈して無言で通過させると、未レビューの\
         変更が gate をすり抜けてしまいます。判定不能な状態で停止を許可しないため、この停止を一時的に\
         ブロックしています。{max}回連続で解消しなければ警告を出して通過を許可します（永久にはブロックしません）。\n\n\
         前に進むには次のいずれか:\n\
         - git のエラーを解消する（`reviewgate status` で対象 repo を確認）。\n\
         - このレビューを1回だけスキップ: project root に `.reviewgate-skip` を作成（理由を1行）。\n\
         - reviewgate を完全に無効化: 環境変数 REVIEWGATE_DISABLE=1。",
        attempt = attempt,
        max = max,
    )
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

/// inject mode: ask the running agent to review its own diff.
fn inject_reason(cfg: &Config, files: &[String], attempt: u32) -> String {
    format!(
        "🔍 reviewgate: 完了前に、自分の変更をコードレビューしてください (round {attempt}/{max}).\n\n\
         レビュー対象 ({n} files):\n{list}\n\
         `git diff` で差分を確認し、次の観点でレビューしてください:\n{rubric}\n\n\
         実在する問題が見つかれば修正してから完了してください。\
         レビューの結果と対応を簡潔に報告すること。\
         修正不要・対応済みなら、そのまま完了して構いません（同じ差分での次の停止は許可されます）。\n\n\
         このレビューを1回だけスキップ: project root に `.reviewgate-skip` を作成（理由を1行）。\
         完全に無効化: 環境変数 REVIEWGATE_DISABLE=1。",
        attempt = attempt,
        max = cfg.max_attempts,
        n = files.len(),
        list = file_list(files),
        rubric = cfg.rubric,
    )
}

/// subprocess mode: inject the independent reviewer's findings.
fn subprocess_reason(files: &[String], findings: &str, attempt: u32, max: u32) -> String {
    format!(
        "🔍 reviewgate: 独立レビュアーが変更に問題を指摘しました (round {attempt}/{max}).\n\n\
         レビュー対象 ({n} files):\n{list}\n\
         --- 指摘 ---\n{findings}\n\
         ------------\n\n\
         妥当な指摘を修正してから完了してください。誤検知だと判断した指摘は、理由を述べてスキップして構いません。\n\n\
         このレビューを1回だけスキップ: `.reviewgate-skip` を作成。完全に無効化: REVIEWGATE_DISABLE=1。",
        attempt = attempt,
        max = max,
        n = files.len(),
        list = file_list(files),
        findings = findings.trim(),
    )
}

/// subprocess mode: the independent reviewer itself failed (couldn't spawn,
/// crashed, timed out, or emitted output we can't use). We block instead of
/// silently allowing, so the reason must always hand the human a way forward —
/// otherwise a broken reviewer would trap the turn.
fn reviewer_unavailable_reason(err: &str, attempt: u32, max: u32) -> String {
    format!(
        "🚧 reviewgate: 独立レビュアーを実行できませんでした (round {attempt}/{max}).\n\n\
         reviewer_cmd がエラー / タイムアウト / 解析不能でした:\n  {err}\n\n\
         これは「レビュー結果クリーン」ではありません。壊れた（またはハングした）レビュアーを\
         無言で通過させると gate がバイパスになってしまうため、この停止を一時的にブロックしています。\
         {max}回連続で失敗した場合は警告を出して通過を許可します（永久にはブロックしません）。\n\n\
         前に進むには次のいずれか:\n\
         - reviewer_cmd を修正する（`reviewgate status` で解決済みコマンドを確認）。\n\
         - このレビューを1回だけスキップ: project root に `.reviewgate-skip` を作成（理由を1行）。\n\
         - reviewgate を完全に無効化: 環境変数 REVIEWGATE_DISABLE=1。",
        attempt = attempt,
        max = max,
        err = err.trim(),
    )
}

/// The diff was truncated: tell the agent the tail went unreviewed and hand it
/// every way forward, so a too-large change can neither slip through unreviewed
/// nor permanently trap the turn.
fn truncated_reason(cfg: &Config, files: &[String], attempt: u32, max: u32) -> String {
    format!(
        "🚧 reviewgate: 変更差分が大きすぎてレビュー用に切り詰められました (round {attempt}/{max}).\n\n\
         diff が max_diff_bytes ({max_bytes} B) を超えたため、末尾がレビュー対象から欠落しています。\
         欠落した末尾は誰にもレビューされていないため、この停止を無言で許可すると未レビューの変更が\
         gate をすり抜けてしまいます。全 diff がレビューされることを保証するため、この停止を一時的に\
         ブロックしています。{max}回連続で解消しなければ警告を出して通過を許可します（永久にはブロックしません）。\n\n\
         レビュー対象 ({n} files):\n{list}\n\
         前に進むには次のいずれか:\n\
         - 変更を小さなコミット / 差分に分割し、それぞれが max_diff_bytes に収まるようにする。\n\
         - max_diff_bytes を引き上げる（reviewgate.toml の max_diff_bytes、現在 {max_bytes} B）。\n\
         - このレビューを1回だけスキップ: project root に `.reviewgate-skip` を作成（理由を1行）。\n\
         - reviewgate を完全に無効化: 環境変数 REVIEWGATE_DISABLE=1。",
        attempt = attempt,
        max = max,
        max_bytes = cfg.max_diff_bytes,
        n = files.len(),
        list = file_list(files),
    )
}

/// Run `reviewer_cmd`, feeding it the review prompt on stdin and reading
/// findings from stdout. Output that is empty or starts with "LGTM" = clean.
///
/// Returns a [`Verdict`]: `Clean` (ran, nothing to report), `Violation`
/// (findings), or `Undetermined` (the reviewer could not run to a conclusion —
/// spawn failure, non-zero exit with no output, timeout, wait error). An
/// `Undetermined` is **not** a clean review and must resolve to the blocking
/// side downstream — never Allow.
fn run_reviewer(cfg: &Config, diff: &str) -> Verdict {
    let prompt = format!(
        "あなたは独立した辛口のコードレビュアーです。以下の git diff をレビューしてください。\n\n\
         観点:\n{rubric}\n\n\
         実在し根拠のある問題だけを、深刻度(high/med/low)付きで簡潔に箇条書きしてください。\
         該当ファイルと行が分かるよう示すこと。問題が無ければ `LGTM` とだけ出力してください。\n\n\
         --- diff ---\n{diff}\n",
        rubric = cfg.rubric,
        diff = diff,
    );

    let mut cmd = build_command(&cfg.reviewer_cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Verdict::undetermined(format!("spawn: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
        // drop closes stdin so the reviewer sees EOF
    }

    let timeout = Duration::from_secs(cfg.reviewer_timeout_secs);
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let mut out = String::new();
            if let Some(mut so) = child.stdout.take() {
                use std::io::Read;
                let _ = so.read_to_string(&mut out);
            }
            if !status.success() && out.trim().is_empty() {
                return Verdict::undetermined(format!("exit {:?}", status.code()));
            }
            classify(&out)
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            Verdict::undetermined("timed out")
        }
        Err(e) => Verdict::undetermined(format!("wait: {e}")),
    }
}

fn classify(out: &str) -> Verdict {
    let t = out.trim();
    if t.is_empty() {
        return Verdict::from_findings(vec![]);
    }
    let first = t.lines().next().unwrap_or("").trim();
    if first.eq_ignore_ascii_case("lgtm") || first.to_ascii_lowercase().starts_with("lgtm") {
        return Verdict::from_findings(vec![]);
    }
    Verdict::violation(t.to_string())
}

/// Split a command line into program + args for `sh -c`-free direct spawn,
/// falling back to the shell for anything with shell metacharacters.
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
#[allow(clippy::panic)]
mod tests {
    use super::*;

    fn cfg_with(include: &[&str], exclude: &[&str]) -> Config {
        Config {
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            ..Config::default()
        }
    }

    #[test]
    fn include_exclude_filtering() {
        let cfg = cfg_with(&["**/*.rs"], &["**/target/**"]);
        let changed = vec![
            "src/main.rs".to_string(),
            "README.md".to_string(),
            "target/x.rs".to_string(),
        ];
        let r = reviewable_files(&cfg, &changed);
        assert_eq!(r, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn lockfile_excluded_by_default() {
        let cfg = Config::default();
        let changed = vec!["Cargo.lock".to_string(), "package-lock.json".to_string()];
        assert!(reviewable_files(&cfg, &changed).is_empty());
    }

    #[test]
    fn classify_lgtm_is_clean() {
        assert!(matches!(classify("LGTM"), Verdict::Clean(_)));
        assert!(matches!(classify("  lgtm \n"), Verdict::Clean(_)));
        assert!(matches!(classify(""), Verdict::Clean(_)));
        assert!(matches!(
            classify("- high: bug in foo.rs:10"),
            Verdict::Violation(_)
        ));
    }

    #[test]
    fn hash_is_stable_and_distinct() {
        assert_eq!(hash_diff("abc"), hash_diff("abc"));
        assert_ne!(hash_diff("abc"), hash_diff("abd"));
    }

    // --- reviewer failure must not become a silent bypass -------------------

    /// A reviewer that could not run (crash / spawn failure / unparseable
    /// output) is `Undetermined` and must BLOCK the stop, never allow it. This
    /// is the regression this gate exists to prevent: a broken reviewer turning
    /// into a bypass (fail-open). `Undetermined` must block exactly like the old
    /// `Error` arm did.
    #[test]
    fn reviewer_error_blocks_it_does_not_allow() {
        let cfg = Config::default(); // max_attempts = 2
        let d = decide_subprocess(
            &cfg,
            Verdict::undetermined("spawn: boom"),
            vec!["src/x.rs".to_string()],
            "deadbeefdeadbeef".to_string(),
            0,
        );
        match d {
            Decision::Allow { tag, .. } => {
                panic!("reviewer error must not allow (fail-open bypass); got allow tag={tag}")
            }
            Decision::Block { tag, reason, .. } => {
                assert_eq!(tag, "reviewer-unavailable");
                // The never-break-a-turn invariant: the reason must always hand
                // the human an escape path so a broken reviewer can't trap them.
                assert!(
                    reason.contains("REVIEWGATE_DISABLE"),
                    "reason must name the disable escape hatch: {reason}"
                );
            }
        }
    }

    /// A timeout is just another reviewer error and must likewise block, not
    /// allow — while still being bounded (see the giveup test below).
    #[test]
    fn reviewer_timeout_blocks_it_does_not_allow() {
        let cfg = Config::default();
        let d = decide_subprocess(
            &cfg,
            Verdict::undetermined("timed out"),
            vec!["src/x.rs".to_string()],
            "hash".to_string(),
            0,
        );
        assert!(
            matches!(d, Decision::Block { .. }),
            "a reviewer timeout must block the stop, not silently allow it"
        );
    }

    /// Bounded, never trapped: after `max_attempts` consecutive reviewer errors
    /// we give up and allow — but via a *distinct* tag (so logs never mistake it
    /// for a clean review), not the fail-open path we removed.
    #[test]
    fn reviewer_error_gives_up_after_max_attempts_but_never_traps() {
        let cfg = Config::default(); // max_attempts = 2
        let d = decide_subprocess(
            &cfg,
            Verdict::undetermined("still broken"),
            vec!["src/x.rs".to_string()],
            "hash".to_string(),
            cfg.max_attempts, // prior attempts already at the cap
        );
        match d {
            Decision::Allow { tag, .. } => assert_eq!(tag, "reviewer-error-giveup"),
            Decision::Block { .. } => {
                panic!("must give up after max_attempts so the turn is never permanently trapped")
            }
        }
    }

    /// End-to-end sanity on the classifier→undetermined boundary: a reviewer_cmd
    /// that cannot even spawn is reported as `Undetermined` (which the decision
    /// then blocks on), never mistaken for Clean.
    #[test]
    fn run_reviewer_reports_error_for_unspawnable_command() {
        let cfg = Config {
            reviewer_cmd: "reviewgate-no-such-binary-xyzzy".to_string(),
            ..Config::default()
        };
        match run_reviewer(&cfg, "diff --git a/x b/x\n") {
            Verdict::Undetermined(_) => {}
            Verdict::Clean(_) => {
                panic!(
                    "an unspawnable reviewer must be Undetermined, not Clean (that would bypass)"
                )
            }
            Verdict::Violation(_) => panic!("an unspawnable reviewer must be Undetermined"),
        }
    }

    // --- a truncated diff must not become a silent allow ---------------------

    /// A diff truncated to fit max_diff_bytes has an unreviewed tail. It must
    /// BLOCK the stop, never allow it — otherwise the dropped tail bypasses the
    /// gate (the hole this fix closes). The reason must always hand the human a
    /// way forward (never-break-a-turn), and no hash may be recorded (the hash
    /// can't cover the tail, so "already-reviewed" must not later certify it).
    #[test]
    fn truncated_diff_blocks_it_does_not_allow() {
        let cfg = Config::default(); // max_attempts = 2
        let d = decide_truncated(&cfg, vec!["src/x.rs".to_string()], 0);
        match d {
            Decision::Allow { tag, .. } => {
                panic!("a truncated diff must not be silently allowed (unreviewed-tail bypass); got allow tag={tag}")
            }
            Decision::Block {
                tag,
                reason,
                last_hash,
                ..
            } => {
                assert_eq!(tag, "diff-truncated");
                assert!(
                    last_hash.is_empty(),
                    "must not record a hash for a truncated diff, else already-reviewed could certify the unreviewed tail"
                );
                // Override paths must be named so a too-large diff can't trap the turn.
                assert!(
                    reason.contains("max_diff_bytes"),
                    "reason must name the raise-the-limit override: {reason}"
                );
                assert!(
                    reason.contains(".reviewgate-skip"),
                    "reason must name the one-shot skip escape hatch: {reason}"
                );
                assert!(
                    reason.contains("REVIEWGATE_DISABLE"),
                    "reason must name the disable escape hatch: {reason}"
                );
            }
        }
    }

    /// Bounded, never trapped: after max_attempts consecutive truncated stops we
    /// give up and allow — but via a *distinct* tag, so a permanently-too-large
    /// diff is never mistaken for a clean review and never traps the turn.
    #[test]
    fn truncated_diff_gives_up_after_max_attempts_but_never_traps() {
        let cfg = Config::default(); // max_attempts = 2
        let d = decide_truncated(&cfg, vec!["src/x.rs".to_string()], cfg.max_attempts);
        match d {
            Decision::Allow { tag, last_hash, .. } => {
                assert_eq!(tag, "truncated-giveup");
                assert!(last_hash.is_empty());
            }
            Decision::Block { .. } => {
                panic!("must give up after max_attempts so a too-large diff never permanently traps the turn")
            }
        }
    }

    // --- a failed git scan must not become a silent allow --------------------

    /// A git command that errored inside a real repo leaves the change set
    /// UNDETERMINED. It must BLOCK the stop, never allow it — otherwise a
    /// collapsed-empty scan bypasses the gate (the fail-open this fix closes).
    /// The reason must always hand the human a way forward (never-break-a-turn),
    /// and no hash may be recorded (there is no diff to certify).
    #[test]
    fn failed_git_scan_blocks_it_does_not_allow() {
        let cfg = Config::default(); // max_attempts = 2
        let d = decide_scan_failed(&cfg, 0);
        match d {
            Decision::Allow { tag, .. } => {
                panic!("a failed git scan must not be silently allowed (undetermined-change bypass); got allow tag={tag}")
            }
            Decision::Block {
                tag,
                reason,
                last_hash,
                ..
            } => {
                assert_eq!(tag, "git-scan-failed");
                assert!(
                    last_hash.is_empty(),
                    "must not record a hash for an undetermined change set"
                );
                assert!(
                    reason.contains(".reviewgate-skip"),
                    "reason must name the one-shot skip escape hatch: {reason}"
                );
                assert!(
                    reason.contains("REVIEWGATE_DISABLE"),
                    "reason must name the disable escape hatch: {reason}"
                );
            }
        }
    }

    /// Bounded, never trapped: after max_attempts consecutive failed scans we
    /// give up and allow — but via a *distinct* tag, so a persistently broken
    /// git is never mistaken for a clean review and never traps the turn.
    #[test]
    fn failed_git_scan_gives_up_after_max_attempts_but_never_traps() {
        let cfg = Config::default(); // max_attempts = 2
        let d = decide_scan_failed(&cfg, cfg.max_attempts);
        match d {
            Decision::Allow { tag, last_hash, .. } => {
                assert_eq!(tag, "git-scan-failed-giveup");
                assert!(last_hash.is_empty());
            }
            Decision::Block { .. } => {
                panic!("must give up after max_attempts so a broken git never permanently traps the turn")
            }
        }
    }
}
