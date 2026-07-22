mod budget;
mod display;
mod hooks;
mod hooks_health;
mod inject;
mod path_shadow;
mod plugins;
mod progress;
mod sessions;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "harness-status",
    about = "Unified HOTL status across all harness plugins"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// How many recent sessions to show
    #[arg(long, default_value = "5", global = true)]
    sessions: usize,
}

#[derive(Subcommand)]
enum Command {
    /// Show budget information only
    Budget,
    /// Show recent sessions only
    Sessions,
    /// Show progress file only
    Progress,
    /// Show Stop-hook latency aggregation only
    Hooks,
    /// Show UserPromptSubmit injection-size aggregation only
    Inject,
    /// Classify all plugins by activation scope
    Plugins,
    /// Check registered hooks in ~/.claude/settings.json for missing binaries
    HooksHealth,
    /// Check whether a stray PATH binary (e.g. a stale ~/.cargo/bin copy)
    /// shadows a plugin-cache binary of the same name
    PathShadow,
    /// SessionStart hook: warn (via additionalContext) only when a registered
    /// hook binary is missing; silent otherwise. Never breaks the turn.
    SessionStart,
}

fn today() -> String {
    // Read from env (testable) or derive the date directly from the system clock.
    // Production: callers can set HARNESS_DATE=YYYY-MM-DD for testing.
    if let Ok(d) = std::env::var("HARNESS_DATE") {
        return d;
    }
    // Derive the date from the system clock. This previously wrote an empty file at
    // a FIXED, world-writable path (std::env::temp_dir()/.harness-status-date) and
    // read back its mtime as a poor-man's clock — a predictable /tmp name is a
    // symlink/TOCTOU surface. SystemTime::now() yields the same day with no
    // filesystem access and no added dependency (still chrono-free).
    if let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        // Compute Gregorian date from days since epoch (1970-01-01).
        return days_to_date(duration.as_secs() / 86400);
    }
    "unknown".to_string()
}

fn days_to_date(days: u64) -> String {
    // Simple Gregorian calendar calculation.
    let mut y = 1970u32;
    let mut d = days as u32;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if d < days_in_year {
            break;
        }
        d -= days_in_year;
        y += 1;
    }
    let months = [
        31u32,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 1u32;
    for &mdays in &months {
        if d < mdays {
            break;
        }
        d -= mdays;
        m += 1;
    }
    format!("{y:04}-{m:02}-{:02}", d + 1)
}

fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn main() {
    let cli = Cli::parse();
    let today = today();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    match cli.command {
        Some(Command::Budget) => {
            let b = budget::read(&today);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&b).unwrap_or_default());
            } else {
                println!(
                    "Today ({}): ${:.4} across {} session(s)",
                    today, b.today_usd, b.session_count_today
                );
            }
        }
        Some(Command::Sessions) => {
            let s = match sessions::recent(cli.sessions) {
                harness_core::verdict::Determination::Known(s) => s,
                // Silence here would read as "no sessions". Say unknown and
                // exit non-zero so a script cannot mistake it for an empty run.
                harness_core::verdict::Determination::Undetermined(why) => {
                    if cli.json {
                        println!(
                            "{}",
                            serde_json::json!({"status": "unknown", "reason": why.as_str()})
                        );
                    } else {
                        eprintln!(
                            "sessions: unknown — gauge's session store could not be read: {why}"
                        );
                    }
                    std::process::exit(1);
                }
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
            } else {
                for sess in &s {
                    println!(
                        "{} | {} | {} turns | ${:.4}",
                        sess.session_id.get(..8).unwrap_or(&sess.session_id),
                        sess.project,
                        sess.turns,
                        sess.cost_usd
                    );
                }
            }
        }
        Some(Command::Progress) => {
            let p = progress::read(&cwd);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default());
            } else if p.exists {
                println!("{}", p.preview.as_deref().unwrap_or("(empty)"));
            } else {
                println!("[no progress file] {}", p.path);
            }
        }
        Some(Command::Hooks) => {
            let h = hooks::read();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&h).unwrap_or_default());
            } else if h.sessions.is_empty() {
                println!("[no Stop-hook latency recorded]");
            } else {
                for sess in &h.sessions {
                    println!(
                        "{} | {}ms across {} hooks",
                        hooks::sess8(&sess.session),
                        sess.total_ms,
                        sess.per_hook.len()
                    );
                }
                for sess in &h.sessions {
                    if sess.over_budget {
                        println!(
                            "⚠ session {} Stop-hook total {}ms exceeds budget {}ms",
                            hooks::sess8(&sess.session),
                            sess.total_ms,
                            h.budget_ms
                        );
                    }
                }
            }
        }
        Some(Command::Inject) => {
            let i = inject::read();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&i).unwrap_or_default());
            } else if i.turns.is_empty() {
                println!("[no UserPromptSubmit injections recorded]");
            } else {
                for t in &i.turns {
                    println!(
                        "{} | {} chars across {} injectors",
                        inject::key8(&t.turn_key),
                        t.total_chars,
                        t.per_plugin.len()
                    );
                }
                for t in &i.turns {
                    if t.over_budget {
                        println!(
                            "⚠ turn {} injection total {} chars exceeds budget {}",
                            inject::key8(&t.turn_key),
                            t.total_chars,
                            i.budget_chars
                        );
                    }
                }
            }
        }
        Some(Command::Plugins) => {
            let root = plugins::find_repo_root(&cwd);
            match plugins::report(&root) {
                Err(e) => {
                    eprintln!("harness-status: could not scan {}: {e}", root.join("crates").display());
                    std::process::exit(1);
                }
                Ok(r) => {
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
                    } else {
                        let section = |title: &str, items: &[plugins::PluginInfo]| {
                            println!("{} ({})", title, items.len());
                            for p in items {
                                println!("  {}  —  {}", p.name, p.trigger);
                            }
                            println!();
                        };
                        section("ALWAYS-ON", &r.always_on);
                        section("EVENT-SCOPED", &r.event_scoped);
                        section("MANUAL", &r.manual);
                    }
                }
            }
        }
        Some(Command::HooksHealth) => {
            let r = hooks_health::read();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
            } else if !r.settings_found {
                println!("[no settings.json found at {}]", r.settings_path);
            } else if r.missing.is_empty() {
                println!("[all registered hook binaries present]");
            } else {
                for m in &r.missing {
                    println!(
                        "⚠ missing hook binary: {} ({}): {}",
                        m.event, m.binary_path, m.command
                    );
                }
            }
        }
        Some(Command::PathShadow) => match path_shadow::detect() {
            harness_core::verdict::Determination::Undetermined(why) => {
                eprintln!("harness-status: PATH-shadow scan could not be completed: {why}");
                std::process::exit(1);
            }
            harness_core::verdict::Determination::Known(shadowed) => {
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&shadowed).unwrap_or_default()
                    );
                } else if shadowed.is_empty() {
                    println!("[no PATH-shadowed plugin binaries]");
                } else {
                    for s in &shadowed {
                        println!(
                            "⚠ shadowed binary: {} — {} shadows plugin cache {}",
                            s.name, s.shadowing_path, s.cache_path
                        );
                    }
                }
            }
        },
        Some(Command::SessionStart) => {
            harness_core::hook::run_hook(|| {
                // Read (and discard) stdin only if actually piped, mirroring the
                // fail-soft, non-blocking pattern other plugins' SessionStart hooks
                // use; this hook doesn't need the payload, just needs to be a good
                // citizen w.r.t. stdin handling.
                let _ = harness_core::hook::read_stdin_if_piped();
                let r = hooks_health::read();
                let shadowed = path_shadow::detect();
                let mut sections = Vec::new();
                if !r.missing.is_empty() {
                    let lines: Vec<String> = r
                        .missing
                        .iter()
                        .map(|m| format!("  ⚠ {} ({}): {}", m.event, m.binary_path, m.command))
                        .collect();
                    sections.push(format!(
                        "harness-status: {} 件の登録済みhookのbinaryが見つかりません（rollout未実行の可能性）:\n{}\n`harness-status hooks-health` で詳細確認、`scripts/rollout-plugins.sh` で反映してください。",
                        r.missing.len(),
                        lines.join("\n")
                    ));
                }
                match shadowed {
                    harness_core::verdict::Determination::Undetermined(why) => {
                        sections.push(format!(
                            "harness-status: PATH-shadow scan が完了しませんでした（判定不能）: {why}\n`harness-status path-shadow` で詳細確認してください。"
                        ));
                    }
                    harness_core::verdict::Determination::Known(shadowed) if !shadowed.is_empty() => {
                        let lines: Vec<String> = shadowed
                            .iter()
                            .map(|s| {
                                format!(
                                    "  ⚠ {} は {} がPATH上で plugin cache版 ({}) より優先されています。古い版が使われ続ける可能性があります（rm または cp で cache版を再コピーしてください）。",
                                    s.name, s.shadowing_path, s.cache_path
                                )
                            })
                            .collect();
                        sections.push(format!(
                            "harness-status: {} 件のbinaryがPATH上で古いコピーに shadow されています:\n{}\n`harness-status path-shadow` で詳細確認できます。",
                            shadowed.len(),
                            lines.join("\n")
                        ));
                    }
                    harness_core::verdict::Determination::Known(_) => {}
                }
                if !sections.is_empty() {
                    println!(
                        "{}",
                        serde_json::json!({ "additionalContext": sections.join("\n\n") })
                    );
                }
                // Nothing to report → stay silent (no output at all).
            });
        }
        None => {
            let b = budget::read(&today);
            let s = sessions::recent(cli.sessions);
            let p = progress::read(&cwd);
            let h = hooks::read();
            let i = inject::read();
            let hh = hooks_health::read();
            let ps = path_shadow::detect();
            let report = display::StatusReport {
                today: &today,
                budget: &b,
                sessions: &s,
                progress: &p,
                hooks: &h,
                inject: &i,
                hooks_health: &hh,
                path_shadow: &ps,
            };
            if cli.json {
                display::print_json(&report);
            } else {
                display::print_status(&report, &cwd.to_string_lossy());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(days_to_date(0), "1970-01-01");
    }

    #[test]
    fn known_dates_round_trip() {
        // 2000-01-01 = 10957 days after epoch; 2026-06-23 = 20627.
        assert_eq!(days_to_date(10957), "2000-01-01");
        assert_eq!(days_to_date(20627), "2026-06-23");
    }

    #[test]
    fn leap_year_feb_29_handled() {
        // 2024-02-29 = 19782 days after epoch.
        assert_eq!(days_to_date(19782), "2024-02-29");
        assert_eq!(days_to_date(19783), "2024-03-01");
    }

    #[test]
    fn leap_rule_centuries() {
        assert!(is_leap(2000)); // divisible by 400
        assert!(!is_leap(1900)); // divisible by 100, not 400
        assert!(is_leap(2024));
        assert!(!is_leap(2026));
    }

    #[test]
    fn today_does_not_create_world_writable_tmp_file() {
        // F->P: today() must not write to the FIXED, predictable /tmp path it used
        // to use (std::env::temp_dir()/.harness-status-date) — a symlink/TOCTOU
        // surface. Exercise the non-env clock path with HARNESS_DATE cleared and
        // assert the legacy file is not (re)created, while today() still yields a
        // well-formed YYYY-MM-DD date. Before the fix this file was created (RED).
        let legacy = std::env::temp_dir().join(".harness-status-date");
        let _ = std::fs::remove_file(&legacy);
        let prev = std::env::var("HARNESS_DATE").ok();
        std::env::remove_var("HARNESS_DATE");
        let d = today();
        match prev {
            Some(v) => std::env::set_var("HARNESS_DATE", v),
            None => std::env::remove_var("HARNESS_DATE"),
        }
        assert!(
            !legacy.exists(),
            "today() must not create the predictable /tmp clock file {legacy:?}"
        );
        assert_eq!(d.len(), 10, "date must be YYYY-MM-DD, got {d:?}");
        assert_eq!(&d[4..5], "-");
        assert_eq!(&d[7..8], "-");
    }
}
