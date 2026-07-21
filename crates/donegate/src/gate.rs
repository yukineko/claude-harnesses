//! The gate: pick the checks that apply to what changed, run them, and turn the
//! outcomes into a verdict plus a model-facing block reason.

use std::path::Path;

use globset::{Glob, GlobSetBuilder};

use crate::config::{Check, Config};
use crate::runner::{self, Outcome};

pub struct Verdict {
    /// Checks that actually ran (in order).
    pub ran: Vec<Outcome>,
    /// Names of checks skipped because nothing they watch changed.
    pub skipped: Vec<String>,
    /// True when no git repo was found, so `when_changed` scoping was bypassed.
    pub git_unscoped: bool,
}

impl Verdict {
    /// Required (non-optional) checks that failed — these block the stop.
    pub fn blocking(&self) -> Vec<&Outcome> {
        self.ran
            .iter()
            .filter(|o| !o.passed && !o.optional)
            .collect()
    }

    /// Optional checks that failed — surfaced as warnings only.
    pub fn warnings(&self) -> Vec<&Outcome> {
        self.ran
            .iter()
            .filter(|o| !o.passed && o.optional)
            .collect()
    }

    pub fn all_green(&self) -> bool {
        self.blocking().is_empty()
    }
}

/// Does this check apply, given the changed-file set? A check with no
/// `when_changed` always applies; with one, it applies iff a changed path
/// matches. When `changed` is `None` (no git) every check applies.
fn applies(check: &Check, changed: &Option<Vec<String>>) -> bool {
    let Some(globs) = &check.when_changed else {
        return true;
    };
    let Some(files) = changed else {
        return true; // can't scope without git → run it
    };
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for g in globs {
        if let Ok(glob) = Glob::new(g) {
            b.add(glob);
            any = true;
        }
    }
    if !any {
        return true;
    }
    let set = match b.build() {
        Ok(s) => s,
        Err(_) => return true,
    };
    files.iter().any(|f| set.is_match(f))
}

pub fn evaluate(cfg: &Config, root: &Path) -> Verdict {
    // donegate is RESTRICTIVE: both "not a repo" and "could not determine" map to
    // "no usable scope" → every check applies. Only a SUCCESSFUL scan narrows the
    // check set. Crucially `Failed` (a git sub-command errored) must land here, not
    // in a `Some(vec![])` "nothing changed" that would silently skip every
    // `when_changed` check and pass the Stop gate on an undetermined tree.
    let changed: Option<Vec<String>> = match crate::git::changed_files(root) {
        crate::git::ChangeScan::Files(v) => Some(v),
        crate::git::ChangeScan::NotRepo | crate::git::ChangeScan::Failed => None,
    };
    let git_unscoped = changed.is_none();
    let tmp_dir = cfg.state_dir.join("tmp");

    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    for check in &cfg.checks {
        if applies(check, &changed) {
            ran.push(runner::run_check(
                check,
                root,
                cfg.default_timeout_secs,
                cfg.output_tail_lines,
                &tmp_dir,
            ));
        } else {
            skipped.push(check.name.clone());
        }
    }

    Verdict {
        ran,
        skipped,
        git_unscoped,
    }
}

/// Render one outcome as an indented block for the model / terminal.
fn render_outcome(o: &Outcome) -> String {
    let mark = if o.passed { "✓" } else { "✗" };
    let detail = if o.timed_out {
        "timed out".to_string()
    } else if let Some(err) = &o.spawn_error {
        err.clone()
    } else {
        match o.exit_code {
            Some(c) => format!("exit {c}"),
            None => "killed".to_string(),
        }
    };
    let mut s = format!(
        "{mark} {} ({detail}, {:.1}s)\n    $ {}",
        o.name, o.duration_secs, o.cmd
    );
    if !o.passed {
        let tail = o.output_tail.trim_end();
        if !tail.is_empty() {
            for line in tail.lines() {
                s.push_str("\n    ");
                s.push_str(line);
            }
        }
    }
    s
}

/// The reason string injected back into the model when the stop is blocked.
pub fn block_reason(v: &Verdict, attempt: u32, max: u32) -> String {
    let failing = v.blocking();
    let mut out = format!(
        "🚦 donegate: not done yet — {} required check(s) failed (attempt {attempt}/{max}). \
         Fix them, then finish.\n",
        failing.len()
    );
    for o in &failing {
        out.push('\n');
        out.push_str(&render_outcome(o));
        out.push('\n');
    }
    let warns = v.warnings();
    if !warns.is_empty() {
        out.push_str("\n(optional, not blocking — but worth a look:)\n");
        for o in &warns {
            out.push_str(&format!("  ⚠ {} ({})\n", o.name, o.status()));
        }
    }
    out.push_str(
        "\nWhen they pass, donegate will let you stop. To finish anyway, create a file \
         `.donegate-skip` in the project root with a one-line reason (consumed once). \
         To disable entirely: set DONEGATE_DISABLE=1.",
    );
    out
}

/// A compact human report for manual `donegate gate` runs.
pub fn human_report(v: &Verdict) -> String {
    let mut out = String::new();
    if v.git_unscoped {
        out.push_str("(no git repo — all checks ran unscoped)\n");
    }
    for o in &v.ran {
        out.push_str(&render_outcome(o));
        out.push('\n');
    }
    if !v.skipped.is_empty() {
        out.push_str(&format!(
            "skipped (no matching changes): {}\n",
            v.skipped.join(", ")
        ));
    }
    let blocking = v.blocking();
    if blocking.is_empty() {
        out.push_str("\n✓ all required checks green");
    } else {
        out.push_str(&format!("\n✗ {} required check(s) failed", blocking.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, when: Option<Vec<&str>>) -> Check {
        Check {
            name: name.to_string(),
            cmd: "true".to_string(),
            when_changed: when.map(|v| v.into_iter().map(String::from).collect()),
            timeout_secs: None,
            optional: false,
            workdir: None,
        }
    }

    #[test]
    fn unconditional_check_always_applies() {
        let c = check("build", None);
        assert!(applies(&c, &Some(vec![])));
        assert!(applies(&c, &None));
    }

    #[test]
    fn scoped_check_matches_glob() {
        let c = check("test", Some(vec!["**/*.rs"]));
        assert!(applies(&c, &Some(vec!["src/main.rs".to_string()])));
        assert!(!applies(&c, &Some(vec!["README.md".to_string()])));
    }

    #[test]
    fn scoped_check_runs_when_git_absent() {
        let c = check("test", Some(vec!["**/*.rs"]));
        assert!(applies(&c, &None));
    }
}
