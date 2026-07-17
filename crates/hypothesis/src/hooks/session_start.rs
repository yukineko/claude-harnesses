use crate::config::Config;
use crate::goal_link::check_goal_link;
use crate::hypothesis::Hypothesis;
use crate::store::Store;
use harness_core::hook::{read_stdin, HookInput};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// SessionStart hook entry point. Loads config and stdin itself so main can
/// hand off to `run_hook` without capturing any state.
pub fn run() -> Option<String> {
    if Config::disabled_env() {
        return None;
    }
    let cfg = Config::load().ok()?;
    if !cfg.enabled {
        return None;
    }
    let raw = read_stdin();
    let input = HookInput::parse(&raw).unwrap_or_default();
    let repo_root = input.cwd_or_current();
    run_with(&cfg, &repo_root)
}

/// Testable core: takes an explicit config and repo root.
pub(crate) fn run_with(cfg: &Config, repo_root: &Path) -> Option<String> {
    let store = Store::load(cfg).ok()?;
    let all = store.all();

    let open: Vec<_> = all.iter().filter(|h| h.status.is_open()).collect();
    // Awaiting-measurement hypotheses have shipped but still need a human to
    // measure and run validate/reject — surface them so they don't get lost.
    let awaiting: Vec<_> = all
        .iter()
        .filter(|h| h.status.is_awaiting_measurement())
        .collect();
    if open.is_empty() && awaiting.is_empty() {
        return None;
    }

    let unlinked_ids = check_goal_link(all, repo_root);

    let mut out = String::from("## Hypothesis \u{2014} open hypotheses for this project\n\n");

    for h in &open {
        let link_marker = if unlinked_ids.contains(&h.id) {
            " [unlinked]"
        } else {
            ""
        };
        out.push_str(&format!("- **[{}]**{} {}\n", h.id, link_marker, h.text));
        if let Some(goal) = &h.linked_goal {
            out.push_str(&format!("  linked_goal: {}\n", goal));
        }
    }

    // Aggregate measurement DEBT: shipped-but-unmeasured hypotheses rot silently
    // if only listed per-item. Surface a prominent summary (count + oldest age in
    // days) so the debt is salient. Fail-soft: unparseable timestamps are skipped.
    // Once the oldest item ages past AGING_THRESHOLD_DAYS, switch to an explicit
    // warning-level rendering so stale measurement debt doesn't silently rot.
    if let Some((count, oldest)) = awaiting_debt(&awaiting, now_epoch_days()) {
        out.push_str(&awaiting_debt_summary_line(count, oldest));
    }

    for h in &awaiting {
        out.push_str(&format!(
            "- **[{}]** [awaiting-measurement] {}\n",
            h.id, h.text
        ));
        if let Some(goal) = &h.linked_goal {
            out.push_str(&format!("  linked_goal: {}\n", goal));
        }
    }

    out.push_str("\n---\n\n");
    out.push_str("To validate: `hypothesis validate <id> --evidence \"...\"`\n");
    out.push_str("To reject:   `hypothesis reject <id> --reason \"...\"`\n");

    let unlinked_open = open.iter().filter(|h| unlinked_ids.contains(&h.id)).count();
    if unlinked_open > 0 {
        out.push_str(&format!(
            "\n\u{26a0}\u{fe0f} {} 件の仮説が compass charter とリンクしていません。\
             `hypothesis add ... --goal \"<keyword>\"` でリンクしてください。\n",
            unlinked_open
        ));
    }

    if out.len() > cfg.inject_limit {
        let truncated = truncate_to_byte_boundary(&out, cfg.inject_limit);
        out = format!("{}\n*(truncated)*", truncated);
    }

    Some(out)
}

fn truncate_to_byte_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Days since the Unix epoch (1970-01-01) from the real clock. Day granularity
/// is intentional — measurement debt is tracked in whole days.
fn now_epoch_days() -> i64 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400) as i64
}

/// Proleptic Gregorian leap-year test — mirrors `hypothesis::is_leap_year`.
fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Parses the leading `YYYY-MM-DD` of an ISO8601 string into days-since-epoch,
/// mirroring `hypothesis.rs`'s forward (days -> date) conversion in reverse.
/// The time/zone portion is ignored (day granularity is intended). Returns
/// `None` on malformed input so the caller can fail soft (never panic).
fn iso_date_to_epoch_days(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut days: i64 = 0;
    let mut y = 1970i64;
    while y < year {
        days += if is_leap_year(y) { 366 } else { 365 };
        y += 1;
    }
    let leap = is_leap_year(year);
    let days_in_month = [
        31i64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for dim in days_in_month.iter().take((month - 1) as usize) {
        days += *dim;
    }
    days += (day - 1) as i64;
    Some(days)
}

/// Aggregate measurement-debt summary over the awaiting-measurement slice.
/// Returns `(count, oldest_age_days)` where age is `now_days - min(updated_days)`
/// clamped to `>= 0`, or `None` if the slice is empty. Fail-soft: items whose
/// `updated_at` cannot be parsed contribute no age (skipped in the min); if ALL
/// fail to parse, the count is still shown with `oldest = 0`.
fn awaiting_debt(awaiting: &[&Hypothesis], now_days: i64) -> Option<(usize, i64)> {
    if awaiting.is_empty() {
        return None;
    }
    let count = awaiting.len();
    let oldest = awaiting
        .iter()
        .filter_map(|h| iso_date_to_epoch_days(&h.updated_at))
        .min()
        .map(|min_updated| (now_days - min_updated).max(0))
        .unwrap_or(0);
    Some((count, oldest))
}

/// Aging threshold (in days) past which measurement debt is considered stale
/// enough to warrant an explicit warning-level rendering rather than the
/// routine debt summary. Chosen as a reasonable default for "measurement is
/// overdue" (roughly two weeks).
const AGING_THRESHOLD_DAYS: i64 = 14;

/// Renders the measurement-debt summary line for injection into the hook
/// output. Below `AGING_THRESHOLD_DAYS` this is the routine summary line;
/// at or above the threshold it switches to an explicit warning-level
/// format (leading warning glyph + literal "WARNING") so aging debt is
/// salient rather than silently blending into routine output.
fn awaiting_debt_summary_line(count: usize, oldest: i64) -> String {
    if oldest >= AGING_THRESHOLD_DAYS {
        format!(
            "\n**\u{26a0}\u{fe0f} WARNING: \u{8a08}\u{6e2c}\u{8ca0}\u{50b5} (measurement debt) \u{304c}\u{9577}\u{671f}\u{5316}: {} \u{4ef6}** \u{2014} \u{6700}\u{53e4} {} \u{65e5}\u{7d4c}\u{904e} (\u{95be}\u{5024} {} \u{65e5}\u{8d85}\u{904e}\u{30fb}\u{51fa}\u{8377}\u{6e08}\u{307f}\u{30fb}\u{672a}\u{691c}\u{8a3c})\n",
            count, oldest, AGING_THRESHOLD_DAYS
        )
    } else {
        format!(
            "\n**\u{8a08}\u{6e2c}\u{8ca0}\u{50b5} (measurement debt): {} \u{4ef6}** \u{2014} \u{6700}\u{53e4} {} \u{65e5}\u{7d4c}\u{904e} (\u{51fa}\u{8377}\u{6e08}\u{307f}\u{30fb}\u{672a}\u{691c}\u{8a3c})\n",
            count, oldest
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;
    use tempfile::TempDir;

    fn test_cfg(dir: &TempDir) -> Config {
        Config {
            enabled: true,
            store_dir: dir.path().to_path_buf(),
            inject_limit: 2000,
        }
    }

    #[test]
    fn session_hook_no_hypotheses_returns_none() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);
        let result = run_with(&cfg, dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn session_hook_open_hypothesis_appears_in_output() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        st.add("users want faster onboarding".to_string(), None)
            .unwrap();

        let out = run_with(&cfg, dir.path()).expect("should produce output");
        assert!(out.contains("users want faster onboarding"));
        assert!(out.contains("## Hypothesis"));
    }

    #[test]
    fn session_hook_awaiting_measurement_hypothesis_appears() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st
            .add("shipped, needs measuring".to_string(), None)
            .unwrap();
        st.mark_awaiting_measurement(&id, Some("run-1".to_string()))
            .unwrap();

        let out = run_with(&cfg, dir.path()).expect("should produce output");
        assert!(out.contains("shipped, needs measuring"));
        assert!(out.contains("[awaiting-measurement]"));
    }

    #[test]
    fn session_hook_validated_hypothesis_not_shown() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st.add("already proven".to_string(), None).unwrap();
        st.validate(&id, vec!["measured".to_string()], None)
            .unwrap();

        let result = run_with(&cfg, dir.path());
        // All open hypotheses gone → None
        assert!(result.is_none());
    }

    #[test]
    fn session_hook_unlinked_marker_shown_when_no_charter() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        st.add("no charter present".to_string(), None).unwrap();

        // No .compass/charter.md → hypothesis treated as unlinked
        let out = run_with(&cfg, dir.path()).expect("output");
        assert!(out.contains("[unlinked]"));
    }

    #[test]
    fn session_hook_linked_hypothesis_no_unlinked_marker() {
        use std::fs;
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        // Write charter so goal link can match
        let compass = dir.path().join(".compass");
        fs::create_dir_all(&compass).unwrap();
        fs::write(
            compass.join("charter.md"),
            "## north_star\nfaster user onboarding\n\n## definition_of_done\n- all tests pass\n",
        )
        .unwrap();

        let mut st = Store::load(&cfg).unwrap();
        st.add(
            "users want faster onboarding".to_string(),
            Some("faster user onboarding".to_string()),
        )
        .unwrap();

        let out = run_with(&cfg, dir.path()).expect("output");
        assert!(!out.contains("[unlinked]"));
    }

    #[test]
    fn awaiting_debt_summary_counts_and_ages() {
        // Two awaiting-measurement hypotheses with fixed past `updated_at` dates.
        // 2026-06-16 -> epoch day 20620, 2026-06-01 -> epoch day 20605.
        let mut a = Hypothesis::new("shipped A", None);
        a.updated_at = "2026-06-16T09:30:00Z".to_string();
        let mut b = Hypothesis::new("shipped B", None);
        b.updated_at = "2026-06-01T00:00:00Z".to_string();
        let awaiting: Vec<&Hypothesis> = vec![&a, &b];

        // Fixed now = 2026-07-16 -> epoch day 20650.
        let now_days = 20650;
        let (count, oldest) = awaiting_debt(&awaiting, now_days).expect("non-empty slice");
        assert_eq!(count, 2);
        // Oldest item is 2026-06-01 (20605); 20650 - 20605 = 45 days.
        assert_eq!(oldest, 45);

        // Empty slice → no debt.
        assert_eq!(awaiting_debt(&[], now_days), None);

        // Reverse epoch-day conversion, independently computed.
        assert_eq!(iso_date_to_epoch_days("2026-06-26T13:00:00Z"), Some(20630));
        // Malformed input fails soft.
        assert_eq!(iso_date_to_epoch_days("not-a-date"), None);

        // All-unparseable timestamps still yield the count, oldest = 0.
        let mut c = Hypothesis::new("bad ts", None);
        c.updated_at = "garbage".to_string();
        assert_eq!(awaiting_debt(&[&c], now_days), Some((1, 0)));
    }

    #[test]
    fn awaiting_debt_summary_line_below_threshold_is_routine() {
        // oldest = 13 days < AGING_THRESHOLD_DAYS (14) → routine wording, no warning.
        let line = awaiting_debt_summary_line(2, 13);
        assert!(line.contains("\u{8a08}\u{6e2c}\u{8ca0}\u{50b5}")); // 計測負債
        assert!(!line.contains("WARNING"));
        assert!(!line.contains('\u{26a0}')); // no warning glyph
    }

    #[test]
    fn awaiting_debt_summary_line_at_or_above_threshold_is_warning() {
        // oldest = 14 days == AGING_THRESHOLD_DAYS → explicit warning-level wording.
        let line = awaiting_debt_summary_line(2, 14);
        assert!(line.contains("WARNING"));
        assert!(line.contains('\u{26a0}')); // warning glyph present

        // Well past the threshold too.
        let line_far = awaiting_debt_summary_line(1, 45);
        assert!(line_far.contains("WARNING"));
    }

    #[test]
    fn session_hook_renders_measurement_debt_summary() {
        let dir = TempDir::new().unwrap();
        let cfg = test_cfg(&dir);

        let mut st = Store::load(&cfg).unwrap();
        let id = st
            .add("shipped, needs measuring".to_string(), None)
            .unwrap();
        st.mark_awaiting_measurement(&id, Some("run-1".to_string()))
            .unwrap();

        let out = run_with(&cfg, dir.path()).expect("output");
        assert!(out.contains("\u{8a08}\u{6e2c}\u{8ca0}\u{50b5}")); // 計測負債
        assert!(out.contains("\u{4ef6}")); // 件
    }

    #[test]
    fn session_hook_truncates_at_inject_limit() {
        let dir = TempDir::new().unwrap();
        let mut cfg = test_cfg(&dir);
        cfg.inject_limit = 60;

        let mut st = Store::load(&cfg).unwrap();
        st.add("a".repeat(200), None).unwrap();

        let out = run_with(&cfg, dir.path()).expect("output");
        assert!(out.contains("*(truncated)*"));
    }
}
