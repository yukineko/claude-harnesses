//! B9 certificate probe (canon §3.3 B9, CONTRACT §4.5).
//!
//! `HOSTAUDIT_TLS_PROBE <hostname> 443` when set; otherwise the built-in probe
//! `openssl s_client -connect <host>:443 -servername <host>` piped into
//! `openssl x509 -noout -enddate`. Either way only exit 0 plus a parseable
//! `notAfter=` line is an observation; anything else is `Err` (undetermined).

use crate::config::Ctx;
use harness_core::verdict::Required;
use std::process::{Command, Stdio};
use std::time::Duration;

fn run_ok(c: &mut Command, secs: u64, stdin: Option<Vec<u8>>) -> Result<String, String> {
    let res =
        harness_core::boundary::run_with_timeout_and_stdin(c, Duration::from_secs(secs), stdin);
    match res.require() {
        Required::Determined(o) => match o.stdout_on_success().require() {
            Required::Determined(s) => Ok(s),
            Required::Blocked(why) => Err(why.to_string()),
        },
        Required::Blocked(why) => Err(why.to_string()),
    }
}

/// Parse `notAfter=Jan 20 00:00:00 2027 GMT` → unix seconds.
pub fn parse_not_after(out: &str) -> Result<i64, String> {
    let line = out
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("notAfter="))
        .ok_or_else(|| format!("probe output has no notAfter= line: {:?}", out.trim()))?;
    let norm: Vec<&str> = line.split_whitespace().collect();
    let (date, tz) = match norm.as_slice() {
        [m, d, t, y, tz] => (format!("{m} {d} {t} {y}"), *tz),
        _ => return Err(format!("unparseable notAfter {line:?}")),
    };
    if tz != "GMT" && tz != "UTC" {
        return Err(format!("notAfter {line:?} is not in GMT"));
    }
    chrono::NaiveDateTime::parse_from_str(&date, "%b %d %H:%M:%S %Y")
        .map(|dt| dt.and_utc().timestamp())
        .map_err(|e| format!("unparseable notAfter {line:?}: {e}"))
}

/// Certificate notAfter (unix seconds) for `host:443`.
pub fn probe(ctx: &Ctx, host: &str) -> Result<i64, String> {
    if host.is_empty() || host.starts_with('-') {
        return Err(format!("refusing to probe {host:?}"));
    }
    let out = match &ctx.tls_probe {
        Some(p) => {
            let mut c = Command::new(p);
            c.arg(host).arg("443").stdin(Stdio::null());
            run_ok(&mut c, 60, None)?
        }
        None => {
            let connect = if host.contains(':') {
                format!("[{host}]:443")
            } else {
                format!("{host}:443")
            };
            let mut s = Command::new("openssl");
            s.args(["s_client", "-connect", &connect, "-servername", host])
                .stdin(Stdio::null());
            let pem = run_ok(&mut s, 30, None).map_err(|e| format!("openssl s_client: {e}"))?;
            let mut x = Command::new("openssl");
            x.args(["x509", "-noout", "-enddate"]).stdin(Stdio::piped());
            run_ok(&mut x, 30, Some(pem.into_bytes())).map_err(|e| format!("openssl x509: {e}"))?
        }
    };
    parse_not_after(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openssl_dates() {
        assert_eq!(
            parse_not_after("notAfter=Jan 20 00:00:00 2027 GMT\n"),
            Ok(1_800_403_200)
        );
        assert!(parse_not_after("notAfter=Jun  1 00:00:00 2027 GMT\n").is_ok());
        assert!(parse_not_after("garbage").is_err());
    }
}
