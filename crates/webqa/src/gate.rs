//! The Stop-hook verdict: which specs does the working tree's diff require,
//! do they exist, did the runner run them, and did they pass.
//!
//! Every non-pass resolves to [`Verdict::Violation`] or
//! [`Verdict::Undetermined`]; `Clean` is produced only through
//! [`Verdict::adjudicate`] on an observed-empty findings list.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use globset::{Glob, GlobSet, GlobSetBuilder};
use harness_core::verdict::{Determination, Reason, Verdict};

use crate::config::Config;
use crate::report;

fn build_set(globs: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g).map_err(|e| format!("invalid glob `{g}`: {e}"))?);
    }
    b.build().map_err(|e| format!("cannot build glob set: {e}"))
}

/// Specs required by the touched files, or why that cannot be decided.
/// A map is selected when a touched path matches one of its `sources` or is
/// one of its `specs`. A selected map with no spec, or with a spec file that
/// does not exist, is undetermined (a web change nothing can verify).
fn select_specs(cfg: &Config, root: &Path, touched: &[String]) -> Determination<Vec<String>> {
    let mut specs: Vec<String> = Vec::new();
    for (i, m) in cfg.maps.iter().enumerate() {
        let set = match build_set(&m.sources) {
            Ok(s) => s,
            Err(e) => return Determination::undetermined(format!("[[map]] #{}: {e}", i + 1)),
        };
        let hit: Vec<&String> = touched
            .iter()
            .filter(|t| set.is_match(t.as_str()) || m.specs.iter().any(|s| s == *t))
            .collect();
        if hit.is_empty() {
            continue;
        }
        if m.specs.is_empty() {
            return Determination::undetermined(format!(
                "{} touched but [[map]] #{} lists no specs, so nothing can verify it",
                hit[0],
                i + 1
            ));
        }
        for s in &m.specs {
            if !root.join(s).is_file() {
                return Determination::undetermined(format!(
                    "{} touched but its mapped spec `{s}` does not exist",
                    hit[0]
                ));
            }
            if !specs.contains(s) {
                specs.push(s.clone());
            }
        }
    }
    Determination::known(specs)
}

struct RunOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_command(
    cfg: &Config,
    root: &Path,
    specs: &[String],
    scratch: &Path,
) -> Result<RunOutput, String> {
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let tag = format!("{}-{}", std::process::id(), nanos());
    let out_p: PathBuf = scratch.join(format!("run-{tag}.stdout"));
    let err_p: PathBuf = scratch.join(format!("run-{tag}.stderr"));
    let result = (|| {
        let out_f = std::fs::File::create(&out_p).map_err(|e| format!("cannot create log: {e}"))?;
        let err_f = std::fs::File::create(&err_p).map_err(|e| format!("cannot create log: {e}"))?;
        let mut child = Command::new(&cfg.command[0])
            .args(&cfg.command[1..])
            .arg("--reporter=json")
            .args(specs)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_f))
            .stderr(Stdio::from(err_f))
            .spawn()
            .map_err(|e| format!("cannot spawn `{}`: {e}", cfg.command[0]))?;
        let deadline = Instant::now() + Duration::from_secs(cfg.timeout_secs.max(1));
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "runner exceeded {}s and was killed",
                        cfg.timeout_secs
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("cannot wait on runner: {e}"));
                }
            }
        };
        let read = |p: &Path| {
            std::fs::read(p)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .map_err(|e| format!("cannot read runner output {}: {e}", p.display()))
        };
        Ok(RunOutput {
            code: status.code(),
            stdout: read(&out_p)?,
            stderr: read(&err_p)?,
        })
    })();
    let _ = std::fs::remove_file(&out_p);
    let _ = std::fs::remove_file(&err_p);
    result
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Turn one runner result into a verdict.
fn judge(run: &RunOutput, specs: &[String]) -> Verdict {
    let Some(code) = run.code else {
        return Verdict::undetermined("runner was terminated by a signal; no result");
    };
    let st = match report::parse(&run.stdout) {
        Ok(s) => s,
        Err(e) => {
            return Verdict::undetermined(format!(
                "runner output unusable ({e}); stderr tail: {}",
                report::tail(&run.stderr, 15)
            ))
        }
    };
    if st.unexpected > 0 && code == 0 {
        return Verdict::undetermined(format!(
            "runner exited 0 but reports {} unexpected failure(s): inconsistent",
            st.unexpected
        ));
    }
    if st.unexpected == 0 && code != 0 {
        return Verdict::undetermined(format!(
            "runner exited {code} but reports 0 failures: inconsistent; stderr tail: {}",
            report::tail(&run.stderr, 15)
        ));
    }
    if st.unexpected > 0 {
        let titles = if st.failed_titles.is_empty() {
            String::new()
        } else {
            format!("\n  {}", st.failed_titles.join("\n  "))
        };
        return Verdict::violation(format!(
            "{} Playwright test(s) failed ({} passed) in {}{titles}",
            st.unexpected,
            st.expected,
            specs.join(", ")
        ));
    }
    if st.flaky > 0 {
        return Verdict::violation(format!(
            "{} flaky Playwright test(s) in {} (passed only on retry); a flaky pass is not a pass",
            st.flaky,
            specs.join(", ")
        ));
    }
    if st.errors > 0 {
        return Verdict::undetermined(format!(
            "runner reported {} top-level error(s) despite exit 0",
            st.errors
        ));
    }
    if st.expected == 0 {
        return Verdict::undetermined(format!(
            "no test ran for {} ({} skipped): nothing was verified",
            specs.join(", "),
            st.skipped
        ));
    }
    Verdict::adjudicate(Determination::known(Vec::<Reason>::new()))
}

/// Full evaluation for `root`. `scratch` holds runner logs.
pub fn evaluate(cfg: &Config, root: &Path, scratch: &Path) -> Verdict {
    let touched = match crate::git::touched_files(root) {
        Ok(t) => t,
        Err(e) => return Verdict::undetermined(format!("cannot list touched files: {e}")),
    };
    let specs = match select_specs(cfg, root, &touched) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return Verdict::Undetermined(why),
    };
    if specs.is_empty() {
        // Observed: no touched file maps to any spec.
        return Verdict::adjudicate(Determination::known(Vec::<Reason>::new()));
    }
    match run_command(cfg, root, &specs, scratch) {
        Ok(r) => judge(&r, &specs),
        Err(e) => Verdict::undetermined(e),
    }
}
