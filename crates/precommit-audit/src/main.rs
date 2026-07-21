//! precommit-audit — a config-driven, cross-platform pre-commit static audit.
//!
//! Operates in two modes (like the PowerShell original it replaces):
//!   stop      — Claude Code Stop hook. Honors the subagent review contract.
//!               Exits 2 to feed findings back to the agent.
//!   precommit — pre-commit-framework / git hook on a human commit. Skips the
//!               review contract. Exits 1 on failure (pre-commit convention).
//!
//! Generic checks are built in; project-specific policy lives in a TOML config
//! (default `.precommit-audit.toml` at the repo root). No project name, path, or
//! rule is hard-coded into the binary.

mod checks;
mod classify;
mod config;
mod git;
mod hookio;
mod model;

use checks::Ctx;
use classify::Classifier;
use config::{Config, Severity};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::exit;

struct Args {
    config: Option<PathBuf>,
    mode: Option<String>,
    root: Option<PathBuf>,
    /// `precommit-audit trust`: add the root to the shared workspace-trust list
    /// so its `.precommit-audit.toml` (which can resolve repo-local linters) is
    /// honored. Until then a repo-shipped config is ignored.
    trust: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        config: None,
        mode: None,
        root: None,
        trust: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "trust" => a.trust = true,
            "--config" => a.config = it.next().map(PathBuf::from),
            "--mode" => a.mode = it.next(),
            "--root" => a.root = it.next().map(PathBuf::from),
            "--version" | "-V" => {
                println!("precommit-audit {}", env!("CARGO_PKG_VERSION"));
                exit(0);
            }
            "--help" | "-h" => {
                print_help();
                exit(0);
            }
            other => {
                eprintln!("precommit-audit: unknown argument '{other}' (try --help)");
                exit(64);
            }
        }
    }
    a
}

fn print_help() {
    println!(
        "precommit-audit {ver}\n\
Config-driven pre-commit static audit (cross-platform).\n\n\
USAGE:\n  precommit-audit [--mode stop|precommit] [--config <file>] [--root <dir>]\n  precommit-audit trust   (trust <root> so its .precommit-audit.toml is honored)\n\n\
OPTIONS:\n  --mode <m>     stop (default) or precommit\n  --config <f>   config file (default: <root>/.precommit-audit.toml)\n  --root <d>     repo root (default: $CLAUDE_PROJECT_DIR, else git toplevel)\n  -V, --version  print version\n  -h, --help     this help\n\n\
EXIT: 0 clean | 1 blocked (precommit) | 2 blocked (stop)",
        ver = env!("CARGO_PKG_VERSION")
    );
}

fn resolve_root(arg: Option<PathBuf>) -> PathBuf {
    if let Some(r) = arg {
        return r;
    }
    if let Ok(p) = std::env::var("CLAUDE_PROJECT_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Some(top) = git::toplevel(&cwd) {
        return PathBuf::from(top);
    }
    cwd
}

fn resolve_mode(arg: Option<String>) -> String {
    arg.or_else(|| std::env::var("AUDIT_MODE").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "stop".to_string())
}

/// Whether the auto-discovered project `.precommit-audit.toml` must be ignored.
/// Only the AUTO-DISCOVERED file is gated: an `explicit` `--config` is the
/// operator's deliberate choice and is always honored. An absent file or a
/// trusted root loads normally; an untrusted repo file is blocked (the one
/// execution vector is `linters.node_projects` resolving repo-local binaries).
fn project_config_blocked(explicit: bool, exists: bool, trusted: bool) -> bool {
    !explicit && exists && !trusted
}

/// One-shot notice (per process) that the auto-discovered project config was
/// ignored because the root isn't trusted. Best effort — never blocks.
fn warn_untrusted_once(config_path: &Path) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "precommit-audit: {} can resolve repo-local linters but this project is not trusted; \
         ignoring it (using built-in checks only). Run 'precommit-audit trust' to enable.",
        config_path.display()
    );
}

fn main() {
    // never-break-a-turn: a panic while scanning changed files (unexpected bytes,
    // linter subprocess quirks, …) must not break the turn with a backtrace. Real
    // `exit(...)` calls inside `run` terminate directly; only a genuine panic
    // unwinds here.
    //
    // fail-closed (99b506b7-sibling, blastguard#3 twin): a caught panic used to
    // fall back to `exit(0)` — but exit 0 IS a clean/allow verdict, so any crash
    // in the audit reported the working set it had just FAILED to scan as clean.
    // A panic means the audit did not finish, which is the SAME "cannot vouch for
    // this working set" condition as the git-error and incomplete-scan paths in
    // `run`, both of which already fail CLOSED (block; never exit 0). So a caught
    // panic now exits with the mode's blocking code (stop→2, precommit→1), never
    // 0. Under SessionEnd a non-zero exit cannot block by platform design, but it
    // surfaces as a failed hook — strictly better than a silent clean pass, and
    // exactly what the git-error path already does (its `exit(3)` fallback).
    if std::panic::catch_unwind(run).is_err() {
        exit(blocking_exit_for_mode(&mode_from_env_argv()));
    }
}

/// The blocking exit code to use when the audit could not complete: `stop`
/// (and its SessionEnd variant) → 2, `precommit` → 1. Pure so the fail-closed
/// contract is unit-testable without spawning a process.
fn blocking_exit_for_mode(mode: &str) -> i32 {
    if mode == "precommit" {
        1
    } else {
        2
    }
}

/// Resolve the mode from argv/env ONLY — never reads stdin (which `run` has
/// already consumed) and does nothing that can itself panic, so it is safe to
/// call from the panic-unwound path in `main`. Mirrors `resolve_mode`'s
/// precedence (explicit `--mode` over `AUDIT_MODE`) without the parsed-args
/// plumbing.
fn mode_from_env_argv() -> String {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--mode") {
        if let Some(m) = args.get(pos + 1) {
            return m.clone();
        }
    }
    std::env::var("AUDIT_MODE").unwrap_or_default()
}

fn run() {
    let args = parse_args();

    // Test-only fault injection to exercise the panic->block barrier in `main`
    // deterministically (there is otherwise no reliable way to induce a panic
    // mid-audit from a test). Compiled OUT of release builds, so it adds no
    // production surface; a panic here is caught by `main` and fails closed.
    #[cfg(debug_assertions)]
    if std::env::var_os("PRECOMMIT_AUDIT_FORCE_PANIC").is_some() {
        panic!("forced panic (fault injection) exercising the panic->block barrier");
    }

    // `precommit-audit trust`: register this root in the shared trust list, then
    // exit. Honors the same `harness_core::trust` store as donegate/reviewgate/tdd.
    // Handled before any stdin read so it works as a plain manual command.
    if args.trust {
        let root = resolve_root(args.root);
        match harness_core::trust::add(&root) {
            Ok(key) => {
                println!("trusted {}", key.display());
                println!(
                    "precommit-audit will now honor {}.",
                    root.join(".precommit-audit.toml").display()
                );
                exit(0);
            }
            Err(e) => {
                eprintln!("precommit-audit: failed to trust {}: {e}", root.display());
                exit(1);
            }
        }
    }

    // Recursion guard: a Stop hook that re-fires within the same stop cycle.
    let hook = hookio::read_stdin();
    if hook.stop_hook_active {
        exit(0);
    }

    let root = resolve_root(args.root);

    let mode = resolve_mode(args.mode);
    // Exit code that signals "blocking issues found". On the Stop hook, 2 blocks
    // the stop; on a git pre-commit invocation, 1 aborts the commit. But under
    // SessionEnd (where this hook now runs as a side effect) a non-zero exit
    // CANNOT block — Claude Code reports it as a *failed hook*. So when invoked
    // from SessionEnd we still run the audit (markers + log are useful), but
    // never surface a blocking exit code.
    let fail_exit = if hook.event == "SessionEnd" {
        0
    } else if mode == "precommit" {
        1
    } else {
        2
    };

    // The repo-shipped `.precommit-audit.toml` can resolve repo-local linter
    // binaries (`linters.node_projects` → eslint/tsc), so a cloned untrusted
    // checkout is an execution vector. Gate the AUTO-DISCOVERED project config
    // behind `harness_core::trust` (same pattern as donegate/reviewgate/tdd):
    // when the root isn't trusted, ignore the repo file and use built-in
    // defaults. An explicit `--config` is the operator's deliberate choice and
    // is always honored.
    let explicit_config = args.config.is_some();
    let config_path = args
        .config
        .unwrap_or_else(|| root.join(".precommit-audit.toml"));
    let cfg = if project_config_blocked(
        explicit_config,
        config_path.exists(),
        harness_core::trust::is_trusted(&root),
    ) {
        warn_untrusted_once(&config_path);
        Config::default()
    } else {
        match Config::load(&config_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("precommit-audit: {e}");
                exit(64);
            }
        }
    };

    // One-shot skip escape hatch.
    if let Some(reason) = hookio::consume_skip(&root, &cfg.audit_dir) {
        eprintln!("pre-commit audit SKIPPED (one-shot) -- reason: {reason}");
        exit(0);
    }

    let mut files = match git::changed_and_untracked(&root) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("precommit-audit: cannot determine the changed-files set: {e}");
            eprintln!(
                "  refusing to report a clean audit on an unknown working set (fail-closed)."
            );
            // Block in stop/precommit; under SessionEnd (fail_exit == 0, which cannot
            // block by platform design) still exit non-zero so the failure surfaces as
            // a failed hook rather than a silent clean pass. NEVER exit(0) on a git error.
            exit(if fail_exit != 0 { fail_exit } else { 3 });
        }
    };
    // Never audit our own config file: it literally contains the rule patterns,
    // which would otherwise self-trigger on the commit that introduces it.
    if let Ok(rel) = config_path.strip_prefix(&root) {
        let cfg_rel = classify::norm(&rel.to_string_lossy());
        files.retain(|f| classify::norm(f) != cfg_rel);
    }
    if files.is_empty() {
        exit(0);
    }

    let classifier = Classifier::new(&cfg.classify);
    let incomplete = RefCell::new(Vec::new());
    let ctx = Ctx {
        root: &root,
        cfg: &cfg,
        cls: &classifier,
        files: &files,
        incomplete: &incomplete,
    };

    let mut issues: Vec<model::Issue> = Vec::new();
    checks::run_static_checks(&ctx, &mut issues);
    if cfg.checks.linters {
        checks::linters::run(&ctx, &mut issues);
    }
    if mode == "stop" {
        if let Some(i) = checks::review::check(&ctx) {
            issues.push(i);
        }
    }

    // Fail-closed: if any git error prevented a complete scan, treat the audit as
    // blocking rather than reporting a pass over content it could not read.
    let incomplete = incomplete.into_inner();
    if !incomplete.is_empty() {
        issues.push(model::Issue::block(
            "AUDIT INCOMPLETE",
            format!(
                "AUDIT INCOMPLETE: git error(s) prevented a full scan; treating as blocking (fail-closed):\n  {}",
                incomplete.join("\n  ")
            ),
        ));
    }

    emit_and_exit(&root, &cfg, &mode, fail_exit, &files, issues);
}

/// The reporter's decision, computed as a *pure value* (no IO, no `exit`) so
/// the advisory-mode contract can be unit-tested. `emit_and_exit` turns this
/// plan into actual stderr writes, marker/log side effects, and a process exit.
struct Emission {
    /// Full stderr payload to print. Empty string means stay completely silent.
    stderr: String,
    /// Audit-log verdict: `"pass"` or `"block"`.
    verdict: &'static str,
    /// Sorted, de-duplicated blocking categories, for the audit log.
    categories: Vec<String>,
    /// Whether to drop the on-disk block marker (vs. clear it).
    set_marker: bool,
    /// Process exit code. Held at 0 in advisory mode even when blocking.
    exit_code: i32,
    blocking_count: usize,
    warning_count: usize,
}

/// Decide what to print, log, and exit with — without doing any of it.
///
/// never-break-a-turn invariant: under SessionEnd this is called with
/// `fail_exit == 0` (see `run`). Claude Code reports a non-zero hook exit as a
/// *failed hook*, which would break the turn, so a blocking finding must NEVER
/// raise the exit code in advisory mode. The advisory contract is
/// "visualize + record, never block": we therefore still surface the finding
/// prominently on stderr AND record a `"block"` verdict + marker in the audit
/// log, while keeping `exit_code == 0`. Blocking findings are never swallowed.
fn plan_emission(
    blocking: &[model::Issue],
    warnings: &[model::Issue],
    fail_exit: i32,
    mode: &str,
) -> Emission {
    let mut out = String::new();

    // Advisory warnings always print, never affect the exit code.
    if !warnings.is_empty() {
        let body = warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        out.push_str("=== pre-commit audit WARNING (non-blocking) ===\n\n");
        out.push_str(&body);
    }

    if blocking.is_empty() {
        return Emission {
            stderr: out,
            verdict: "pass",
            categories: Vec::new(),
            set_marker: false,
            exit_code: 0,
            blocking_count: 0,
            warning_count: warnings.len(),
        };
    }

    let mut categories: Vec<String> = blocking.iter().map(|i| i.category.clone()).collect();
    categories.sort();
    categories.dedup();

    let body = blocking
        .iter()
        .map(|i| i.message.clone())
        .collect::<Vec<_>>()
        .join("\n\n");
    let header = if fail_exit == 0 {
        // SessionEnd: advisory only. never-break-a-turn means the non-zero exit
        // that would normally block is suppressed here — but the finding is NOT
        // silently dropped: it is printed prominently and logged as a "block"
        // verdict below, so the user and the audit log both still see it.
        "=== pre-commit audit found BLOCKING issues (advisory; session ended -- NOT suppressed) ==="
    } else if mode == "precommit" {
        "=== pre-commit audit BLOCKED the git commit ==="
    } else {
        "=== pre-commit audit BLOCKED the auto-commit ==="
    };
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(header);
    out.push_str("\n\n");
    out.push_str(&body);
    out.push_str(
        "\n\n\
Required actions:\n  1. Fix the code or add the missing test for each issue above.\n  2. Or suppress with a reasoned marker (reason REQUIRED):\n       Per-line:  append  '# audit-ignore: <reason>'  (use // for JS/TS)\n       Per-file:  add     'audit-ignore-file: <reason>'  in the first 20 lines\n  3. Re-run to re-check.",
    );

    Emission {
        stderr: out,
        verdict: "block",
        categories,
        set_marker: true,
        exit_code: fail_exit,
        blocking_count: blocking.len(),
        warning_count: warnings.len(),
    }
}

fn emit_and_exit(
    root: &Path,
    cfg: &Config,
    mode: &str,
    fail_exit: i32,
    files: &[String],
    issues: Vec<model::Issue>,
) {
    let (blocking, warnings): (Vec<_>, Vec<_>) = issues
        .into_iter()
        .partition(|i| i.severity == Severity::Block);

    let plan = plan_emission(&blocking, &warnings, fail_exit, mode);

    if !plan.stderr.is_empty() {
        eprintln!("{}", plan.stderr);
    }

    if plan.set_marker {
        hookio::set_block_marker(root, &cfg.audit_dir);
    } else {
        hookio::clear_block_marker(root, &cfg.audit_dir);
    }

    let ts = now_iso8601();
    hookio::write_audit_log(
        root,
        &cfg.audit_dir,
        mode,
        plan.verdict,
        plan.blocking_count,
        &plan.categories,
        plan.warning_count,
        files.len(),
        &ts,
    );

    exit(plan.exit_code);
}

/// UTC ISO-8601 timestamp (e.g. 2026-06-12T10:30:00Z) without pulling in a date
/// crate. Uses Howard Hinnant's civil-from-days algorithm.
fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // civil_from_days
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod advisory_emission_tests {
    use super::model::Issue;
    use super::plan_emission;

    // SessionEnd runs the audit with fail_exit == 0. A blocking finding must be
    // surfaced (stderr) and recorded (log verdict "block") without ever raising
    // the exit code — the never-break-a-turn invariant. The bug this guards was
    // advisory mode silently swallowing blocking findings.
    #[test]
    fn advisory_surfaces_blocking_but_never_exits_nonzero() {
        let blocking = vec![Issue::block(
            "SECRET",
            "hardcoded API token in src/foo.rs:12".to_string(),
        )];
        let warnings: Vec<Issue> = Vec::new();

        let plan = plan_emission(&blocking, &warnings, 0, "stop");

        // never-break-a-turn: exit stays 0 even though a block was found.
        assert_eq!(plan.exit_code, 0);
        // but the finding is NOT swallowed: it is on stderr...
        assert!(
            plan.stderr.contains("hardcoded API token in src/foo.rs:12"),
            "blocking message must reach stderr in advisory mode"
        );
        assert!(
            plan.stderr.to_lowercase().contains("advisory"),
            "advisory header must make the non-blocking nature explicit"
        );
        // ...and it is recorded in the audit log as a real block.
        assert_eq!(plan.verdict, "block");
        assert!(plan.set_marker);
        assert_eq!(plan.blocking_count, 1);
        assert_eq!(plan.categories, vec!["SECRET".to_string()]);
    }

    // With zero findings the reporter must stay completely silent.
    #[test]
    fn no_findings_is_silent() {
        let none: Vec<Issue> = Vec::new();

        let plan = plan_emission(&none, &none, 0, "stop");

        assert!(plan.stderr.is_empty(), "no findings must produce no output");
        assert_eq!(plan.verdict, "pass");
        assert!(!plan.set_marker);
        assert_eq!(plan.exit_code, 0);
        assert_eq!(plan.blocking_count, 0);
    }

    // Non-advisory modes (Stop=2, precommit=1) must still block via exit code.
    #[test]
    fn blocking_modes_still_exit_nonzero() {
        let blocking = vec![Issue::block("SECRET", "boom".to_string())];
        let none: Vec<Issue> = Vec::new();

        let stop = plan_emission(&blocking, &none, 2, "stop");
        assert_eq!(stop.exit_code, 2);
        assert!(stop.stderr.contains("BLOCKED the auto-commit"));
        assert_eq!(stop.verdict, "block");

        let pc = plan_emission(&blocking, &none, 1, "precommit");
        assert_eq!(pc.exit_code, 1);
        assert!(pc.stderr.contains("BLOCKED the git commit"));
    }

    // Advisory warnings alone print but never block and log a pass verdict.
    #[test]
    fn warnings_alone_print_without_blocking() {
        let none: Vec<Issue> = Vec::new();
        let warnings = vec![Issue::warn("STYLE", "trailing whitespace".to_string())];

        let plan = plan_emission(&none, &warnings, 0, "stop");

        assert!(plan.stderr.contains("trailing whitespace"));
        assert!(plan.stderr.contains("WARNING (non-blocking)"));
        assert_eq!(plan.verdict, "pass");
        assert!(!plan.set_marker);
        assert_eq!(plan.exit_code, 0);
        assert_eq!(plan.warning_count, 1);
    }
}

#[cfg(test)]
mod panic_block {
    //! Regression: backlog 99b71596 ("fail-open #9") — a caught panic in `main`
    //! must exit with the mode's BLOCKING code, never 0. Previously a caught
    //! panic fell back to `exit(0)`, which IS a clean/allow verdict, so any
    //! crash mid-audit reported the un-scanned working set as clean (fail-open).
    //! This pins panic→block (mirrors blastguard#3 panic→Deny), NOT panic→exit0.
    //!
    //! These unit-test the pure `blocking_exit_for_mode` seam directly; the
    //! end-to-end panic barrier is exercised by `tests/panic_fails_closed.rs`.
    use super::blocking_exit_for_mode;

    #[test]
    fn stop_mode_blocks_with_two() {
        assert_eq!(blocking_exit_for_mode("stop"), 2);
    }

    #[test]
    fn precommit_mode_blocks_with_one() {
        assert_eq!(blocking_exit_for_mode("precommit"), 1);
    }

    #[test]
    fn default_empty_mode_blocks_with_two() {
        // No `--mode`/`AUDIT_MODE` resolves to an empty string in the
        // panic-unwound path (`mode_from_env_argv`); it must fail closed as stop.
        assert_eq!(blocking_exit_for_mode(""), 2);
    }

    #[test]
    fn session_end_variant_blocks_with_two() {
        // The SessionEnd variant is not "precommit", so it takes the stop code.
        assert_eq!(blocking_exit_for_mode("SessionEnd"), 2);
    }

    // The core invariant in one place: NO mode maps a panic to exit 0.
    #[test]
    fn no_mode_maps_panic_to_exit_zero() {
        for mode in ["stop", "precommit", "", "SessionEnd", "anything-else"] {
            assert_ne!(
                blocking_exit_for_mode(mode),
                0,
                "mode {mode:?} must never map a caught panic to exit 0 (fail-open #9)"
            );
        }
    }
}

#[cfg(test)]
mod trust_gate_tests {
    use super::project_config_blocked;

    #[test]
    fn auto_config_blocked_only_when_untrusted_and_present() {
        // auto-discovered, present, untrusted: blocked (the security case).
        assert!(project_config_blocked(false, true, false));
        // trusted root: honored.
        assert!(!project_config_blocked(false, true, true));
        // absent file: nothing to block.
        assert!(!project_config_blocked(false, false, false));
        // explicit --config is the operator's choice: always honored.
        assert!(!project_config_blocked(true, true, false));
    }
}
