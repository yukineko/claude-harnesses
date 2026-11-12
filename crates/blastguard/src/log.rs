//! `blastguard log` — read-only listing of blastguard violations from overwatch's ledger.
//!
//! This is a pure query command (no side effects, no stdin, no modification).
//! Violations are filtered and printed in text or JSON format.
//!
//! Exit codes:
//! - 0: success (including "no matches")
//! - 2: usage error (invalid flags, mutually exclusive options, parse error in arg)
//! - 3: read error (ledger file cannot be read, or contains unparsable non-empty lines)

use harness_core::boundary;
use harness_core::verdict::Determination;
use serde_json::json;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Result of running `blastguard log`.
#[derive(Debug)]
struct LogResult {
    entries: Vec<LogEntry>,
    parse_errors: Vec<(usize, String)>, // (1-based line number, error message)
    excluded_count: usize,
}

#[derive(Debug, Clone)]
struct LogEntry {
    ts: i64,
    signature: String, // blastguard:<rule>
    decision: Option<String>,
    tool: Option<String>,
    command: Option<String>,
    target: Option<String>,
    cwd: Option<String>,
    actor: Option<String>,
    system_harm: Option<bool>,
}

impl LogEntry {
    fn rule_id(&self) -> String {
        self.signature
            .strip_prefix("blastguard:")
            .unwrap_or("unknown")
            .to_string()
    }
}

#[derive(Debug, Clone, Default)]
struct Filters {
    decision: Option<String>, // "deny" or "ask"
    rule: Option<String>,
    since: Option<i64>, // unix timestamp (seconds)
    system_harm: bool,
    actor: Option<String>, // "headless", "interactive", "subagent"
    json_output: bool,
}

/// Parse command-line arguments for `blastguard log`.
fn parse_log_args(rest: &[String]) -> Result<Filters, String> {
    let mut filters = Filters::default();
    let mut it = rest.iter();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--decision" => {
                let val = it.next().ok_or("--decision needs a value (deny or ask)")?;
                if val != "deny" && val != "ask" {
                    return Err(format!("--decision must be 'deny' or 'ask', not '{}'", val));
                }
                filters.decision = Some(val.clone());
            }
            "--rule" => {
                let val = it.next().ok_or("--rule needs a rule id")?;
                filters.rule = Some(val.clone());
            }
            "--since" => {
                let val = it
                    .next()
                    .ok_or("--since needs a time (RFC3339 or duration)")?;
                filters.since = Some(parse_since(val)?);
            }
            "--system-harm" => {
                filters.system_harm = true;
            }
            "--subagent" => {
                if filters.actor.is_some() {
                    return Err("--subagent and --interactive are mutually exclusive".to_string());
                }
                filters.actor = Some("subagent".to_string());
            }
            "--interactive" => {
                if filters.actor.is_some() {
                    return Err("--interactive and --subagent are mutually exclusive".to_string());
                }
                filters.actor = Some("interactive".to_string());
            }
            "--json" => {
                filters.json_output = true;
            }
            other => {
                return Err(format!("unknown flag: {}", other));
            }
        }
    }

    Ok(filters)
}

/// Parse a time value: either RFC3339 timestamp or a duration (Ns|m|h|d).
fn parse_since(val: &str) -> Result<i64, String> {
    // Try duration first (simple heuristic: contains only digits and one letter)
    if let Some(pos) = val.rfind(|c: char| !c.is_ascii_digit()) {
        if pos == val.len() - 1 {
            // Last char is a letter, so it looks like a duration.
            let num_str = &val[..pos];
            let unit = &val[pos..];
            if let Ok(n) = num_str.parse::<i64>() {
                let secs = match unit {
                    "s" => n,
                    "m" => n * 60,
                    "h" => n * 3600,
                    "d" => n * 86400,
                    _ => return Err(format!("unknown duration unit: {}", unit)),
                };
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "cannot determine current time".to_string())?
                    .as_secs() as i64;
                return Ok(now - secs);
            }
        }
    }

    // Try RFC3339
    parse_rfc3339(val)
}

/// Hand-parse RFC3339 timestamp to UNIX seconds (fail-closed: reject any variant).
/// Expected format: YYYY-MM-DDTHH:MM:SSZ (with optional .fractional-seconds before Z).
fn parse_rfc3339(s: &str) -> Result<i64, String> {
    // Strict parsing: reject any deviation from the expected format.
    if s.len() < 19 {
        // Minimum: "2000-01-01T00:00:00Z"
        return Err(format!("RFC3339 timestamp too short: {}", s));
    }

    // Split on 'T'
    let parts: Vec<&str> = s.split('T').collect();
    if parts.len() != 2 {
        return Err(format!("RFC3339 must have 'T' separator: {}", s));
    }

    let date_str = parts[0];
    let time_str = parts[1];

    // Parse date YYYY-MM-DD
    let date_parts: Vec<&str> = date_str.split('-').collect();
    if date_parts.len() != 3 {
        return Err(format!("invalid date format in RFC3339: {}", date_str));
    }

    let year: i32 = date_parts[0]
        .parse()
        .map_err(|_| format!("invalid year: {}", date_parts[0]))?;
    let month: u32 = date_parts[1]
        .parse()
        .map_err(|_| format!("invalid month: {}", date_parts[1]))?;
    let day: u32 = date_parts[2]
        .parse()
        .map_err(|_| format!("invalid day: {}", date_parts[2]))?;

    // Parse time HH:MM:SS[.fractional]Z
    // Ensure it ends with Z
    if !time_str.ends_with('Z') {
        return Err(format!("RFC3339 time must end with 'Z': {}", time_str));
    }

    let time_part = &time_str[..time_str.len() - 1]; // strip the Z

    // Remove fractional seconds if present
    let time_base = if let Some(dot_pos) = time_part.find('.') {
        &time_part[..dot_pos]
    } else {
        time_part
    };

    let time_parts: Vec<&str> = time_base.split(':').collect();
    if time_parts.len() != 3 {
        return Err(format!("invalid time format in RFC3339: {}", time_part));
    }

    let hour: u32 = time_parts[0]
        .parse()
        .map_err(|_| format!("invalid hour: {}", time_parts[0]))?;
    let minute: u32 = time_parts[1]
        .parse()
        .map_err(|_| format!("invalid minute: {}", time_parts[1]))?;
    let second: u32 = time_parts[2]
        .parse()
        .map_err(|_| format!("invalid second: {}", time_parts[2]))?;

    // Basic validation
    if !(1..=12).contains(&month) {
        return Err(format!("month out of range: {}", month));
    }
    if !(1..=31).contains(&day) {
        return Err(format!("day out of range: {}", day));
    }
    if hour > 23 {
        return Err(format!("hour out of range: {}", hour));
    }
    if minute > 59 {
        return Err(format!("minute out of range: {}", minute));
    }
    if second > 59 {
        return Err(format!("second out of range: {}", second));
    }

    // Convert to UNIX timestamp using a simple days calculation
    // (not handling leap seconds, just leap years)
    let days_in_year = |y: i32| -> u32 {
        if is_leap_year(y) {
            366
        } else {
            365
        }
    };
    let days_in_month = |y: i32, m: u32| -> u32 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap_year(y) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    };

    // Days since 1970-01-01
    let mut total_days: i64 = 0;
    for y in 1970..year {
        total_days += days_in_year(y) as i64;
    }
    for m in 1..month {
        total_days += days_in_month(year, m) as i64;
    }
    total_days += (day - 1) as i64;

    let total_seconds =
        total_days * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64;
    Ok(total_seconds)
}

fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

/// Read and parse the violations ledger, returning all entries (not filtered yet).
fn read_ledger(path: &PathBuf) -> io::Result<LogResult> {
    // Use boundary wrapper (fail-closed: unknown/unreadable treated as errors)
    let content = match boundary::read_to_string(path) {
        Determination::Known(Some(text)) => text,
        Determination::Known(None) => {
            // File doesn't exist - return NotFound error
            return Err(io::Error::new(io::ErrorKind::NotFound, "ledger not found"));
        }
        Determination::Undetermined(why) => {
            // File exists but unreadable (or fault injection active)
            return Err(io::Error::new(io::ErrorKind::Other, why.as_str()));
        }
    };
    let mut entries = Vec::new();
    let mut parse_errors = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_num_1based = line_num + 1;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(obj) => {
                // Check if this is a blastguard entry
                if obj["source"].as_str() != Some("blastguard") {
                    continue;
                }

                let entry = LogEntry {
                    ts: obj["ts"].as_i64().unwrap_or(0),
                    signature: obj["signature"].as_str().unwrap_or("unknown").to_string(),
                    decision: obj["decision"].as_str().map(|s| s.to_string()),
                    tool: obj["tool"].as_str().map(|s| s.to_string()),
                    command: obj["command"].as_str().map(|s| s.to_string()),
                    target: obj["target"].as_str().map(|s| s.to_string()),
                    cwd: obj["cwd"].as_str().map(|s| s.to_string()),
                    actor: obj["actor"].as_str().map(|s| s.to_string()),
                    system_harm: obj["system_harm"].as_bool(),
                };
                entries.push(entry);
            }
            Err(e) => {
                // Extract error message without serde_json position info
                let err_msg = e.to_string();
                let msg = if let Some(idx) = err_msg.find(" at line ") {
                    &err_msg[..idx]
                } else {
                    &err_msg
                };
                parse_errors.push((line_num_1based, msg.to_string()));
            }
        }
    }

    Ok(LogResult {
        entries,
        parse_errors,
        excluded_count: 0,
    })
}

/// Apply filters to entries and return matching ones, along with count of excluded.
/// "Excluded" means the entry had an unknown/None value for a field that was actively filtered.
fn apply_filters(mut result: LogResult, filters: &Filters) -> LogResult {
    let mut excluded = 0;
    let filtered: Vec<LogEntry> = result
        .entries
        .into_iter()
        .filter(|entry| {
            // Check each filter; if unknown field, count as excluded (fail-closed).
            if let Some(ref dec) = filters.decision {
                match &entry.decision {
                    Some(d) if d == dec => {}
                    Some(_) => return false, // Known value doesn't match, filter silently
                    None => {
                        excluded += 1;
                        return false; // Unknown field, count as excluded
                    }
                }
            }

            if let Some(ref rule) = filters.rule {
                // rule_id() extracts from signature, which is never None (has default "unknown")
                if entry.rule_id() != *rule {
                    return false; // Filter but don't count as "excluded"
                }
            }

            if let Some(since) = filters.since {
                if entry.ts < since {
                    return false; // Filter but don't count as "excluded"
                }
            }

            if filters.system_harm {
                match entry.system_harm {
                    Some(true) => {}
                    Some(false) => return false, // Known value doesn't match, filter silently
                    None => {
                        excluded += 1;
                        return false; // Unknown field, count as excluded
                    }
                }
            }

            if let Some(ref actor) = filters.actor {
                match &entry.actor {
                    Some(a) if a == actor => {}
                    Some(_) => return false, // Known value doesn't match, filter silently
                    None => {
                        excluded += 1;
                        return false; // Unknown field, count as excluded
                    }
                }
            }

            true
        })
        .collect();

    result.entries = filtered;
    result.excluded_count = excluded;
    result
}

/// Format a single entry as text tokens.
fn format_text_line(entry: &LogEntry) -> String {
    let decision = entry.decision.as_deref().unwrap_or("unknown");
    let actor = entry.actor.as_deref().unwrap_or("unknown");
    let system_harm = match entry.system_harm {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    };
    let rule = entry.rule_id();

    let mut line = format!(
        "decision={} actor={} system_harm={} rule={}",
        decision, actor, system_harm, rule
    );

    // Append command/target text if present
    if let Some(ref cmd) = entry.command {
        line.push(' ');
        line.push_str(cmd);
    }
    if let Some(ref target) = entry.target {
        line.push(' ');
        line.push_str(target);
    }

    line
}

/// Format a single entry as JSON.
fn format_json_object(entry: &LogEntry) -> serde_json::Value {
    json!({
        "ts": entry.ts,
        "rule": entry.rule_id(),
        "decision": entry.decision,
        "tool": entry.tool,
        "command": entry.command,
        "target": entry.target,
        "cwd": entry.cwd,
        "actor": entry.actor,
        "system_harm": entry.system_harm,
    })
}

/// Main entry point for `blastguard log`.
pub fn run_log(rest: &[String]) -> i32 {
    // Parse arguments
    let filters = match parse_log_args(rest) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("blastguard log: {}", msg);
            return 2;
        }
    };

    // Find the ledger path
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => {
            eprintln!("blastguard log: cannot determine current directory");
            return 3;
        }
    };

    let ledger_path = match overwatch::store::violations_path(&cwd) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("blastguard log: no ledger at {}", cwd.display());
            return 0;
        }
    };

    // Try to read the ledger
    let mut result = match read_ledger(&ledger_path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("blastguard log: no ledger at {}", ledger_path.display());
            return 0;
        }
        Err(e) => {
            eprintln!("blastguard log: cannot read ledger: {}", e);
            return 3;
        }
    };

    // Check for parse errors (fail-closed)
    let has_parse_errors = !result.parse_errors.is_empty();
    if has_parse_errors {
        for (line_num, err) in &result.parse_errors {
            eprintln!("blastguard log: line {}: {}", line_num, err);
        }
    }

    // Apply filters
    result = apply_filters(result, &filters);

    // Report excluded entries if any filter was active
    if result.excluded_count > 0
        && (filters.decision.is_some()
            || filters.rule.is_some()
            || filters.since.is_some()
            || filters.system_harm
            || filters.actor.is_some())
    {
        eprintln!(
            "blastguard log: {} entries excluded by filters",
            result.excluded_count
        );
    }

    // Print results
    if filters.json_output {
        for entry in &result.entries {
            println!("{}", format_json_object(entry));
        }
    } else {
        for entry in &result.entries {
            println!("{}", format_text_line(entry));
        }
    }

    // Return exit code
    if has_parse_errors {
        3
    } else {
        0
    }
}
