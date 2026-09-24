// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Pins the file-granularity half of the sub-agent scan's fail-closed contract
//! (backlog `fbb3100a`, from `docs/audit-gauge-verdict-paths.md` P1/P5).
//!
//! `subagent_files` already forwards `Undetermined` when the **directory**
//! cannot be read, so "the whole sub-agent dir is unreadable" was fail-closed.
//! The per-**file** read inside the same two functions was not: it dropped the
//! `Err` and carried on, so a session whose sub-agent transcripts were only
//! PARTIALLY readable reported a total indistinguishable from a session that
//! genuinely had no sub-agent spend. That is the dangerous half — the audit
//! measured a 270.00 USD sub-agent turn vanishing into a 0.0075 USD session
//! total with no diagnostic anywhere, which is enough to silence budgetguard.
//!
//! Each direction is pinned with its own control. A fix that simply made the
//! scan always `Undetermined` would satisfy the failure cases while destroying
//! the feature, so every "unreadable" assertion is paired with an otherwise
//! identical "readable" assertion that must stay `Known`.

use harness_core::usage::{aggregate, subagent_usage};
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};

/// Lays down `<base>/<stem>.jsonl` plus a `subagents/` dir holding two agent
/// transcripts that each carry one real assistant turn with usage.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "harness-core-subfile-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let stem = "sess";
    let sub_dir = base.join(stem).join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();

    let main_path = base.join(format!("{stem}.jsonl"));
    std::fs::write(&main_path, assistant_line(10, 20)).unwrap();

    std::fs::write(sub_dir.join("agent-aaa111.jsonl"), assistant_line(100, 200)).unwrap();
    std::fs::write(sub_dir.join("agent-bbb222.jsonl"), assistant_line(300, 400)).unwrap();

    (main_path, sub_dir)
}

fn assistant_line(input: u64, output: u64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"2026-09-24T00:00:00.000Z\",\
         \"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5\",\
         \"usage\":{{\"input_tokens\":{input},\"output_tokens\":{output}}}}}}}\n"
    )
}

/// Makes one file unreadable while its directory stays listable, and returns a
/// guard that restores the mode so the temp dir can be cleaned up.
#[cfg(unix)]
fn make_unreadable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
}

#[cfg(unix)]
fn restore(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644));
}

/// Guards against the test itself being vacuous: if the process can still read
/// a mode-000 file (running as root, or a filesystem that ignores the bit),
/// the injection proved nothing and the assertion below would pass for the
/// wrong reason.
#[cfg(unix)]
fn injection_is_effective(path: &Path) -> bool {
    std::fs::read_to_string(path).is_err()
}

#[cfg(unix)]
#[test]
fn one_unreadable_agent_file_makes_the_aggregate_scan_undetermined() {
    let (main_path, sub_dir) = fixture("agg-fail");
    let victim = sub_dir.join("agent-bbb222.jsonl");
    make_unreadable(&victim);

    if !injection_is_effective(&victim) {
        restore(&victim);
        let _ = std::fs::remove_dir_all(main_path.parent().unwrap());
        // Not a silent skip: say why, so a vacuous pass is not read as a pass.
        panic!(
            "fault injection ineffective: {} is still readable at mode 000 \
             (running as root?). This test cannot observe the defect here.",
            victim.display()
        );
    }

    let agg = aggregate(main_path.to_str().unwrap()).expect("main transcript reads");
    let scan = agg.subagent_scan.clone();

    restore(&victim);
    let _ = std::fs::remove_dir_all(main_path.parent().unwrap());

    match scan {
        Determination::Undetermined(_) => {}
        Determination::Known(()) => panic!(
            "a sub-agent transcript that could not be read was dropped silently: \
             subagent_scan is Known, so the under-counted total is indistinguishable \
             from a fully-scanned session"
        ),
    }
}

/// Control for the test above: identical fixture, nothing injected. The scan
/// must stay `Known`, otherwise the fix above is just "always Undetermined".
#[cfg(unix)]
#[test]
fn fully_readable_agent_files_keep_the_aggregate_scan_known() {
    let (main_path, _sub_dir) = fixture("agg-ok");

    let agg = aggregate(main_path.to_str().unwrap()).expect("main transcript reads");
    let scan = agg.subagent_scan.clone();
    let _ = std::fs::remove_dir_all(main_path.parent().unwrap());

    match scan {
        Determination::Known(()) => {}
        Determination::Undetermined(why) => panic!(
            "every sub-agent transcript was readable, yet the scan reported \
             Undetermined({why:?}) — the fix cannot buy fail-closed by refusing \
             to ever succeed"
        ),
    }
}

#[cfg(unix)]
#[test]
fn one_unreadable_agent_file_makes_subagent_usage_undetermined() {
    let (main_path, sub_dir) = fixture("usage-fail");
    let victim = sub_dir.join("agent-bbb222.jsonl");
    make_unreadable(&victim);

    if !injection_is_effective(&victim) {
        restore(&victim);
        let _ = std::fs::remove_dir_all(main_path.parent().unwrap());
        panic!(
            "fault injection ineffective: {} is still readable at mode 000 \
             (running as root?). This test cannot observe the defect here.",
            victim.display()
        );
    }

    let got = subagent_usage(main_path.to_str().unwrap());

    restore(&victim);
    let _ = std::fs::remove_dir_all(main_path.parent().unwrap());

    match got {
        Determination::Undetermined(_) => {}
        Determination::Known(rows) => panic!(
            "subagent_usage skipped an unreadable transcript and returned {} row(s) \
             as if that were the whole picture",
            rows.len()
        ),
    }
}

/// Control for `subagent_usage`: both files readable must still yield both rows.
#[cfg(unix)]
#[test]
fn fully_readable_agent_files_keep_subagent_usage_known() {
    let (main_path, _sub_dir) = fixture("usage-ok");

    let got = subagent_usage(main_path.to_str().unwrap());
    let _ = std::fs::remove_dir_all(main_path.parent().unwrap());

    match got {
        Determination::Known(rows) => assert_eq!(
            rows.len(),
            2,
            "both readable sub-agent transcripts must be reported"
        ),
        Determination::Undetermined(why) => panic!(
            "every sub-agent transcript was readable, yet subagent_usage reported \
             Undetermined({why:?})"
        ),
    }
}
