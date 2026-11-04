//! The two remaining canon §1 consumers: the `daily` task stanza and the
//! SessionStart one-line systemMessage.

use crate::config::Ctx;
use harness_core::verdict::Required;
use std::collections::BTreeMap;

/// `hostaudit daily-task --print` (CONTRACT §10). Never includes `aws apply`.
pub fn daily_stanza() -> String {
    "# hostaudit — append to ~/.daily/config.toml (runs once a day; read-only audits).\n\
     # `hostaudit` must be on the PATH of the shell daily runs tasks in; otherwise\n\
     # replace it with the absolute path of the plugin's bin/hostaudit launcher.\n\
     # A non-zero exit means a violation or an undetermined check — see\n\
     # ~/.hostaudit/reports.jsonl or `overwatch review-queue`.\n\
     \n\
     [[task]]\n\
     name = \"hostaudit-audit\"\n\
     command = \"hostaudit audit\"\n\
     \n\
     [[task]]\n\
     name = \"hostaudit-local\"\n\
     command = \"hostaudit local\"\n\
     \n\
     [[task]]\n\
     name = \"hostaudit-aws\"\n\
     command = \"hostaudit aws\"\n"
        .to_string()
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
    let und = latest
        .values()
        .filter(|(_, v)| v != "violation" && v != "clean" && v != "not_applicable")
        .count();
    let when = chrono::DateTime::from_timestamp(newest, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown time".to_string());
    let mut msg = format!(
        "hostaudit: {viol} violation / {und} undetermined (latest per finding; newest report {when})"
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
