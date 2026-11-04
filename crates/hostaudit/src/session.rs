//! The two remaining canon §1 consumers: the `daily` task stanza and the
//! SessionStart one-line systemMessage.

use crate::config::Ctx;
use harness_core::verdict::Required;
use std::collections::BTreeMap;

/// The running executable as a shell word for `sh -c`: an absolute path to
/// an existing file, single-quoted only when it contains a character outside
/// `[A-Za-z0-9/._+@:,=-]`. `Err` when it cannot be resolved — the caller must
/// fail rather than print a bare `hostaudit` (daily runs tasks with a PATH
/// that does not contain plugin bin dirs, so a bare name exits 127).
fn self_command() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot resolve the hostaudit executable path: {e}"))?;
    if !exe.is_absolute() {
        return Err(format!(
            "the hostaudit executable path {} is not absolute",
            exe.display()
        ));
    }
    if !exe.is_file() {
        return Err(format!(
            "the hostaudit executable path {} is not an existing file",
            exe.display()
        ));
    }
    let s = exe.to_str().ok_or_else(|| {
        format!(
            "the hostaudit executable path {} is not UTF-8",
            exe.display()
        )
    })?;
    if s.chars().any(char::is_control) {
        return Err(format!(
            "the hostaudit executable path {s:?} contains a control character"
        ));
    }
    let plain = s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '/' | '.' | '_' | '+' | '@' | ':' | ',' | '=' | '-')
    });
    Ok(if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    })
}

/// A TOML basic string (`"…"`) holding `s`.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `hostaudit daily-task --print` (CONTRACT §10). Never includes `aws apply`.
/// Every task command starts with the absolute path of this executable;
/// `Err` when that path cannot be resolved (nothing is printed then).
pub fn daily_stanza() -> Result<String, String> {
    let exe = self_command()?;
    let task = |name: &str, verb: &str| {
        format!(
            "[[task]]\nname = {}\ncommand = {}\n",
            toml_str(name),
            toml_str(&format!("{exe} {verb}"))
        )
    };
    Ok(format!(
        "# hostaudit — append to ~/.daily/config.toml (runs once a day; read-only audits).\n\
         # The commands use the absolute path of this hostaudit binary, because daily's\n\
         # task shell does not have plugin bin dirs on its PATH. Re-run\n\
         # `hostaudit daily-task --print` after the plugin is upgraded to a new path.\n\
         # A non-zero exit means a violation or an undetermined check — see\n\
         # ~/.hostaudit/reports.jsonl or `overwatch review-queue`. The last output line\n\
         # counts violation / undetermined / warning / clean separately.\n\
         \n\
         {}\n{}\n{}",
        task("hostaudit-audit", "audit"),
        task("hostaudit-local", "local"),
        task("hostaudit-aws", "aws")
    ))
}

/// The SessionStart line. Never silent: no report and an unreadable report
/// both say the state is UNKNOWN.
pub fn session_message(ctx: &Ctx) -> String {
    let p = ctx.state_dir().join("reports.jsonl");
    let text = match harness_core::boundary::read_to_string(&p).require() {
        Required::Determined(Some(t)) => t,
        Required::Determined(None) => {
            return "hostaudit: no audit recorded yet — server/AWS/local state UNKNOWN (run `hostaudit audit` / `local` / `aws`, or register `hostaudit daily-task --print`)".to_string()
        }
        Required::Blocked(why) => {
            return format!("hostaudit: reports.jsonl unreadable — state UNKNOWN ({why})")
        }
    };
    // Latest row per finding id.
    let mut latest: BTreeMap<String, (i64, String)> = BTreeMap::new();
    let mut bad = 0usize;
    let mut newest = i64::MIN;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            bad += 1;
            continue;
        };
        let (Some(id), Some(verdict), Some(time)) =
            (v["id"].as_str(), v["verdict"].as_str(), v["time"].as_i64())
        else {
            bad += 1;
            continue;
        };
        newest = newest.max(time);
        let e = latest
            .entry(id.to_string())
            .or_insert((i64::MIN, String::new()));
        if time >= e.0 {
            *e = (time, verdict.to_string());
        }
    }
    let viol = latest.values().filter(|(_, v)| v == "violation").count();
    let warn = latest.values().filter(|(_, v)| v == "warning").count();
    // Any verdict string other than the four known ones counts as undetermined.
    let und = latest
        .values()
        .filter(|(_, v)| {
            !matches!(
                v.as_str(),
                "violation" | "warning" | "clean" | "not_applicable"
            )
        })
        .count();
    let when = chrono::DateTime::from_timestamp(newest, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown time".to_string());
    // canon v1.1 §1: warnings are counted separately, never folded into clean.
    let mut msg = format!(
        "hostaudit: {viol} violation / {und} undetermined / {warn} warning (latest per finding; newest report {when})"
    );
    if latest.is_empty() {
        msg = "hostaudit: reports.jsonl has no readable rows — state UNKNOWN".to_string();
    }
    if bad > 0 {
        msg.push_str(&format!(
            "; {bad} unreadable report rows — counts may be incomplete"
        ));
    }
    if viol + und > 0 {
        msg.push_str(" — details: ~/.hostaudit/reports.jsonl / `overwatch review-queue`");
    }
    msg
}
