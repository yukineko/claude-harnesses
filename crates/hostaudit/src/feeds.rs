//! v1.1 external feeds (canon v1.1 §6, CONTRACT §12.4): CISA KEV and
//! endoflife.date, each with a local cache that records its fetch time.
//!
//! A failed fetch is NEVER an empty feed: it falls back to a cache no older
//! than the shared `osv_cache_max_age_hours`, otherwise it is `Err` (the
//! caller turns that into `kev: unknown` / an Undetermined item). Only the
//! fixed KEV URL and the product slug leave the machine.

use crate::config::Ctx;
use chrono::NaiveDate;
use harness_core::verdict::Required;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A feed body plus when it was fetched (from the cache when the fetch failed).
#[derive(Clone, Debug)]
pub struct Fetched<T> {
    pub data: T,
    pub fetched_at: i64,
    pub from_cache: bool,
}

/// Read a `file://` path or GET an http(s) URL (`curl -fsSL`, exit status
/// decides). Anything else is an error.
pub fn fetch_text(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return match harness_core::boundary::read_to_string(Path::new(path)).require() {
            Required::Determined(Some(t)) => Ok(t),
            Required::Determined(None) => Err(format!("{path} does not exist")),
            Required::Blocked(why) => Err(why.to_string()),
        };
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        let mut curl = std::process::Command::new("curl");
        curl.args(["-fsSL", "--max-time", "120"])
            .arg(url)
            .stdin(std::process::Stdio::null());
        return match harness_core::boundary::run_with_timeout(&mut curl, Duration::from_secs(150))
            .require()
        {
            Required::Determined(o) => match o.stdout_on_success().require() {
                Required::Determined(s) => Ok(s),
                Required::Blocked(why) => Err(why.to_string()),
            },
            Required::Blocked(why) => Err(why.to_string()),
        };
    }
    Err(format!("unsupported feed URL {url:?}"))
}

fn cache_file(ctx: &Ctx, dir: &str, key: &str) -> PathBuf {
    ctx.state_dir().join(dir).join(format!("{key}.json"))
}

fn write_cache(ctx: &Ctx, dir: &str, key: &str, data: &Value) -> Result<(), String> {
    let p = cache_file(ctx, dir, key);
    let d = p.parent().ok_or("cache path has no parent")?;
    std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    let body = serde_json::json!({"key": key, "fetched_at": ctx.now, "data": data});
    let tmp = d.join(format!(".{key}.json.{}", std::process::id()));
    std::fs::write(&tmp, body.to_string())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("cannot replace {}: {e}", p.display()))
}

fn read_cache(ctx: &Ctx, dir: &str, key: &str) -> Result<(Value, i64), String> {
    let p = cache_file(ctx, dir, key);
    let text = match harness_core::boundary::read_to_string(&p).require() {
        Required::Determined(Some(t)) => t,
        Required::Determined(None) => return Err("no cache".to_string()),
        Required::Blocked(why) => return Err(format!("cache unreadable: {why}")),
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("cache corrupt: {e}"))?;
    if v["key"].as_str() != Some(key) {
        return Err("cache is for another key".to_string());
    }
    let fetched_at = v["fetched_at"].as_i64().ok_or("cache has no fetched_at")?;
    let age = ctx.now - fetched_at;
    if age < 0 || age > ctx.osv_max_age_secs {
        return Err(format!(
            "cache is {}h old (limit {}h)",
            age / 3600,
            ctx.osv_max_age_secs / 3600
        ));
    }
    Ok((v["data"].clone(), fetched_at))
}

/// Fetch `url`, reduce the body with `slim` (which also validates the shape),
/// cache the reduced value under `dir/key`; on any failure use a fresh cache.
fn cached(
    ctx: &Ctx,
    dir: &str,
    key: &str,
    url: &str,
    slim: impl Fn(Value) -> Result<Value, String>,
) -> Result<Fetched<Value>, String> {
    let fetched = fetch_text(url).and_then(|t| {
        let v: Value = serde_json::from_str(&t).map_err(|e| format!("not JSON: {e}"))?;
        slim(v)
    });
    match fetched {
        Ok(data) => {
            if let Err(e) = write_cache(ctx, dir, key, &data) {
                crate::finding::print_err(&format!("hostaudit: {dir} not written: {e}"));
            }
            Ok(Fetched {
                data,
                fetched_at: ctx.now,
                from_cache: false,
            })
        }
        Err(fetch_err) => match read_cache(ctx, dir, key) {
            Ok((data, fetched_at)) => Ok(Fetched {
                data,
                fetched_at,
                from_cache: true,
            }),
            Err(cache_err) => Err(format!("fetch failed ({fetch_err}); {cache_err}")),
        },
    }
}

// ───────────────────────────── KEV ─────────────────────────────────────────

/// Load the KEV catalogue as a set of CVE ids. An empty or list-less
/// catalogue is `Err` (KEV unknown), never an empty set.
pub fn load_kev(ctx: &Ctx) -> Result<Fetched<BTreeSet<String>>, String> {
    let f = cached(ctx, "kev-cache", "kev", &ctx.kev_url, |v| {
        let arr = v["vulnerabilities"]
            .as_array()
            .ok_or("KEV feed has no `vulnerabilities` array")?;
        // CONTRACT §12.10 item 2: an empty catalogue is not "no CVE is in KEV".
        if arr.is_empty() {
            return Err("KEV feed has an empty `vulnerabilities` list".into());
        }
        let mut ids = Vec::new();
        for x in arr {
            let id = x["cveID"]
                .as_str()
                .ok_or("KEV entry without a string `cveID`")?;
            ids.push(Value::String(id.to_string()));
        }
        Ok(serde_json::json!({ "vulnerabilities": ids }))
    })?;
    let ids = f.data["vulnerabilities"]
        .as_array()
        .ok_or("cached KEV data has no list")?
        .iter()
        .map(|x| {
            x.as_str()
                .map(str::to_string)
                .ok_or("cached KEV id not a string")
        })
        .collect::<Result<BTreeSet<String>, &str>>()?;
    if ids.is_empty() {
        return Err("cached KEV list is empty; KEV unknown".into());
    }
    Ok(Fetched {
        data: ids,
        fetched_at: f.fetched_at,
        from_cache: f.from_cache,
    })
}

// ───────────────────────────── endoflife.date ──────────────────────────────

/// An `eol` / `extendedSupport` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// support ends at the end of this UTC day (unix secs of 00:00 that day)
    Until(i64),
    /// `true` for `eol` = already ended; for `extendedSupport` = available.
    Flag(bool),
}

#[derive(Clone, Debug)]
pub struct Cycle {
    pub cycle: String,
    /// `None` = missing or not a date/bool: the cycle cannot be judged.
    pub eol: Option<Support>,
    /// `None` = no `extendedSupport` key (no extended period).
    pub ext: Option<Support>,
    pub latest: Option<String>,
}

pub const DAY: i64 = 86_400;

fn parse_support(v: &Value) -> Option<Support> {
    match v {
        Value::Bool(b) => Some(Support::Flag(*b)),
        Value::String(s) => NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|t| Support::Until(t.and_utc().timestamp())),
        _ => None,
    }
}

fn cycle_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Endoflife data of one product slug (`[a-z0-9-]+` only).
pub fn load_eol(ctx: &Ctx, slug: &str) -> Result<Fetched<Vec<Cycle>>, String> {
    if slug.is_empty()
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!("invalid product slug {slug:?}"));
    }
    let url = ctx.eol_url.replace("{product}", slug);
    let f = cached(ctx, "eol-cache", slug, &url, |v| {
        let arr = v.as_array().ok_or("endoflife feed is not a JSON array")?;
        let mut out = Vec::new();
        for c in arr {
            let cycle = cycle_string(&c["cycle"]).ok_or("endoflife entry without `cycle`")?;
            out.push(serde_json::json!({
                "cycle": cycle,
                "eol": c["eol"],
                "extendedSupport": c.get("extendedSupport").cloned().unwrap_or(Value::Null),
                "latest": c["latest"],
            }));
        }
        Ok(Value::Array(out))
    })?;
    let cycles = f
        .data
        .as_array()
        .ok_or("cached endoflife data is not an array")?
        .iter()
        .filter_map(|c| {
            Some(Cycle {
                cycle: cycle_string(&c["cycle"])?,
                eol: parse_support(&c["eol"]),
                ext: match &c["extendedSupport"] {
                    Value::Null => None,
                    // A present but unreadable value is treated as "no
                    // extended support" (the more restrictive reading).
                    other => Some(parse_support(other).unwrap_or(Support::Flag(false))),
                },
                latest: c["latest"].as_str().map(str::to_string),
            })
        })
        .collect();
    Ok(Fetched {
        data: cycles,
        fetched_at: f.fetched_at,
        from_cache: f.from_cache,
    })
}

/// Longest cycle that is a dot-boundary prefix of `version` (canon v1.1 §4.1).
pub fn match_cycle<'a>(cycles: &'a [Cycle], version: &str) -> Option<&'a Cycle> {
    cycles
        .iter()
        .filter(|c| {
            version == c.cycle
                || (version.starts_with(&c.cycle) && version[c.cycle.len()..].starts_with('.'))
        })
        .max_by_key(|c| c.cycle.len())
}

/// State of one support period at `now`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    /// over
    Ended,
    /// still running; `Some(days)` = whole days left until the end day is over
    Running(Option<i64>),
}

/// `eol` semantics: a date expires once `now` is past that UTC day; `true` =
/// ended; `false` = supported. `None` (unreadable) is `Err`.
pub fn eol_state(s: Option<Support>, now: i64) -> Result<Period, String> {
    match s {
        Some(Support::Flag(true)) => Ok(Period::Ended),
        Some(Support::Flag(false)) => Ok(Period::Running(None)),
        Some(Support::Until(day)) => {
            if now >= day + DAY {
                Ok(Period::Ended)
            } else {
                Ok(Period::Running(Some(
                    (day - now.div_euclid(DAY) * DAY) / DAY,
                )))
            }
        }
        None => Err("the cycle has no readable `eol` value".to_string()),
    }
}

/// `extendedSupport` semantics: `true` / future date = still in the extended
/// period; `false` / past date / absent = none.
pub fn ext_running(s: Option<Support>, now: i64) -> bool {
    match s {
        Some(Support::Flag(b)) => b,
        Some(Support::Until(day)) => now < day + DAY,
        None => false,
    }
}

pub fn ymd(day: i64) -> String {
    chrono::DateTime::from_timestamp(day, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| day.to_string())
}

pub fn support_text(s: Option<Support>) -> String {
    match s {
        Some(Support::Until(d)) => ymd(d),
        Some(Support::Flag(true)) => "true".into(),
        Some(Support::Flag(false)) => "false".into(),
        None => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(cycle: &str) -> Cycle {
        Cycle {
            cycle: cycle.into(),
            eol: Some(Support::Flag(false)),
            ext: None,
            latest: None,
        }
    }

    #[test]
    fn longest_dot_boundary_prefix() {
        let cs = vec![c("2"), c("2.2")];
        assert_eq!(
            match_cycle(&cs, "2.2.1").map(|x| x.cycle.as_str()),
            Some("2.2")
        );
        assert_eq!(
            match_cycle(&cs, "2.20.1").map(|x| x.cycle.as_str()),
            Some("2")
        );
        assert!(match_cycle(&cs, "20.1").is_none());
        assert_eq!(match_cycle(&cs, "2").map(|x| x.cycle.as_str()), Some("2"));
    }

    #[test]
    fn eol_day_semantics() {
        let day = 1_000 * DAY;
        assert_eq!(
            eol_state(Some(Support::Until(day)), day + 10),
            Ok(Period::Running(Some(0)))
        );
        assert_eq!(
            eol_state(Some(Support::Until(day)), day + DAY),
            Ok(Period::Ended)
        );
        assert_eq!(
            eol_state(Some(Support::Until(day + 89 * DAY)), day + 5),
            Ok(Period::Running(Some(89)))
        );
        assert!(eol_state(None, day).is_err());
    }
}
