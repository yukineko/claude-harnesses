mod budget;
mod display;
mod hooks;
mod inject;
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
            let s = sessions::recent(cli.sessions);
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
            let r = plugins::report(&root);
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
        None => {
            let b = budget::read(&today);
            let s = sessions::recent(cli.sessions);
            let p = progress::read(&cwd);
            let h = hooks::read();
            let i = inject::read();
            if cli.json {
                display::print_json(&today, &b, &s, &p, &h, &i);
            } else {
                display::print_status(&today, &b, &s, &p, &h, &i, &cwd.to_string_lossy());
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
