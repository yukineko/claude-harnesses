//! Findings, the four-valued per-rule verdict, report rows and the overwatch
//! review-queue append (canon §1, §2, §8; CONTRACT §8).
//!
//! The verdict is four-valued, not the shared three-valued
//! `harness_core::verdict::Verdict`, because canon §2 requires
//! `NotApplicable` as a value distinct from `Clean`. The fail-closed property
//! is kept by construction: a `Finding` can only be built through
//! [`Finding::clean`] / [`Finding::not_applicable`] / [`Finding::violation`] /
//! [`Finding::undetermined`]; a violation without a fix and an undetermined
//! without a reason are replaced by placeholders that say so, never by a pass.

use crate::redact;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Clean,
    NotApplicable,
    Violation,
    Undetermined,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Clean => "clean",
            Status::NotApplicable => "not_applicable",
            Status::Violation => "violation",
            Status::Undetermined => "undetermined",
        }
    }
    /// Violation and Undetermined make the command exit non-zero (canon §2).
    pub fn is_failing(self) -> bool {
        matches!(self, Status::Violation | Status::Undetermined)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Attached {
    pub instance_id: Option<String>,
    pub name: Option<String>,
    pub eni: String,
}

/// A1/A2 location data (canon §5.1).
#[derive(Clone, Debug, Serialize)]
pub struct SgDetail {
    pub region: String,
    pub sg_id: String,
    pub sg_name: Option<String>,
    /// `None` = the attachments could not be determined (never shown as `[]`).
    pub attached: Option<Vec<Attached>>,
    pub protocol: String,
    pub port_range: String,
    pub cidr: String,
    pub sgr_id: String,
    pub console_path: String,
    pub cli: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub id: String,
    pub target: String,
    pub rule: String,
    pub verdict: Status,
    pub evidence: Vec<String>,
    pub fix: Option<String>,
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory_fetched_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apt_index_time: Option<i64>,
    #[serde(flatten)]
    pub sg: Option<SgDetail>,
}

impl Finding {
    fn base(rule: &str, target: &str, verdict: Status, evidence: Vec<String>) -> Finding {
        Finding {
            id: format!("{rule}:{target}"),
            target: target.to_string(),
            rule: rule.to_string(),
            verdict,
            evidence: redact::clean_evidence(evidence),
            fix: None,
            reason: None,
            advisory_fetched_at: None,
            apt_index_time: None,
            sg: None,
        }
    }

    pub fn clean(rule: &str, target: &str, evidence: Vec<String>) -> Finding {
        Self::base(rule, target, Status::Clean, evidence)
    }

    pub fn not_applicable(rule: &str, target: &str, evidence: Vec<String>, why: &str) -> Finding {
        let mut f = Self::base(rule, target, Status::NotApplicable, evidence);
        f.reason = Some(redact::clean_text(why, " / "));
        f
    }

    pub fn violation(rule: &str, target: &str, evidence: Vec<String>, fix: &str) -> Finding {
        let mut f = Self::base(rule, target, Status::Violation, evidence);
        let fix = redact::clean_text(fix, "\n");
        f.fix = Some(if fix.trim().is_empty() {
            // Unreachable by construction (every caller passes the canon fix);
            // kept non-empty so a violation can never read as "nothing to do".
            format!("{rule}: see docs/hostaudit-baseline-v1.md for the fix")
        } else {
            fix
        });
        f
    }

    pub fn undetermined(rule: &str, target: &str, evidence: Vec<String>, reason: &str) -> Finding {
        let mut f = Self::base(rule, target, Status::Undetermined, evidence);
        let r = redact::cap_bytes(&redact::clean_text(reason, " / "), 2048);
        f.reason = Some(if r.trim().is_empty() {
            "could not be determined (no further detail)".to_string()
        } else {
            r
        });
        f
    }

    pub fn with_id(mut self, id: String) -> Finding {
        self.id = id;
        self
    }

    pub fn to_value(&self) -> serde_json::Value {
        match serde_json::to_value(self) {
            Ok(v) => v,
            // Serializing plain strings/ints cannot fail; if it ever did, the
            // row says so instead of vanishing.
            Err(e) => serde_json::json!({
                "id": self.id, "target": self.target, "rule": self.rule,
                "verdict": "undetermined", "evidence": [],
                "fix": null, "reason": format!("finding not serializable: {e}")
            }),
        }
    }
}

/// Exit code for a finished command (CONTRACT §2): 0 only if every finding is
/// clean / not_applicable. An empty finding list is a bug, never a pass.
pub fn exit_code(findings: &[Finding]) -> i32 {
    if findings.is_empty() || findings.iter().any(|f| f.verdict.is_failing()) {
        1
    } else {
        0
    }
}

/// Write to stdout through the final redaction pass.
pub fn print_out(text: &str) {
    let mut s = redact::for_output(text);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// Write to stderr through the final redaction pass.
pub fn print_err(text: &str) {
    let mut s = redact::for_output(text);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    let _ = std::io::stderr().lock().write_all(s.as_bytes());
}

/// Render findings: `{"findings":[...]}` with `json`, a listing otherwise.
pub fn render(findings: &[Finding], json: bool) {
    if json {
        let arr: Vec<serde_json::Value> = findings.iter().map(Finding::to_value).collect();
        print_out(&serde_json::json!({ "findings": arr }).to_string());
        return;
    }
    let mut s = String::new();
    for f in findings {
        s.push_str(&format!(
            "{:<14} {:<4} {}\n",
            f.verdict.label().to_uppercase(),
            f.rule,
            f.target
        ));
        for e in &f.evidence {
            s.push_str(&format!("    evidence: {e}\n"));
        }
        if let Some(r) = &f.reason {
            s.push_str(&format!("    reason:   {r}\n"));
        }
        if let Some(fx) = &f.fix {
            for (i, l) in fx.lines().enumerate() {
                let head = if i == 0 { "fix:     " } else { "         " };
                s.push_str(&format!("    {head} {l}\n"));
            }
        }
    }
    let (v, u) = counts(findings);
    s.push_str(&format!(
        "hostaudit: {} findings, {v} violation, {u} undetermined\n",
        findings.len()
    ));
    print_out(&s);
}

pub fn counts(findings: &[Finding]) -> (usize, usize) {
    let v = findings
        .iter()
        .filter(|f| f.verdict == Status::Violation)
        .count();
    let u = findings
        .iter()
        .filter(|f| f.verdict == Status::Undetermined)
        .count();
    (v, u)
}

/// Append one row per finding to `~/.hostaudit/reports.jsonl` (canon §8).
pub fn append_reports(dir: &Path, findings: &[Finding], now: i64) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join("reports.jsonl");
    let mut body = String::new();
    for f in findings {
        let mut v = f.to_value();
        if let Some(o) = v.as_object_mut() {
            o.insert("time".to_string(), serde_json::json!(now));
        }
        body.push_str(&v.to_string());
        body.push('\n');
    }
    let body = redact::for_output(&body);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    file.write_all(body.as_bytes())
        .map_err(|e| format!("cannot append to {}: {e}", path.display()))
}

/// Append every Violation / Undetermined finding to overwatch's
/// `review_findings.jsonl` through overwatch's own library writer
/// (`overwatch::store::append_review_finding`), so the path (keyed by the
/// main-worktree root of `cwd`) and the row schema are overwatch's, not a
/// copy. Rows are stored `Unverified`: hostaudit is an observer, not an
/// adversarial verifier (same rule as `overwatch::store::record_finding`).
pub fn append_review_queue(cwd: &Path, findings: &[Finding], now: i64) -> Result<usize, String> {
    let mut n = 0;
    for f in findings.iter().filter(|f| f.verdict.is_failing()) {
        let detail = match (&f.reason, f.evidence.first()) {
            (Some(r), _) => r.clone(),
            (None, Some(e)) => e.clone(),
            (None, None) => String::new(),
        };
        let summary = redact::for_output(&redact::cap_bytes(
            &format!(
                "hostaudit {} {} {}: {}",
                f.rule,
                f.target,
                f.verdict.label(),
                detail
            ),
            400,
        ));
        let severity = match f.verdict {
            Status::Violation => "high",
            _ => "medium",
        };
        let rf = overwatch::review_finding::ReviewFinding::new(
            format!("hostaudit:{}", f.id),
            "hostaudit".to_string(),
            Some(severity.to_string()),
            summary,
            None,
            f.fix.as_ref().map(|x| redact::for_output(x)),
            now,
        )
        .with_verdict(overwatch::review_finding::AuditVerdict::Unverified);
        overwatch::store::append_review_finding(cwd, &rf)
            .map_err(|e| format!("overwatch review_findings.jsonl append failed: {e}"))?;
        n += 1;
    }
    Ok(n)
}

/// Record a finished command's findings (report + review queue) and return the
/// exit code. A recording failure is reported on stderr and makes the exit
/// non-zero: a result nobody can read later is not a clean result.
pub fn finish(ctx: &crate::config::Ctx, findings: &[Finding], json: bool) -> i32 {
    render(findings, json);
    let mut code = exit_code(findings);
    if let Err(e) = append_reports(&ctx.state_dir(), findings, ctx.now) {
        print_err(&format!(
            "hostaudit: report NOT recorded: {e} — treating the run as not clean"
        ));
        code = 1;
    }
    if let Err(e) = append_review_queue(&ctx.cwd, findings, ctx.now) {
        print_err(&format!(
            "hostaudit: review-queue NOT updated: {e} — treating the run as not clean"
        ));
        code = 1;
    }
    code
}
