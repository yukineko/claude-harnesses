//! Aggregate the undetermined-telemetry stream written by
//! `harness_core::undetermined` (backlog 6d493e39).
//!
//! The stream answers "how often do the gates give up, and where". Writing it is
//! deliberately fail-soft — telemetry must never change a verdict. **Reading it
//! is not.** A reader that cannot parse the ledger and reports a smaller total
//! anyway would under-state exactly the quantity the stream exists to measure,
//! and under-stating it reads as good news. So every read resolves to
//! [`Determination`]: a missing ledger is `Known(empty)` (nothing has given up
//! yet, which is a real observation), while an unreadable or unparseable one is
//! `Undetermined`.
//!
//! Aggregation groups by **site** (`crate` + `file:line`) rather than by reason
//! text. Reason strings embed paths, exit codes and error messages, so they are
//! nearly all distinct; grouping them would need a normalizer, and a normalizer
//! that merged two genuinely different give-ups would fabricate a trend. The
//! site is exact, needs no heuristic, and is the thing an auditor opens next.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_core::undetermined::{self, SinkState};
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};

/// One recorded give-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndetRecord {
    pub ts: i64,
    /// The crate the give-up happened in, as attributed by `#[track_caller]`.
    #[serde(rename = "crate")]
    pub krate: String,
    pub file: String,
    pub line: u32,
    pub reason: String,
    /// True on the marker record that says a process hit the per-process cap,
    /// so its later events are missing from this stream.
    #[serde(default)]
    pub capped: bool,
}

impl UndetRecord {
    /// `crate::file:line` — the grouping key.
    pub fn site(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// Which file the metrics are read from, and whether writers are recording.
///
/// These are two different questions and both matter: the ledger can hold a
/// year of history while recording is currently off, and a reader told only the
/// counts would draw the wrong conclusion from a flat tail.
pub fn ledger_path(cwd: &Path) -> PathBuf {
    match std::env::var(undetermined::SINK_ENV) {
        Ok(v) if !v.trim().is_empty() && !v.trim().eq_ignore_ascii_case("off") => {
            PathBuf::from(v.trim())
        }
        _ => undetermined::default_sink_path(cwd),
    }
}

/// Parse a whole ledger. One bad line makes the WHOLE read undetermined rather
/// than yielding the records around it: a partial ledger presented as a total is
/// an under-count, and the caller has no way to know it happened.
pub fn parse(text: &str) -> Determination<Vec<UndetRecord>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<UndetRecord>(line) {
            Ok(r) => out.push(r),
            Err(e) => {
                return Determination::undetermined(format!(
                    "undetermined-telemetry ledger line {} is unparseable ({e}); \
                     refusing to report a partial total, which would under-state \
                     the give-up count the ledger exists to measure",
                    i + 1
                ))
            }
        }
    }
    Determination::Known(out)
}

/// Load and parse the ledger.
///
/// * absent → `Known(vec![])`: nothing has given up, a real observation.
/// * unreadable (permissions, IO error) → `Undetermined`, never an empty list.
/// * unparseable → `Undetermined`, via [`parse`].
///
/// Reads through [`harness_core::boundary::read_to_string`] rather than
/// `std::fs`, which is what draws the absent/unreadable distinction above: its
/// `Determination<Option<String>>` makes "not there" (`Known(None)`) a different
/// value from "could not look" (`Undetermined`), so this function has no shape in
/// which an IO error becomes an empty history. The raw-IO ratchet enforces that
/// in gate crates, and it was right to block the first version of this module,
/// which hand-matched `std::fs`'s `ErrorKind` instead.
pub fn load(path: &Path) -> Determination<Vec<UndetRecord>> {
    match harness_core::boundary::read_to_string(path) {
        Determination::Known(Some(text)) => parse(&text),
        Determination::Known(None) => Determination::Known(Vec::new()),
        // Forwarded, not re-recorded: boundary already counted this give-up.
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// A counted group, ordered for stable output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub key: String,
    pub count: u64,
    /// One verbatim reason from this group, so the output is actionable without
    /// opening the ledger. Not a summary of the group — the reasons within a
    /// site can differ (different errno, different path).
    pub sample_reason: String,
}

/// The aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Metrics {
    /// Records inside the window.
    pub total: u64,
    /// Records excluded by the window (reported, not silently dropped).
    pub outside_window: u64,
    pub window_days: Option<i64>,
    pub by_crate: Vec<Group>,
    pub by_site: Vec<Group>,
    /// How many cap markers are in the window. Each one means some process's
    /// later give-ups are missing, so `total` is a floor, not the true count.
    pub capped_records: u64,
    /// Human-readable: is anything recording right now, and where to.
    pub sink: String,
    /// Where these numbers were read from.
    pub ledger: String,
}

impl Metrics {
    /// True when the stream is known to be incomplete, so `total` is a lower
    /// bound. Callers must not present a floor as a measurement.
    pub fn is_floor(&self) -> bool {
        self.capped_records > 0
    }
}

/// Aggregate records. `now` and `window_days` are passed in rather than read
/// from the clock so this is a pure function and testable.
pub fn aggregate(
    records: &[UndetRecord],
    now: i64,
    window_days: Option<i64>,
    sink: &SinkState,
    ledger: &Path,
) -> Metrics {
    let cutoff = window_days.map(|d| now - d * 86_400);
    let mut by_crate: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut by_site: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut total = 0u64;
    let mut outside = 0u64;
    let mut capped = 0u64;

    for r in records {
        if let Some(c) = cutoff {
            if r.ts < c {
                outside += 1;
                continue;
            }
        }
        total += 1;
        if r.capped {
            capped += 1;
        }
        by_crate
            .entry(r.krate.clone())
            .and_modify(|e| e.0 += 1)
            .or_insert((1, r.reason.clone()));
        by_site
            .entry(r.site())
            .and_modify(|e| e.0 += 1)
            .or_insert((1, r.reason.clone()));
    }

    // Descending by count, then by key, so the output is deterministic and the
    // hottest give-up is first.
    let to_groups = |m: BTreeMap<String, (u64, String)>| {
        let mut v: Vec<Group> = m
            .into_iter()
            .map(|(key, (count, sample_reason))| Group {
                key,
                count,
                sample_reason,
            })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
        v
    };

    Metrics {
        total,
        outside_window: outside,
        window_days,
        by_crate: to_groups(by_crate),
        by_site: to_groups(by_site),
        capped_records: capped,
        sink: sink.describe(),
        ledger: ledger.display().to_string(),
    }
}

/// Render for a terminal.
pub fn render(m: &Metrics) -> String {
    let mut s = String::new();
    let window = match m.window_days {
        Some(d) => format!("last {d} day(s)"),
        None => "all time".to_string(),
    };
    s.push_str(&format!("undetermined give-ups ({window})\n"));
    s.push_str(&format!("  ledger: {}\n", m.ledger));
    // The sink state comes BEFORE the counts on purpose: a zero read without it
    // is uninterpretable, and a reader who stops after the first number must
    // have already seen whether anything was recording.
    s.push_str(&format!("  sink:   {}\n", m.sink));
    s.push_str(&format!("  total:  {}", m.total));
    if m.is_floor() {
        s.push_str(&format!(
            "  (FLOOR, not a count: {} process(es) hit the per-process cap, so \
             later give-ups from them are absent)",
            m.capped_records
        ));
    }
    s.push('\n');
    if m.outside_window > 0 {
        s.push_str(&format!("  excluded by window: {}\n", m.outside_window));
    }
    if m.total == 0 {
        s.push_str(
            "\n  No give-ups in this window. Read the `sink` line above before \
             treating that as good news.\n",
        );
        return s;
    }
    s.push_str("\nby crate:\n");
    for g in &m.by_crate {
        s.push_str(&format!("  {:>6}  {}\n", g.count, g.key));
    }
    s.push_str("\nby site (hottest first):\n");
    for g in m.by_site.iter().take(20) {
        s.push_str(&format!("  {:>6}  {}\n", g.count, g.key));
        s.push_str(&format!("          {}\n", truncate(&g.sample_reason, 120)));
    }
    if m.by_site.len() > 20 {
        // No silent caps: say what was left out.
        s.push_str(&format!(
            "  … {} further site(s) not shown; use --json for all of them\n",
            m.by_site.len() - 20
        ));
    }
    s
}

fn truncate(s: &str, n: usize) -> String {
    let one_line = s.replace('\n', " ");
    if one_line.chars().count() <= n {
        return one_line;
    }
    let cut: String = one_line.chars().take(n).collect();
    format!("{cut}…")
}

/// CLI entry point. Fails (non-zero) rather than printing a zero when the
/// ledger cannot be read — `Determination::require` is the only extractor, so
/// there is no shape of this function that quietly substitutes an empty history.
pub fn run_cli(json: bool, window_days: Option<i64>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let path = ledger_path(&cwd);
    let records = match load(&path).require() {
        Ok(r) => r,
        Err(v) => anyhow::bail!(
            "cannot report undetermined metrics: {}",
            v.reason()
                .map(harness_core::verdict::Reason::as_str)
                .unwrap_or("undetermined with no stated reason")
        ),
    };
    let m = aggregate(
        &records,
        crate::store::now(),
        window_days,
        &undetermined::sink_state(),
        &path,
    );
    if json {
        println!("{}", serde_json::to_string_pretty(&m)?);
    } else {
        print!("{}", render(&m));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: i64, krate: &str, line: u32, reason: &str) -> UndetRecord {
        UndetRecord {
            ts,
            krate: krate.to_string(),
            file: format!("crates/{krate}/src/gate.rs"),
            line,
            reason: reason.to_string(),
            capped: false,
        }
    }

    fn json_of(r: &UndetRecord) -> String {
        serde_json::to_string(r).expect("record must serialize")
    }

    #[test]
    fn an_unparseable_line_is_undetermined_not_a_shorter_history() {
        // The §3 property for this reader. A ledger with one corrupt line must
        // NOT report the records around it as the total: that is an under-count
        // presented as a measurement, in the direction that flatters the repo.
        let good = json_of(&rec(100, "blastguard", 42, "spawn failed"));
        let text = format!("{good}\n{{not json\n{good}\n");
        let d = parse(&text);
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "a corrupt line must poison the whole read, got {d:?}"
        );
        // Anti-vacuity: the same ledger without the corrupt line IS Known(2), so
        // the assertion above is about the corruption and not about parse always
        // failing.
        let clean = parse(&format!("{good}\n{good}\n"));
        assert!(
            matches!(&clean, Determination::Known(v) if v.len() == 2),
            "a clean ledger must parse to both records, got {clean:?}"
        );
    }

    #[test]
    fn a_missing_ledger_is_known_empty_but_an_unreadable_one_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.jsonl");
        let absent = load(&missing);
        assert!(
            matches!(&absent, Determination::Known(v) if v.is_empty()),
            "an absent ledger is a legitimate empty history, got {absent:?}"
        );

        // …whereas one that exists but cannot be read is NOT an empty history.
        let unreadable = dir.path().join("locked.jsonl");
        std::fs::write(&unreadable, json_of(&rec(1, "propguard", 9, "boom"))).expect("seed ledger");
        let mut perms = std::fs::metadata(&unreadable).expect("stat").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o000);
        }
        std::fs::set_permissions(&unreadable, perms).expect("chmod");
        // Observe the precondition WHILE the file is still locked. Checking it
        // after restoring permissions was the first version of this test, and it
        // made the whole case vacuous: the read then always succeeded, the
        // `assert!` never ran, and a mutation that returned `Known(vec![])` on an
        // IO error survived. A test whose assertion is skipped passes for the
        // same reason a fail-open does — nothing looked.
        let blocked = std::fs::read_to_string(&unreadable).is_err();
        let d = load(&unreadable);
        // Restore before asserting, so a failing assert cannot leave an
        // unremovable temp dir behind.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&unreadable).expect("stat").permissions();
            p.set_mode(0o644);
            let _ = std::fs::set_permissions(&unreadable, p);
        }
        // Loud, not skipped: if the environment cannot express "unreadable"
        // (running as root, or a filesystem ignoring the mode) then this case is
        // unmeasurable here, and that fact must surface rather than be recorded
        // as a pass.
        assert!(
            blocked,
            "chmod 000 did not make the ledger unreadable, so the unreadable-vs-absent \
             distinction cannot be observed in this environment (running as root?). \
             This is an unmeasurable case, not a passing one."
        );
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "an unreadable ledger must not read as an empty history, got {d:?}"
        );
    }

    #[test]
    fn the_window_excludes_older_records_and_says_how_many() {
        let now = 1_000_000i64;
        let recs = vec![
            rec(now - 100, "blastguard", 1, "recent"),
            rec(now - 40 * 86_400, "blastguard", 2, "old"),
        ];
        let m = aggregate(
            &recs,
            now,
            Some(7),
            &SinkState::Active(PathBuf::from("/x")),
            Path::new("/x"),
        );
        assert_eq!(m.total, 1);
        // Dropped records are reported, never silently truncated away.
        assert_eq!(m.outside_window, 1);

        let all = aggregate(
            &recs,
            now,
            None,
            &SinkState::Active(PathBuf::from("/x")),
            Path::new("/x"),
        );
        assert_eq!(all.total, 2);
        assert_eq!(all.outside_window, 0);
    }

    #[test]
    fn groups_are_ordered_hottest_first_and_keyed_by_site() {
        let now = 100i64;
        let recs = vec![
            rec(now, "propguard", 10, "a"),
            rec(now, "propguard", 10, "b"),
            rec(now, "propguard", 99, "c"),
            rec(now, "condukt", 5, "d"),
        ];
        let m = aggregate(
            &recs,
            now,
            None,
            &SinkState::Active(PathBuf::from("/x")),
            Path::new("/x"),
        );
        assert_eq!(m.by_crate[0].key, "propguard");
        assert_eq!(m.by_crate[0].count, 3);
        assert_eq!(m.by_site[0].key, "crates/propguard/src/gate.rs:10");
        assert_eq!(m.by_site[0].count, 2);
        // The sample is verbatim from the group, not a synthesized summary.
        assert!(["a", "b"].contains(&m.by_site[0].sample_reason.as_str()));
    }

    #[test]
    fn a_capped_total_is_labelled_a_floor_not_a_count() {
        // A process that hit the cap has give-ups missing from the stream. If the
        // total were printed as a plain number, the aggregate would under-state
        // reality with no indication — the same silence this whole feature
        // removes, reintroduced at the reader.
        let mut capped = rec(10, "condukt", 7, "[CAPPED at 512 …] boom");
        capped.capped = true;
        let m = aggregate(
            &[capped],
            10,
            None,
            &SinkState::Active(PathBuf::from("/x")),
            Path::new("/x"),
        );
        assert_eq!(m.capped_records, 1);
        assert!(m.is_floor());
        assert!(
            render(&m).contains("FLOOR"),
            "the rendering must say the total is a floor: {}",
            render(&m)
        );

        // Anti-vacuity control: an uncapped aggregate must NOT claim to be a
        // floor, or the label would be noise that readers learn to ignore.
        let plain = aggregate(
            &[rec(10, "condukt", 7, "boom")],
            10,
            None,
            &SinkState::Active(PathBuf::from("/x")),
            Path::new("/x"),
        );
        assert!(!plain.is_floor());
        assert!(!render(&plain).contains("FLOOR"));
    }

    #[test]
    fn a_zero_total_is_rendered_with_the_sink_state_not_as_good_news() {
        // The reader-side half of the "0 is ambiguous" problem. An empty result
        // must arrive with the reason recording might not be happening.
        for sink in [
            SinkState::SuppressedUnderCargo,
            SinkState::DisabledByEnv,
            SinkState::Unresolvable("no cwd".into()),
        ] {
            let m = aggregate(&[], 0, Some(7), &sink, Path::new("/x"));
            assert_eq!(m.total, 0);
            let out = render(&m);
            assert!(
                out.contains("Read the `sink` line above"),
                "a zero total must not stand alone: {out}"
            );
            assert!(
                out.contains("says nothing") || out.contains("nothing was recorded"),
                "the sink caveat must reach the rendered output for {sink:?}: {out}"
            );
        }
    }

    #[test]
    fn the_env_override_redirects_the_read_but_off_does_not() {
        // `off` means "do not record"; it is not a ledger path. Reading must fall
        // back to the default location, or `--json` after disabling recording
        // would try to open a file called "off" and report zero.
        let cwd = Path::new("/tmp/whatever");
        // ONE acquisition for the whole test, taken BEFORE the expected value is
        // computed: `default_sink_path` resolves through `$HOME`, so a test in
        // another module that sandboxes `$HOME` between that line and
        // `ledger_path` below would make the two disagree and fail this test for
        // a reason that has nothing to do with the env override it checks.
        // (Re-locking per iteration would deadlock — `Mutex` is not reentrant.)
        let _g = env_lock();
        let default = undetermined::default_sink_path(cwd);
        for (val, want) in [
            (Some("off"), default.clone()),
            (Some("  "), default.clone()),
            (
                Some("/tmp/explicit.jsonl"),
                PathBuf::from("/tmp/explicit.jsonl"),
            ),
        ] {
            let prev = std::env::var_os(undetermined::SINK_ENV);
            match val {
                Some(v) => std::env::set_var(undetermined::SINK_ENV, v),
                None => std::env::remove_var(undetermined::SINK_ENV),
            }
            let got = ledger_path(cwd);
            match prev {
                Some(p) => std::env::set_var(undetermined::SINK_ENV, p),
                None => std::env::remove_var(undetermined::SINK_ENV),
            }
            assert_eq!(got, want, "for {val:?}");
        }
    }

    /// The process-global env lock — deliberately the CRATE-WIDE
    /// [`crate::store::HOME_ENV_LOCK`], not a module-local mutex.
    ///
    /// These tests read a `$HOME`-derived path, and `$HOME` is process-global:
    /// a module-local mutex would serialize this module against itself while
    /// leaving it racing every `$HOME`-sandboxing test in `store`, `aggregate`,
    /// `canary_cli` and `audit_round_cli` — all of which already share the one
    /// crate-wide lock for exactly this reason (see the note on
    /// `store::HOME_ENV_LOCK`). Observed: with a module-local lock this module's
    /// override test failed roughly 1 run in 5 once `store`'s ledger tests
    /// started sandboxing `$HOME` more often.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::store::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}
