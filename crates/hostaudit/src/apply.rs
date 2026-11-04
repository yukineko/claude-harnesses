//! `hostaudit aws apply` — the ONLY mutating operation (canon §5, CONTRACT §7).
//!
//! Default is manual; this path runs only on an explicit per-finding human
//! confirmation and never in an autonomous context. Every refusal happens
//! before any dry-run or mutating call.

use crate::aws::{self, Call, Mutation, SgRule};
use crate::config::Ctx;
use crate::finding::{self, Finding, SgDetail};
use std::io::{BufRead, IsTerminal, Write};

const REFUSED: i32 = 3;

struct Target {
    rule: &'static str,
    region: String,
    sg: String,
    sgr: String,
}

fn parse_id(id: &str) -> Result<Target, String> {
    let parts: Vec<&str> = id.split(':').collect();
    let rule = match parts.first() {
        Some(&"A1") => "A1",
        Some(&"A2") => "A2",
        _ => {
            return Err(format!(
                "finding {id:?} is not a security-group finding (only A1/A2 can be applied; everything else is fixed by hand)"
            ))
        }
    };
    match parts.as_slice() {
        [_, region, sg, sgr]
            if !region.is_empty() && sg.starts_with("sg-") && sgr.starts_with("sgr-") =>
        {
            Ok(Target {
                rule,
                region: region.to_string(),
                sg: sg.to_string(),
                sgr: sgr.to_string(),
            })
        }
        _ => Err(format!(
            "finding id {id:?} is not of the form {rule}:<region>:<sg-id>:<sgr-id>"
        )),
    }
}

fn valid_v4_cidr(c: &str) -> bool {
    let Some((ip, len)) = c.split_once('/') else {
        return false;
    };
    let Ok(len) = len.parse::<u8>() else {
        return false;
    };
    len <= 32 && ip.parse::<std::net::Ipv4Addr>().is_ok()
}

fn refuse(msg: &str) -> i32 {
    finding::print_err(&format!("hostaudit aws apply: REFUSED — {msg}"));
    REFUSED
}

fn rules_of(ctx: &Ctx, t: &Target) -> Result<Vec<SgRule>, String> {
    let v = aws::call(
        ctx,
        &Call::SecurityGroupRules {
            region: &t.region,
            group: Some(&t.sg),
        },
    )
    .map_err(|e| format!("describe-security-group-rules: {e}"))?;
    Ok(aws::parse_rules(&v)?
        .into_iter()
        .filter(|r| r.group_id == t.sg && !r.is_egress)
        .collect())
}

fn dry_run_ok(ctx: &Ctx, m: &Mutation) -> Result<(), String> {
    match aws::mutate(ctx, m) {
        Ok((code, stderr)) if code != 0 && stderr.contains("DryRunOperation") => Ok(()),
        Ok((code, stderr)) => Err(format!(
            "dry-run of `{}` did not confirm permission (exit {code}): {}",
            m.display(),
            stderr.trim()
        )),
        Err(e) => Err(format!("dry-run of `{}` failed: {e}", m.display())),
    }
}

fn record(ctx: &Ctx, f: &Finding) {
    if let Err(e) = finding::append_reports(&ctx.state_dir(), std::slice::from_ref(f), ctx.now) {
        finding::print_err(&format!("hostaudit: report NOT recorded: {e}"));
    }
    if let Err(e) = finding::append_review_queue(&ctx.cwd, std::slice::from_ref(f), ctx.now) {
        finding::print_err(&format!("hostaudit: review-queue NOT updated: {e}"));
    }
}

/// `hostaudit aws apply --finding <id> [--confirm <sgr>] [--replace-cidr <cidr>]`.
pub fn run(ctx: &Ctx, finding_id: &str, confirm: Option<&str>, replace_cidr: Option<&str>) -> i32 {
    // (0) never in an autonomous context — even with --confirm.
    if std::env::var_os("CLAUDECODE").is_some() {
        return refuse(
            "CLAUDECODE is set (autonomous context). Security-group changes need a human: run this yourself in a terminal outside Claude Code, or apply the manual fix printed by `hostaudit aws`.",
        );
    }
    let t = match parse_id(finding_id) {
        Ok(t) => t,
        Err(e) => return refuse(&e),
    };
    let interactive = std::io::stdin().is_terminal();
    match confirm {
        Some(c) if c != t.sgr => {
            return refuse(&format!(
                "--confirm {c:?} does not match the finding's rule id {} (confirmation must name exactly the rule being removed)",
                t.sgr
            ))
        }
        Some(_) => {}
        None if !interactive => {
            return refuse(&format!(
                "no confirmation: stdin is not a terminal and --confirm was not given. Re-run with `--confirm {}` after reviewing the change, or run interactively.",
                t.sgr
            ))
        }
        None => {}
    }
    if let Some(c) = replace_cidr {
        if !valid_v4_cidr(c) {
            return refuse(&format!(
                "--replace-cidr {c:?} is not an IPv4 CIDR (a.b.c.d/n); IPv6 replacements must be done by hand"
            ));
        }
    }

    // (1) re-locate the finding: AW1 + AW3.
    if let Err(e) = aws::call(ctx, &Call::CallerIdentity) {
        return refuse(&format!("{}: {e}", aws::LOGIN_EXPIRED));
    }
    let rules = match rules_of(ctx, &t) {
        Ok(r) => r,
        Err(e) => return refuse(&format!("cannot re-locate the finding: {e}")),
    };
    let Some(rule) = rules.iter().find(|r| r.sgr_id == t.sgr).cloned() else {
        return refuse(&format!(
            "{} is not an ingress rule of {} in {} any more (finding unknown or already fixed)",
            t.sgr, t.sg, t.region
        ));
    };
    if rule.offending_rule() != Some(t.rule) {
        return refuse(&format!(
            "{} ({}) is no longer an {} finding",
            t.sgr,
            rule.describe(),
            t.rule
        ));
    }

    // (2) port-22 lockout: same SG only (2026-11-04 ruling).
    let closes_ssh = rule.covers_tcp(22)
        && !rules
            .iter()
            .any(|r| r.sgr_id != rule.sgr_id && r.covers_tcp(22));
    if closes_ssh && replace_cidr.is_none() {
        return refuse(&format!(
            "WARNING: removing {} leaves no other ingress rule in {} that admits tcp/22 — this would lock out ssh. Re-run with --replace-cidr <your-ip>/32 to add a narrower ssh rule before the revoke.",
            rule.sgr_id, t.sg
        ));
    }

    // Replacement rule (authorize before revoke).
    let (rep_proto, rep_port): (String, Option<String>) = if closes_ssh || rule.all_protocols() {
        ("tcp".into(), Some("22".into()))
    } else {
        (rule.protocol_label(), Some(rule.port_range()))
    };
    let authorize = |dry_run: bool| {
        replace_cidr.map(|cidr| Mutation::Authorize {
            region: &t.region,
            group: &t.sg,
            protocol: &rep_proto,
            port: rep_port.as_deref(),
            cidr,
            dry_run,
        })
    };
    let revoke = |dry_run: bool| Mutation::Revoke {
        region: &t.region,
        group: &t.sg,
        sgr: &t.sgr,
        dry_run,
    };

    // (a) the diff.
    let mut diff = format!(
        "hostaudit aws apply {finding_id}\n  security group {} in {}\n  - remove  {}\n",
        t.sg,
        t.region,
        rule.describe()
    );
    if let Some(cidr) = replace_cidr {
        diff.push_str(&format!(
            "  + add     {} {} from {cidr}\n",
            rep_proto,
            rep_port.as_deref().unwrap_or("all")
        ));
    }
    finding::print_out(&diff);

    // Interactive confirmation (only when --confirm was not given).
    if confirm.is_none() {
        let _ = write!(
            std::io::stderr(),
            "Type the rule id ({}) to confirm, anything else aborts: ",
            t.sgr
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let typed = std::io::stdin().lock().read_line(&mut line);
        if typed.is_err() || line.trim() != t.sgr {
            return refuse("confirmation not given");
        }
    }

    // (b) dry-run permission checks.
    if let Some(m) = authorize(true) {
        if let Err(e) = dry_run_ok(ctx, &m) {
            return refuse(&e);
        }
    }
    if let Err(e) = dry_run_ok(ctx, &revoke(true)) {
        return refuse(&e);
    }

    // (d) the change: authorize first, then revoke exactly one rule.
    if let Some(m) = authorize(false) {
        match aws::mutate(ctx, &m) {
            Ok((0, _)) => finding::print_out(&format!("authorized: {}", m.display())),
            Ok((code, stderr)) => {
                finding::print_err(&format!(
                    "hostaudit aws apply: authorize failed (exit {code}): {} — nothing was revoked",
                    stderr.trim()
                ));
                return 1;
            }
            Err(e) => {
                finding::print_err(&format!(
                    "hostaudit aws apply: authorize did not complete: {e} — nothing was revoked"
                ));
                return 1;
            }
        }
    }
    let rv = revoke(false);
    let revoke_result = aws::mutate(ctx, &rv);
    let revoked = match &revoke_result {
        Ok((0, _)) => {
            finding::print_out(&format!("revoked: {}", rv.display()));
            true
        }
        Ok((code, stderr)) => {
            finding::print_err(&format!(
                "hostaudit aws apply: revoke failed (exit {code}): {}",
                stderr.trim()
            ));
            false
        }
        Err(e) => {
            finding::print_err(&format!(
                "hostaudit aws apply: revoke did not complete: {e}"
            ));
            false
        }
    };

    // (e) re-audit with AW3.
    let after = rules_of(ctx, &t);
    let mut evidence = vec![format!("apply: revoke {}", rule.describe())];
    if let Some(c) = replace_cidr {
        evidence.push(format!(
            "apply: replacement {rep_proto} {} from {c}",
            rep_port.as_deref().unwrap_or("all")
        ));
    }
    let verdict = match &after {
        Err(e) => Err(format!("re-audit failed: {e}")),
        Ok(rs) if rs.iter().any(|r| r.sgr_id == t.sgr) => Ok(false),
        Ok(rs) => match replace_cidr {
            Some(c)
                if !rs.iter().any(|r| {
                    r.cidr_v4.as_deref() == Some(c) && (r.covers_tcp(22) || !closes_ssh)
                }) =>
            {
                Err(format!(
                    "re-audit: the replacement rule from {c} is not listed"
                ))
            }
            _ => Ok(true),
        },
    };
    let console = aws::console_path(&t.sg, &t.sgr);
    let cli = aws::revoke_cli(&t.region, &t.sg, &t.sgr);
    let mut f = match verdict {
        Ok(true) if revoked => {
            evidence.push(format!("re-audit: {} is gone from {}", t.sgr, t.sg));
            Finding::clean(t.rule, &t.sg, evidence)
        }
        Ok(true) => Finding::undetermined(
            t.rule,
            &t.sg,
            evidence,
            "the revoke call failed but the rule is no longer listed; verify by hand",
        ),
        Ok(false) => {
            evidence.push(format!("re-audit: {} is STILL present in {}", t.sgr, t.sg));
            Finding::violation(
                t.rule,
                &t.sg,
                evidence,
                &format!("The rule is still present. Remove it by hand: {console}\nCLI: {cli}"),
            )
        }
        Err(e) => Finding::undetermined(t.rule, &t.sg, evidence, &e),
    }
    .with_id(finding_id.to_string());
    f.sg = Some(SgDetail {
        region: t.region.clone(),
        sg_id: t.sg.clone(),
        sg_name: None,
        attached: None,
        protocol: rule.protocol_label(),
        port_range: rule.port_range(),
        cidr: rule.world_cidr().unwrap_or("").to_string(),
        sgr_id: t.sgr.clone(),
        console_path: console,
        cli: vec![cli],
    });
    record(ctx, &f);
    finding::print_out(&format!(
        "re-audit: {} — {}",
        f.verdict.label(),
        f.reason.clone().unwrap_or_else(|| f.evidence.join("; "))
    ));
    if f.verdict == finding::Status::Clean {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert!(parse_id("A1:ap-northeast-1:sg-0aaa:sgr-0db").is_ok());
        assert!(parse_id("A4:db-one").is_err());
        assert!(parse_id("A1:ap-northeast-1:sg-0aaa").is_err());
    }

    #[test]
    fn cidrs() {
        assert!(valid_v4_cidr("198.51.100.7/32"));
        assert!(!valid_v4_cidr("198.51.100.7"));
        assert!(!valid_v4_cidr("::/0"));
        assert!(!valid_v4_cidr("10.0.0.0/33"));
    }
}
