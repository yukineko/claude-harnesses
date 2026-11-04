//! Windows rules WN1–WN5 (canon v1.1 §7, CONTRACT §12.6). Only W1, W0 and the
//! five fixed PowerShell strings are ever sent to a Windows host.
//!
//! "now" is `HOSTAUDIT_NOW` / the local clock (`ctx.now`): the whitelist has
//! no remote clock command for Windows (CONTRACT §12.3 [choice]).

use crate::audit::{Memo, Probe};
use crate::config::Ctx;
use crate::feeds::{self, Period, DAY};
use crate::finding::Finding;
use crate::ssh::{Obs, Remote};
use serde_json::{json, Value};

const HOTFIX_MAX: i64 = 45 * DAY;
const REBOOT_MAX: i64 = 7 * DAY;

fn text(o: &Obs, id: &str) -> Result<String, String> {
    match o {
        Obs::Out(s) => Ok(s.clone()),
        Obs::Undet(w) | Obs::Missing(w) => Err(w.clone()),
        other => Err(format!("{id} unexpected {other:?}")),
    }
}

fn parse_json(o: &Obs, id: &str) -> Result<Value, String> {
    let t = text(o, id)?;
    serde_json::from_str::<Value>(t.trim())
        .map_err(|e| format!("{id} output is not JSON ({e}): {:?}", t.trim()))
}

fn wn1(ctx: &Ctx, t: &str, o: &Obs, memo: &mut Memo) -> Finding {
    let v = match parse_json(o, "WN1") {
        Ok(v) => v,
        Err(e) => return Finding::undetermined("WN1", t, vec![], &e),
    };
    let (Some(caption), Some(version)) = (v["Caption"].as_str(), v["Version"].as_str()) else {
        return Finding::undetermined(
            "WN1",
            t,
            vec![],
            &format!("WN1 JSON lacks Caption / Version: {v}"),
        );
    };
    let build = v["BuildNumber"].as_str().unwrap_or("?");
    let slug = if caption.contains("Server") {
        "windows-server"
    } else {
        "windows"
    };
    let ev0 = format!("{caption} Version {version} Build {build}");
    let feed = match memo.eol(ctx, slug) {
        Ok(f) => f.clone(),
        Err(e) => {
            return Finding::undetermined(
                "WN1",
                t,
                vec![ev0],
                &format!("endoflife.date {slug} unavailable: {e}"),
            )
        }
    };
    let Some(c) = feed.data.iter().find(|c| {
        c.latest
            .as_deref()
            .is_some_and(|l| !version.is_empty() && l.starts_with(version))
    }) else {
        return Finding::undetermined(
            "WN1",
            t,
            vec![ev0],
            &format!("no endoflife.date {slug} cycle whose `latest` starts with {version}"),
        )
        .with("eol_fetched_at", json!(feed.fetched_at));
    };
    let ev = vec![
        ev0,
        format!(
            "{slug} cycle {}: eol {}, extendedSupport {}",
            c.cycle,
            feeds::support_text(c.eol),
            feeds::support_text(c.ext)
        ),
    ];
    let f = match feeds::eol_state(c.eol, ctx.now) {
        Err(e) => Finding::undetermined("WN1", t, ev, &e),
        Ok(Period::Running(_)) => Finding::clean("WN1", t, ev),
        Ok(Period::Ended) if feeds::ext_running(c.ext, ctx.now) => Finding::warning(
            "WN1",
            t,
            ev,
            "標準サポートは終了し、延長セキュリティ更新（ESU）期間のみです。ESU 契約の有無は観測できないため確認し、後継版への移行を計画してください",
        ),
        Ok(Period::Ended) => Finding::violation(
            "WN1",
            t,
            ev,
            "この Windows の版はサポート（延長を含む）が終了しています。後継のサポート版への移行を計画してください",
        ),
    };
    f.with("eol_fetched_at", json!(feed.fetched_at))
}

/// `InstalledOn` → unix seconds: ISO `YYYY-MM-DDTHH:MM:SS…` or `/Date(<ms>)/`.
fn hotfix_time(v: &Value) -> Result<i64, String> {
    let s = v["InstalledOn"]
        .as_str()
        .ok_or_else(|| format!("WN2 has no readable InstalledOn: {v}"))?;
    if let Some(ms) = s.strip_prefix("/Date(").and_then(|r| r.strip_suffix(")/")) {
        let digits: String = ms
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        return digits
            .parse::<i64>()
            .map(|ms| ms.div_euclid(1000))
            .map_err(|_| format!("WN2 InstalledOn {s:?} is not /Date(ms)/"));
    }
    let day = s.get(..10).unwrap_or("");
    let d = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| format!("WN2 InstalledOn {s:?} is not a date"))?;
    let rest = &s[10..];
    if !(rest.is_empty() || rest.starts_with('T') || rest.starts_with(' ')) {
        return Err(format!("WN2 InstalledOn {s:?} is not a date"));
    }
    let time = rest
        .get(1..9)
        .and_then(|h| chrono::NaiveTime::parse_from_str(h, "%H:%M:%S").ok())
        .unwrap_or(chrono::NaiveTime::MIN);
    Ok(d.and_time(time).and_utc().timestamp())
}

fn wn2_age(ctx: &Ctx, o: &Obs) -> Result<(i64, String), String> {
    let v = parse_json(o, "WN2")?;
    if v.is_null() {
        return Err("WN2: no hotfix is installed (null)".into());
    }
    let at = hotfix_time(&v)?;
    let id = v["HotFixID"].as_str().unwrap_or("?");
    Ok((
        ctx.now - at,
        format!("latest hotfix {id} installed {}", feeds::ymd(at)),
    ))
}

fn wn2(t: &str, age: &Result<(i64, String), String>) -> Finding {
    match age {
        Err(e) => Finding::undetermined("WN2", t, vec![], e),
        Ok((age, line)) => {
            let ev = vec![format!("{line} ({} days ago)", age / DAY)];
            if *age > HOTFIX_MAX {
                Finding::violation(
                    "WN2",
                    t,
                    ev,
                    "Windows Update を実行して月例の更新を適用してください（最後の適用から 45 日を超えています）",
                )
            } else {
                Finding::clean("WN2", t, ev)
            }
        }
    }
}

fn wn3(t: &str, o: &Obs, age: &Result<(i64, String), String>) -> Finding {
    let s = match text(o, "WN3") {
        Ok(s) => s,
        Err(e) => return Finding::undetermined("WN3", t, vec![], &e),
    };
    match s.trim() {
        "False" => Finding::clean("WN3", t, vec!["RebootRequired: False".into()]),
        "True" => match age {
            Err(e) => Finding::undetermined(
                "WN3",
                t,
                vec!["RebootRequired: True".into()],
                &format!("reboot pending but its age cannot be measured (WN2: {e})"),
            ),
            Ok((age, line)) => {
                let ev = vec!["RebootRequired: True".into(), line.clone()];
                if *age > REBOOT_MAX {
                    Finding::violation(
                        "WN3",
                        t,
                        ev,
                        "再起動待ちが 7 日を超えて放置されています。計画停止枠で再起動してください",
                    )
                } else {
                    Finding::clean("WN3", t, ev)
                }
            }
        },
        other => Finding::undetermined(
            "WN3",
            t,
            vec![],
            &format!("WN3 output {other:?} is neither True nor False"),
        ),
    }
}

fn enabled(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        Value::String(s) => match s.as_str() {
            "True" | "true" | "1" => Some(true),
            "False" | "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn wn4(t: &str, o: &Obs) -> Finding {
    let v = match parse_json(o, "WN4") {
        Ok(v) => v,
        Err(e) => return Finding::undetermined("WN4", t, vec![], &e),
    };
    let list: Vec<Value> = match v {
        Value::Array(a) => a,
        obj @ Value::Object(_) => vec![obj],
        other => {
            return Finding::undetermined(
                "WN4",
                t,
                vec![],
                &format!("WN4 JSON is not a profile list: {other}"),
            )
        }
    };
    if list.is_empty() {
        return Finding::undetermined(
            "WN4",
            t,
            vec![],
            "WN4 returned no firewall profile; an empty list is not a pass",
        );
    }
    let mut ev = Vec::new();
    let mut off = Vec::new();
    for p in &list {
        let (Some(name), Some(en)) = (p["Name"].as_str(), enabled(&p["Enabled"])) else {
            return Finding::undetermined(
                "WN4",
                t,
                vec![],
                &format!("WN4 profile entry without Name / Enabled: {p}"),
            );
        };
        ev.push(format!(
            "{name}: {}",
            if en { "enabled" } else { "disabled" }
        ));
        if !en {
            off.push(name.to_string());
        }
    }
    if off.is_empty() {
        Finding::clean("WN4", t, ev)
    } else {
        let fix = off
            .iter()
            .map(|n| format!("Set-NetFirewallProfile -Profile {n} -Enabled True"))
            .collect::<Vec<_>>()
            .join("\n");
        Finding::violation("WN4", t, ev, &format!("{fix}\n（人間が実行してください）"))
    }
}

fn wn5(t: &str, o: &Obs) -> Finding {
    let v = match parse_json(o, "WN5") {
        Ok(v) => v,
        Err(e) => return Finding::undetermined("WN5", t, vec![], &e),
    };
    let Some(ua) = v["UserAuthentication"].as_i64() else {
        return Finding::undetermined(
            "WN5",
            t,
            vec![],
            &format!("WN5 JSON has no numeric UserAuthentication: {v}"),
        );
    };
    let ev = vec![format!("RDP-Tcp UserAuthentication = {ua}")];
    if ua == 1 {
        Finding::clean("WN5", t, ev)
    } else {
        Finding::violation(
            "WN5",
            t,
            ev,
            "システムのプロパティ →「リモート」→「ネットワークレベル認証でリモートデスクトップを実行しているコンピューターからのみ接続を許可する」を有効にしてください（AWS の A1 で 3389 の公開も確認）",
        )
    }
}

/// WN1–WN5 for a host already identified as Windows by W0.
pub fn evaluate(ctx: &Ctx, t: &str, p: &mut Probe<'_>, memo: &mut Memo) -> Vec<Finding> {
    let mut o = Vec::new();
    for w in Remote::WIN {
        o.push(p.get(Remote::Win(w)));
    }
    let age = wn2_age(ctx, &o[1]);
    vec![
        wn1(ctx, t, &o[0], memo),
        wn2(t, &age),
        wn3(t, &o[2], &age),
        wn4(t, &o[3]),
        wn5(t, &o[4]),
    ]
}
