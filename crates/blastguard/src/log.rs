//! `blastguard log` — read-only listing of blastguard violations from overwatch's ledger.
//!
//! A pure query command: it never reads stdin and never writes to the ledger.
//! The ledger is `overwatch::store::violations_path(current_dir)`; only lines
//! with `source == "blastguard"` are listed, in file order. Lines from other
//! sources are skipped (not errors).
//!
//! A field that is absent from a ledger line (old-format lines predate the
//! context fields) is UNKNOWN. It is printed as `unknown` (text) / `null`
//! (JSON), and a filter on that field never matches it: the entry is dropped
//! and the number of such drops is reported on stderr ("excluded").
//!
//! Exit codes:
//! - 0: success (including "no matches" and an absent ledger, which says
//!   `no ledger at <path>` on stderr)
//! - 2: usage error (unknown flag, missing/bad value, bad `--since`,
//!   `--subagent` together with `--interactive`)
//! - 3: the ledger could not be located or read (path + error on stderr,
//!   stdout empty), any non-empty line that is not a valid blastguard/other
//!   ledger object (`line <N>` on stderr, 1-based; the valid entries are
//!   still printed), or a failure writing to stdout.

use harness_core::boundary;
use harness_core::verdict::Determination;
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct LogEntry {
    ts: Option<i64>,
    rule: Option<String>,
    decision: Option<String>,
    tool: Option<String>,
    command: Option<String>,
    target: Option<String>,
    cwd: Option<String>,
    actor: Option<String>,
    system_harm: Option<bool>,
}

#[derive(Debug, Default)]
struct Parsed {
    entries: Vec<LogEntry>,
    /// (1-based line number, message)
    bad_lines: Vec<(usize, String)>,
}

#[derive(Debug, Clone, Default)]
struct Filters {
    decision: Option<String>, // "deny" or "ask"
    rule: Option<String>,
    since: Option<i64>, // unix timestamp (seconds)
    system_harm: bool,
    actor: Option<String>, // "interactive" or "subagent"
    json_output: bool,
}

impl Filters {
    fn any_active(&self) -> bool {
        self.decision.is_some()
            || self.rule.is_some()
            || self.since.is_some()
            || self.system_harm
            || self.actor.is_some()
    }
}

fn set_actor(filters: &mut Filters, want: &str, flag: &str, other: &str) -> Result<(), String> {
    match filters.actor.as_deref() {
        Some(cur) if cur != want => Err(format!("{flag} and {other} are mutually exclusive")),
        _ => {
            filters.actor = Some(want.to_string());
            Ok(())
        }
    }
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
            "--system-harm" => filters.system_harm = true,
            "--subagent" => set_actor(&mut filters, "subagent", "--subagent", "--interactive")?,
            "--interactive" => {
                set_actor(&mut filters, "interactive", "--interactive", "--subagent")?
            }
            "--json" => filters.json_output = true,
            other => return Err(format!("unknown flag: {}", other)),
        }
    }

    Ok(filters)
}

/// Parse a time value: a duration `<digits>(s|m|h|d)` ago, or a UTC RFC3339
/// timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`). Anything else is an error.
fn parse_since(val: &str) -> Result<i64, String> {
    if let Some(unit) = val.chars().last().filter(|c| c.is_ascii_alphabetic()) {
        let num_str = &val[..val.len() - 1];
        if !num_str.is_empty() && num_str.bytes().all(|b| b.is_ascii_digit()) {
            let n: i64 = num_str
                .parse()
                .map_err(|_| format!("duration out of range: {}", val))?;
            let mult = match unit {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => return Err(format!("unknown duration unit: {}", unit)),
            };
            let secs = n
                .checked_mul(mult)
                .ok_or_else(|| format!("duration out of range: {}", val))?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "cannot determine current time".to_string())?
                .as_secs() as i64;
            return now
                .checked_sub(secs)
                .ok_or_else(|| format!("duration out of range: {}", val));
        }
    }
    parse_rfc3339(val)
}

fn parse_num(s: &str, what: &str) -> Result<u32, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid {}: {:?}", what, s));
    }
    s.parse().map_err(|_| format!("invalid {}: {:?}", what, s))
}

/// Strict UTC RFC3339 -> UNIX seconds. Only the `Z` suffix is accepted;
/// anything else is rejected (never guessed).
fn parse_rfc3339(s: &str) -> Result<i64, String> {
    let (date_str, time_str) = s
        .split_once('T')
        .ok_or_else(|| format!("not a duration or RFC3339 timestamp: {}", s))?;
    let time_part = time_str
        .strip_suffix('Z')
        .ok_or_else(|| format!("RFC3339 time must end with 'Z': {}", s))?;

    let date_parts: Vec<&str> = date_str.split('-').collect();
    if date_parts.len() != 3 {
        return Err(format!("invalid date format in RFC3339: {}", date_str));
    }
    let year = parse_num(date_parts[0], "year")? as i64;
    let month = parse_num(date_parts[1], "month")?;
    let day = parse_num(date_parts[2], "day")?;

    let (time_base, frac) = match time_part.split_once('.') {
        Some((b, f)) => (b, Some(f)),
        None => (time_part, None),
    };
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("invalid fractional seconds: {}", f));
        }
    }
    let time_parts: Vec<&str> = time_base.split(':').collect();
    if time_parts.len() != 3 {
        return Err(format!("invalid time format in RFC3339: {}", time_part));
    }
    let hour = parse_num(time_parts[0], "hour")?;
    let minute = parse_num(time_parts[1], "minute")?;
    let second = parse_num(time_parts[2], "second")?;

    if year < 1970 {
        return Err(format!("year before 1970 is not supported: {}", year));
    }
    if !(1..=12).contains(&month) {
        return Err(format!("month out of range: {}", month));
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(format!("day out of range: {}", day));
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("time out of range: {}", time_part));
    }

    let mut total_days: i64 = 0;
    for y in 1970..year {
        total_days += if is_leap_year(y) { 366 } else { 365 };
    }
    for m in 1..month {
        total_days += days_in_month(year, m) as i64;
    }
    total_days += (day - 1) as i64;

    total_days
        .checked_mul(86400)
        .and_then(|d| d.checked_add(hour as i64 * 3600 + minute as i64 * 60 + second as i64))
        .ok_or_else(|| format!("timestamp out of range: {}", s))
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(y) => 29,
        _ => 28,
    }
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

/// Optional string field: absent/null is unknown; a non-string is a bad line.
fn opt_str(obj: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match obj.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("field `{}` is not a string", key)),
    }
}

fn parse_entry(obj: &serde_json::Value) -> Result<LogEntry, String> {
    let ts = match obj.get("ts") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            v.as_i64()
                .ok_or_else(|| "field `ts` is not an integer".to_string())?,
        ),
    };
    let system_harm = match obj.get("system_harm") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            v.as_bool()
                .ok_or_else(|| "field `system_harm` is not a boolean".to_string())?,
        ),
    };
    let rule = opt_str(obj, "signature")?.map(|sig| {
        sig.strip_prefix("blastguard:")
            .map(str::to_string)
            .unwrap_or(sig)
    });
    Ok(LogEntry {
        ts,
        rule,
        decision: opt_str(obj, "decision")?,
        tool: opt_str(obj, "tool")?,
        command: opt_str(obj, "command")?,
        target: opt_str(obj, "target")?,
        cwd: opt_str(obj, "cwd")?,
        actor: opt_str(obj, "actor")?,
        system_harm,
    })
}

/// Parse ledger text. Every non-empty line must be a JSON object; a line that
/// is not (or whose blastguard fields have the wrong type) is a bad line.
fn parse_ledger(content: &str) -> Parsed {
    let mut out = Parsed::default();
    for (idx, line) in content.lines().enumerate() {
        let n = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // serde's own " at line L column C" refers to this single
                // line, not the ledger; drop it so `line <N>` is unambiguous.
                let full = e.to_string();
                let msg = full.split(" at line ").next().unwrap_or(&full);
                out.bad_lines.push((n, msg.to_string()));
                continue;
            }
        };
        if !obj.is_object() {
            out.bad_lines.push((n, "not a JSON object".to_string()));
            continue;
        }
        match obj.get("source") {
            Some(serde_json::Value::String(s)) if s == "blastguard" => match parse_entry(&obj) {
                Ok(e) => out.entries.push(e),
                Err(msg) => out.bad_lines.push((n, msg)),
            },
            // Other sources are skipped, not errors.
            Some(serde_json::Value::String(_)) => {}
            _ => out
                .bad_lines
                .push((n, "missing or non-string `source`".to_string())),
        }
    }
    out
}

/// Apply filters. Returns (kept entries, number excluded because a filtered
/// field was unknown). An entry with ANY unknown field under an active filter
/// is excluded (never matched) and counted once.
fn apply_filters(entries: Vec<LogEntry>, f: &Filters) -> (Vec<LogEntry>, usize) {
    let mut excluded = 0usize;
    let mut kept = Vec::new();
    for e in entries {
        let unknown = (f.decision.is_some() && e.decision.is_none())
            || (f.rule.is_some() && e.rule.is_none())
            || (f.since.is_some() && e.ts.is_none())
            || (f.system_harm && e.system_harm.is_none())
            || (f.actor.is_some() && e.actor.is_none());
        if unknown {
            excluded += 1;
            continue;
        }
        let ok = f
            .decision
            .as_ref()
            .is_none_or(|d| e.decision.as_ref() == Some(d))
            && f.rule.as_ref().is_none_or(|r| e.rule.as_ref() == Some(r))
            && f.since.is_none_or(|s| e.ts.is_some_and(|t| t >= s))
            && (!f.system_harm || e.system_harm == Some(true))
            && f.actor.as_ref().is_none_or(|a| e.actor.as_ref() == Some(a));
        if ok {
            kept.push(e);
        }
    }
    (kept, excluded)
}

fn format_text_line(entry: &LogEntry) -> String {
    let unk = |o: &Option<String>| o.clone().unwrap_or_else(|| "unknown".to_string());
    let system_harm = match entry.system_harm {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    };
    let mut line = format!(
        "decision={} actor={} system_harm={} rule={}",
        unk(&entry.decision),
        unk(&entry.actor),
        system_harm,
        unk(&entry.rule)
    );
    if let Some(cmd) = &entry.command {
        line.push(' ');
        line.push_str(cmd);
    }
    if let Some(target) = &entry.target {
        line.push(' ');
        line.push_str(target);
    }
    line
}

fn format_json_object(entry: &LogEntry) -> serde_json::Value {
    json!({
        "ts": entry.ts,
        "rule": entry.rule,
        "decision": entry.decision,
        "tool": entry.tool,
        "command": entry.command,
        "target": entry.target,
        "cwd": entry.cwd,
        "actor": entry.actor,
        "system_harm": entry.system_harm,
    })
}

/// Main entry point for `blastguard log`. Returns the process exit code.
pub fn run_log(rest: &[String]) -> i32 {
    let filters = match parse_log_args(rest) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("blastguard log: {}", msg);
            return 2;
        }
    };

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("blastguard log: cannot determine current directory: {}", e);
            return 3;
        }
    };
    let ledger_path = match overwatch::store::violations_path(&cwd) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "blastguard log: cannot resolve the ledger path for {}: {}",
                cwd.display(),
                e
            );
            return 3;
        }
    };

    let content = match read_ledger_text(&ledger_path) {
        Ok(Some(t)) => t,
        Ok(None) => {
            eprintln!("blastguard log: no ledger at {}", ledger_path.display());
            return 0;
        }
        Err(why) => {
            eprintln!(
                "blastguard log: cannot read ledger {}: {}",
                ledger_path.display(),
                why
            );
            return 3;
        }
    };

    let parsed = parse_ledger(&content);
    for (n, msg) in &parsed.bad_lines {
        eprintln!(
            "blastguard log: {}: line {}: {}",
            ledger_path.display(),
            n,
            msg
        );
    }
    let bad = !parsed.bad_lines.is_empty();

    let (entries, excluded) = apply_filters(parsed.entries, &filters);
    if filters.any_active() && excluded > 0 {
        eprintln!(
            "blastguard log: {} entries excluded: a filtered field is unknown on those lines",
            excluded
        );
    }

    let mut buf = String::new();
    for e in &entries {
        if filters.json_output {
            buf.push_str(&format_json_object(e).to_string());
        } else {
            buf.push_str(&format_text_line(e));
        }
        buf.push('\n');
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if let Err(e) = lock.write_all(buf.as_bytes()).and_then(|_| lock.flush()) {
        eprintln!("blastguard log: cannot write to stdout: {}", e);
        return 3;
    }

    if bad {
        3
    } else {
        0
    }
}

/// Ok(Some(text)) = read; Ok(None) = genuinely absent; Err = unreadable.
fn read_ledger_text(path: &Path) -> Result<Option<String>, String> {
    match boundary::read_to_string(path) {
        Determination::Known(t) => Ok(t),
        Determination::Undetermined(why) => Err(why.as_str().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rfc3339_epoch_and_rejects() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:01Z"), Ok(1));
        assert_eq!(parse_rfc3339("2000-01-01T00:00:00Z"), Ok(946684800));
        assert!(parse_rfc3339("2023-02-31T00:00:00Z").is_err());
        assert!(parse_rfc3339("1960-01-01T00:00:00Z").is_err());
        assert!(parse_rfc3339("2023-01-01T00:00:00+09:00").is_err());
        assert!(parse_since("banana").is_err());
        assert!(parse_since("-5s").is_err());
        assert!(parse_since("99999999999999999999d").is_err());
    }

    #[test]
    fn actor_flags_exclusive_but_repeatable() {
        assert!(parse_log_args(&s(&["--subagent", "--interactive"])).is_err());
        assert!(parse_log_args(&s(&["--subagent", "--subagent"])).is_ok());
    }

    #[test]
    fn non_object_and_wrong_type_lines_are_bad() {
        let p = parse_ledger("123\n{\"source\":\"blastguard\",\"decision\":5}\n{\"x\":1}\n");
        assert_eq!(p.bad_lines.len(), 3);
        assert!(p.entries.is_empty());
    }

    #[test]
    fn unknown_ts_never_matches_since() {
        let p = parse_ledger("{\"source\":\"blastguard\",\"signature\":\"blastguard:r\"}\n");
        let f = Filters {
            since: Some(0),
            ..Default::default()
        };
        let (kept, excluded) = apply_filters(p.entries, &f);
        assert!(kept.is_empty());
        assert_eq!(excluded, 1);
    }
}
