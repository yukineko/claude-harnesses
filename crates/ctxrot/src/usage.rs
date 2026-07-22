//! Context-usage readout shared by `ctxrot statusline` (the live status bar) and
//! `ctxrot usage` (the skill-facing readout that makes `/distill` usage-aware).
//!
//! Both render the same one-line meter from a single `(percent, tokens)` pair so
//! the number the user sees in the status bar matches the number `/distill`
//! branches on. The percentage source differs: `statusline` trusts Claude's own
//! `context_window.used_percentage` from the hook stdin; `usage` estimates from
//! the transcript (the same `transcript::estimate_tokens` the guard uses).

use std::path::PathBuf;

use crate::config::Config;

/// A proportional band meter like `▮▮▯▯▯`, filled to `frac` of `slots` cells.
fn bar(frac: f64, slots: usize) -> String {
    let filled = ((frac * slots as f64).round() as usize).min(slots);
    let mut s = String::with_capacity(slots * 3);
    for i in 0..slots {
        s.push(if i < filled { '▮' } else { '▯' });
    }
    s
}

/// ANSI color for a band: 0–1 green, 2 yellow, 3+ red. Suppressed under NO_COLOR.
fn color(band: usize) -> (&'static str, &'static str) {
    if std::env::var_os("NO_COLOR").is_some() {
        return ("", "");
    }
    let c = match band {
        0 | 1 => "\x1b[32m",
        2 => "\x1b[33m",
        _ => "\x1b[31m",
    };
    (c, "\x1b[0m")
}

/// Neutral/dim (bright-black) color for an UNVERIFIED low reading. Suppressed
/// under NO_COLOR. A bytes-proxy estimate that lands in the green band is a low
/// number we could not confirm (a corrupt/truncated transcript reads low here),
/// so per doctrine it must NOT paint the confident "healthy green" — it is dimmed
/// to signal "unverified", without being floored to yellow/red (which would
/// over-alarm every pre-usage-block session and get the bar ignored).
fn dim_color() -> (&'static str, &'static str) {
    if std::env::var_os("NO_COLOR").is_some() {
        return ("", "");
    }
    ("\x1b[90m", "\x1b[0m")
}

/// Tokens as a compact `104k` / `512` string.
fn fmt_k(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{}k", (tokens as f64 / 1000.0).round() as u64)
    } else {
        format!("{tokens}")
    }
}

/// The shared one-line readout: `ctxrot 52% ▮▮▯▯▯ band1 ~104k/200k`.
/// `pct` is 0–100; `tokens` adds the absolute `~used/window` suffix when known.
///
/// `estimated` marks a reading that came from the bytes-proxy fallback
/// (`estimate_tokens` source `"bytes"`) rather than a real `usage` block: a rough
/// `size/4` guess that a corrupt/truncated transcript can make read LOW while true
/// usage is high. Such a reading gets an explicit `est?` marker so a low band is
/// never taken as a confident "plenty of headroom" — the caller threads the
/// `estimate_tokens` source instead of discarding it. (It deliberately does NOT
/// force a color: flooring every early, usage-block-less session to yellow/red
/// would over-alarm and get the bar ignored; the marker surfaces the uncertainty
/// while keeping the useful low reading. Authoritative sources — Claude's own %
/// or a real `usage` block — pass `false` and render unmarked.)
pub fn line(cfg: &Config, pct: u64, tokens: Option<u64>, estimated: bool) -> String {
    let frac = pct as f64 / 100.0;
    let band = cfg.band_for(frac);
    let slots = cfg.bands.len() + 1; // +1 for the "below the lowest band" slot

    // An estimate that lands in the GREEN band (0–1) is an UNVERIFIED low reading
    // → dim it (never confident green). A high estimate keeps its alarming color
    // (the conservative direction); an authoritative reading uses its real band
    // color unchanged.
    let (c, r) = if estimated && band <= 1 {
        dim_color()
    } else {
        color(band)
    };
    let tok = match tokens {
        Some(t) => format!(" ~{}/{}", fmt_k(t), fmt_k(cfg.context_window)),
        None => String::new(),
    };
    let est = if estimated { " est?" } else { "" };
    format!(
        "{c}ctxrot {pct}% {} band{band}{est}{r}{tok}",
        bar(frac, slots)
    )
}

/// The readout when context usage is UNKNOWN — the transcript could not be read
/// or estimated (a cannot-determine). It must NEVER render as a green low band or
/// a blank line that reads as "plenty of headroom": that is the statusline
/// fail-open. Instead it gets an explicit, non-green `?% … unknown` state — the
/// visual mirror of `ctxrot usage`'s "不明" readout — in the red band color (via
/// `color`'s `_` arm) so it is distinct from every real band and still honours
/// NO_COLOR. The caller keeps exit 0 so the status bar never crashes; what
/// changes is that "unknown" is SHOWN rather than silently rendered as absence.
pub fn unknown_line(cfg: &Config) -> String {
    let (c, r) = color(usize::MAX); // red arm: unknown is an alarm state, never green
    let slots = cfg.bands.len() + 1;
    format!("{c}ctxrot ?% {} unknown{r}", "▚".repeat(slots))
}

/// A band-keyed action hint for usage-aware `/distill`. Centralizing it here (not
/// in the skill prose) keeps the threshold logic next to `band_for`.
pub fn hint(cfg: &Config, pct: u64) -> &'static str {
    match cfg.band_for(pct as f64 / 100.0) {
        0 => "使用率は低め。distill は急ぎ不要（focus 指定があるときだけ実施）。",
        1 => "中程度。区切りが良ければ distill して要約＋リンク化を。",
        _ => "高い。distill したら、その場で /compact してトークンを実際に解放すること。",
    }
}

/// Locate the Claude Code transcript for a session id without replicating
/// Claude's cwd-mangling: scan every `~/.claude/projects/*/` for `<id>.jsonl`
/// and take the most recently modified match. Returns None if nothing
/// matches, INCLUDING when `projects` cannot be listed at all — that `None`
/// already flows into callers' honest "unknown" readout (`unknown_line` /
/// "context使用量は不明"), so it is not a fail-open. A permission-denied (or
/// other non-NotFound) error on the top-level scan, or on an individual
/// per-project entry, is still surfaced via `eprintln!` so the gap is visible
/// rather than indistinguishable from "no transcripts exist yet".
pub fn find_transcript_for_session(session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let projects = PathBuf::from(home).join(".claude").join("projects");
    let target = format!("{session_id}.jsonl");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = match std::fs::read_dir(&projects) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!(
                "warning: find_transcript_for_session: cannot read {}: {e}",
                projects.display()
            );
            return None;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!(
                    "warning: find_transcript_for_session: unreadable entry in {}: {e}",
                    projects.display()
                );
                continue;
            }
        };
        let p = entry.path().join(&target);
        if let Ok(meta) = std::fs::metadata(&p) {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Percentage (0–100) from raw token count against the configured window.
pub fn pct_from_tokens(cfg: &Config, tokens: u64) -> u64 {
    (tokens as f64 / cfg.context_window as f64 * 100.0).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(bar(0.0, 4), "▯▯▯▯");
        assert_eq!(bar(1.0, 4), "▮▮▮▮");
        assert_eq!(bar(0.5, 4), "▮▮▯▯");
    }

    #[test]
    fn line_has_percent_and_band() {
        std::env::set_var("NO_COLOR", "1");
        let l = line(&cfg(), 52, Some(104_000), false);
        assert!(l.contains("52%"), "{l}");
        assert!(l.contains("band1"), "{l}");
        assert!(l.contains("~104k/200k"), "{l}");
        assert!(
            !l.contains("est?"),
            "an authoritative reading must not be marked estimated: {l}"
        );
    }

    /// A bytes-proxy reading (estimated) carries an explicit `est?` marker so a
    /// low band is never read as a confident "plenty of headroom" — the byte-proxy
    /// fail-open being closed. The same pct rendered from an authoritative source
    /// has no marker, so the two are distinguishable.
    #[test]
    fn estimated_reading_is_marked() {
        std::env::set_var("NO_COLOR", "1");
        let est = line(&cfg(), 3, Some(6_000), true);
        assert!(
            est.contains("est?"),
            "byte-proxy estimate must be marked: {est}"
        );
        let authoritative = line(&cfg(), 3, Some(6_000), false);
        assert!(
            !authoritative.contains("est?"),
            "authoritative reading must be unmarked: {authoritative}"
        );
    }

    #[test]
    fn hint_escalates_with_band() {
        assert!(hint(&cfg(), 10).contains("急ぎ不要"));
        assert!(hint(&cfg(), 60).contains("区切り"));
        assert!(hint(&cfg(), 80).contains("/compact"));
    }

    #[test]
    fn pct_from_tokens_rounds() {
        assert_eq!(pct_from_tokens(&cfg(), 100_000), 50);
        assert_eq!(pct_from_tokens(&cfg(), 150_000), 75);
    }

    /// The unknown readout must be a distinct, labeled, NON-blank state that is
    /// NOT a real band — so it can never be mistaken for a green low-usage bar
    /// (the statusline fail-open being closed). Content-level assertions under
    /// NO_COLOR (race-free vs the ANSI codes).
    #[test]
    fn unknown_line_is_labeled_not_a_real_band() {
        std::env::set_var("NO_COLOR", "1");
        let l = unknown_line(&cfg());
        assert!(l.contains("unknown"), "must be labeled unknown: {l}");
        assert!(l.contains("?%"), "must show ?% not a concrete %: {l}");
        assert!(
            !l.contains("band"),
            "unknown must NOT render as a real band (would read as healthy): {l}"
        );
        assert!(!l.trim().is_empty(), "unknown must never be blank");
    }

    /// A genuine low-usage band DOES say `band0` and never `unknown` — the two
    /// states are content-distinguishable without inspecting color.
    #[test]
    fn real_band_and_unknown_are_distinguishable() {
        std::env::set_var("NO_COLOR", "1");
        let low = line(&cfg(), 3, Some(6_000), false);
        assert!(low.contains("band0") && !low.contains("unknown"), "{low}");
        let unk = unknown_line(&cfg());
        assert!(unk.contains("unknown") && !unk.contains("band0"), "{unk}");
    }

    /// An absent `~/.claude/projects` legitimately contributes no transcript
    /// (`None`, which callers already render as an honest "unknown" state).
    #[test]
    fn find_transcript_absent_projects_dir_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        let result = find_transcript_for_session("nonexistent-session");
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(result, None);
    }

    /// A `~/.claude/projects` that EXISTS but cannot be read must not panic —
    /// it degrades to the same `None` → "unknown" readout, now with a visible
    /// eprintln warning instead of an indistinguishable silent skip.
    #[cfg(unix)]
    #[test]
    fn find_transcript_unreadable_projects_dir_does_not_panic() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join(".claude").join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let mut perms = std::fs::metadata(&projects).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&projects, perms.clone()).unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        let result = std::panic::catch_unwind(|| find_transcript_for_session("some-session"));
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        perms.set_mode(0o755);
        std::fs::set_permissions(&projects, perms).unwrap();
        assert!(result.is_ok(), "must not panic on an unreadable projects dir");
        assert_eq!(result.unwrap(), None);
    }
}
