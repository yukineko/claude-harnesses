//! Findings, the five-valued per-rule verdict, report rows and the overwatch
//! review-queue append (canon v1 §1, §2, §8; v1.1 §1; CONTRACT §8, §12.1).
//!
//! The verdict is five-valued, not the shared three-valued
//! `harness_core::verdict::Verdict`, because canon v1 §2 requires
//! `NotApplicable` as a value distinct from `Clean` and canon v1.1 §1 adds
//! `Warning` (observed, meets the baseline, but needs a human's plan; exit 0,
//! always counted separately, never sent to the review queue). The
//! fail-closed property is kept by construction: a `Finding` can only be
//! built through [`Finding::clean`] / [`Finding::not_applicable`] /
//! [`Finding::warning`] / [`Finding::violation`] / [`Finding::undetermined`];
//! a violation or warning without a fix and an undetermined without a reason
//! are replaced by placeholders that say so, never by a pass.

use crate::redact;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Clean,
    NotApplicable,
    Warning,
    Violation,
    Undetermined,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Clean => "clean",
            Status::NotApplicable => "not_applicable",
            Status::Warning => "warning",
            Status::Violation => "violation",
            Status::Undetermined => "undetermined",
        }
    }

    /// Violation and Undetermined make the command exit non-zero (canon v1 §2);
    /// Warning does not (canon v1.1 §1). The same predicate selects what goes
    /// to the overwatch review queue, so warnings are never queued.
    pub fn is_failing(self) -> bool {
        matches!(self, Status::Violation | Status::Undetermined)
    }

    /// Order for multi-item rules (canon v1.1 §1):
    /// Violation > Undetermined > Warning > Clean (> NotApplicable).
    pub fn rank(self) -> u8 {
        match self {
            Status::Violation => 4,
            Status::Undetermined => 3,
            Status::Warning => 2,
            Status::Clean => 1,
            Status::NotApplicable => 0,
        }
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
    /// v1.1 rule-specific fields (`cves`, `priority_counts`, `kev_fetched_at`,
    /// `eol_fetched_at`), flattened into the finding object.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
            extra: serde_json::Map::new(),
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

    /// Canon v1.1 §1: a warning always carries advice in `fix`.
    pub fn warning(rule: &str, target: &str, evidence: Vec<String>, fix: &str) -> Finding {
        let mut f = Self::base(rule, target, Status::Warning, evidence);
        let fix = redact::clean_text(fix, "\n");
        f.fix = Some(if fix.trim().is_empty() {
            format!("{rule}: see docs/hostaudit-baseline-v1.1.md for the advice")
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

    /// Build a finding for `status`, routing `fix` / `reason` to the field the
    /// verdict requires (multi-item rules, canon v1.1 §1).
    pub fn of(
        status: Status,
        rule: &str,
        target: &str,
        evidence: Vec<String>,
        fix: &str,
        reason: &str,
    ) -> Finding {
        match status {
            Status::Violation => Self::violation(rule, target, evidence, fix),
            Status::Warning => Self::warning(rule, target, evidence, fix),
            Status::Undetermined => Self::undetermined(rule, target, evidence, reason),
            Status::Clean => Self::clean(rule, target, evidence),
            Status::NotApplicable => Self::not_applicable(rule, target, evidence, reason),
        }
    }

    pub fn with(mut self, key: &str, v: serde_json::Value) -> Finding {
        self.extra.insert(key.to_string(), v);
        self
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

/// Exit code for a finished command (CONTRACT §2, §12.1): 0 only if every
/// finding is clean / not_applicable / warning. An empty finding list is a
/// bug, never a pass.
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

/// Per-verdict counts (canon v1.1 §1: warning is always its own number).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub violation: usize,
    pub undetermined: usize,
    pub warning: usize,
    pub clean: usize,
    pub not_applicable: usize,
}

impl Counts {
    pub fn of(findings: &[Finding]) -> Counts {
        let mut c = Counts::default();
        for f in findings {
            match f.verdict {
                Status::Violation => c.violation += 1,
                Status::Undetermined => c.undetermined += 1,
                Status::Warning => c.warning += 1,
                Status::Clean => c.clean += 1,
                Status::NotApplicable => c.not_applicable += 1,
            }
        }
        c
    }

    /// `violation V / undetermined U / warning W / clean C / not_applicable N`
    /// (canon v1.1 §1 example, CONTRACT §12.1).
    pub fn line(&self) -> String {
        format!(
            "violation {} / undetermined {} / warning {} / clean {} / not_applicable {}",
            self.violation, self.undetermined, self.warning, self.clean, self.not_applicable
        )
    }

    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "violation": self.violation,
            "undetermined": self.undetermined,
            "warning": self.warning,
            "clean": self.clean,
            "not_applicable": self.not_applicable,
        })
    }
}

/// Render findings: `{"findings":[...], "summary":{...}}` with `json`, a
/// listing otherwise. The listing's last line is the one-line summary the
/// daily task shows (canon v1 §1, v1.1 §1), with warnings counted on their own.
pub fn render(findings: &[Finding], json: bool) {
    let counts = Counts::of(findings);
    if json {
        let arr: Vec<serde_json::Value> = findings.iter().map(Finding::to_value).collect();
        print_out(&serde_json::json!({ "findings": arr, "summary": counts.to_json() }).to_string());
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
    s.push_str(&format!(
        "hostaudit: {} findings — {}\n",
        findings.len(),
        counts.line()
    ));
    print_out(&s);
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

/// Append every Violation / Undetermined finding (never a Warning, canon
/// v1.1 §1) to overwatch's `review_findings.jsonl` through overwatch's own
/// library writer (`overwatch::store::append_review_finding`), so the path
/// (keyed by the main-worktree root of `cwd`) and the row schema are
/// overwatch's, not a copy. Rows are stored `Unverified`: hostaudit is an
/// observer, not an adversarial verifier (same rule as
/// `overwatch::store::record_finding`).
pub fn append_review_queue(cwd: &Path, findings: &[Finding], now: i64) -> Result<usize, String> {
    let mut n = 0;
    for f in findings.iter().filter(|f| queued(f.verdict)) {
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

/// Which verdicts go to the overwatch review queue: Violation and
/// Undetermined only (canon v1 §1); Warning is excluded (canon v1.1 §1).
fn queued(s: Status) -> bool {
    matches!(s, Status::Violation | Status::Undetermined)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_is_counted_apart_and_not_queued_nor_failing() {
        let fs = vec![
            Finding::warning("S5", "h", vec![], "check the repo"),
            Finding::clean("B1", "h", vec![]),
        ];
        assert_eq!(exit_code(&fs), 0);
        let c = Counts::of(&fs);
        assert_eq!(
            (c.warning, c.clean, c.violation, c.undetermined),
            (1, 1, 0, 0)
        );
        assert!(!queued(Status::Warning));
        assert!(queued(Status::Violation) && queued(Status::Undetermined));
        assert!(fs[0].fix.as_deref().is_some_and(|x| !x.is_empty()));
    }
}
