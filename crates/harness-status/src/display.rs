//! Terminal-friendly display of the full HOTL status.

use crate::budget::BudgetStatus;
use crate::hooks::{sess8, HookLatencyReport};
use crate::hooks_health::HooksHealthReport;
use crate::inject::{key8, InjectReport};
use crate::path_shadow::ShadowedBinary;
use crate::progress::ProgressStatus;
use crate::sessions::SessionSummary;

/// All the per-panel reports the default (no-subcommand) view aggregates,
/// bundled into one struct so `print_status`/`print_json` take a manageable
/// argument count regardless of how many panels harness-status grows.
pub struct StatusReport<'a> {
    pub today: &'a str,
    pub budget: &'a BudgetStatus,
    pub sessions: &'a [SessionSummary],
    pub progress: &'a ProgressStatus,
    pub hooks: &'a HookLatencyReport,
    pub inject: &'a InjectReport,
    pub hooks_health: &'a HooksHealthReport,
    pub path_shadow: &'a [ShadowedBinary],
}

pub fn print_status(report: &StatusReport, cwd_display: &str) {
    let StatusReport {
        today,
        budget,
        sessions,
        progress,
        hooks,
        inject,
        hooks_health,
        path_shadow,
    } = *report;
    println!("╔══════════════════════════════════════════════╗");
    println!("║         harness-status  ({today})         ║");
    println!("╚══════════════════════════════════════════════╝");
    println!();

    // Budget section
    println!("── Budget (budgetguard) ──────────────────────────");
    if budget.ledger_present {
        println!(
            "  Today spend:  ${:.4}  ({} session(s))",
            budget.today_usd, budget.session_count_today
        );
    } else {
        println!("  ledger.json not found — budgetguard not installed?");
    }
    println!();

    // Recent sessions
    println!("── Recent sessions (gauge) ───────────────────────");
    if sessions.is_empty() {
        println!("  No session records found — gauge not installed?");
    } else {
        println!(
            "  {:<16} {:<20} {:>6} {:>12} {:>9}",
            "Session", "Project", "Turns", "Tokens", "Cost USD"
        );
        println!("  {}", "-".repeat(70));
        for s in sessions {
            let id8 = s.session_id.get(..8).unwrap_or(&s.session_id);
            let proj = truncate(&s.project, 20);
            println!(
                "  {:<16} {:<20} {:>6} {:>12} {:>9.4}",
                id8, proj, s.turns, s.total_tokens, s.cost_usd
            );
        }
    }
    println!();

    // Progress file
    println!("── Progress file (taskprog) ──────────────────────");
    println!("  cwd: {cwd_display}");
    if progress.exists {
        println!("  {}", progress.path);
        if let Some(preview) = &progress.preview {
            println!();
            for line in preview.lines() {
                println!("  │ {line}");
            }
        }
    } else {
        println!("  [not found] {}", progress.path);
        println!("  Run `taskprog init` or `/taskprog` to create one.");
    }
    println!();

    // Stop-hook latency (only the 3 heavy 600s gates record; see the contract).
    println!("── Stop-hook latency (donegate/reviewgate/propguard) ──");
    if hooks.sessions.is_empty() {
        println!("  [no Stop-hook latency recorded]");
    } else {
        println!("  budget: {}ms", hooks.budget_ms);
        for s in &hooks.sessions {
            let flag = if s.over_budget {
                "  ⚠ OVER BUDGET"
            } else {
                ""
            };
            println!(
                "  {} | {}ms across {} hooks{}",
                sess8(&s.session),
                s.total_ms,
                s.per_hook.len(),
                flag
            );
        }
    }
    println!();

    // UserPromptSubmit injection size (ADR 0001 Phase 2): the five injectors
    // (playbook/runbook/ctxrot/context-governor/fugu-router) record post-cap
    // injected char size per turn; warn when the combined size exceeds budget.
    println!("── UserPromptSubmit injection (aggregate budget) ──");
    if inject.turns.is_empty() {
        println!("  [no UserPromptSubmit injections recorded]");
    } else {
        println!("  budget: {} chars", inject.budget_chars);
        for t in &inject.turns {
            let flag = if t.over_budget {
                "  ⚠ OVER BUDGET"
            } else {
                ""
            };
            println!(
                "  {} | {} chars across {} injectors{}",
                key8(&t.turn_key),
                t.total_chars,
                t.per_plugin.len(),
                flag
            );
        }
    }
    println!();

    // Hook binary health: registered hooks in ~/.claude/settings.json whose
    // command binary no longer exists on disk (stale rollout, pruned cache, …).
    println!("── Hook binary health (settings.json) ────────────");
    if !hooks_health.settings_found {
        println!(
            "  [no settings.json found at {}]",
            hooks_health.settings_path
        );
    } else if hooks_health.missing.is_empty() {
        println!("  all registered hook binaries present");
    } else {
        for m in &hooks_health.missing {
            println!(
                "  ⚠ MISSING BINARY  {} | {} | {}",
                m.event, m.binary_path, m.command
            );
        }
    }
    println!();

    // PATH shadowing: a stray standalone binary (e.g. a stale ~/.cargo/bin
    // copy) resolving before the up-to-date plugin-cache copy on bare-name
    // PATH lookup.
    println!("── PATH shadowing (stray binaries) ───────────────");
    if path_shadow.is_empty() {
        println!("  no PATH-shadowed plugin binaries");
    } else {
        for s in path_shadow {
            println!(
                "  ⚠ SHADOWED  {} | {} shadows {}",
                s.name, s.shadowing_path, s.cache_path
            );
        }
    }
    println!();
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

pub fn print_json(report: &StatusReport) {
    let out = serde_json::json!({
        "date": report.today,
        "budget": report.budget,
        "recent_sessions": report.sessions,
        "progress": report.progress,
        "hook_latency": report.hooks,
        "inject": report.inject,
        "hooks_health": report.hooks_health,
        "path_shadow": report.path_shadow,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}
