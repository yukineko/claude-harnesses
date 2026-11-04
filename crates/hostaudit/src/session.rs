//! The two remaining canon §1 consumers: the `daily` task stanza and the
//! SessionStart one-line systemMessage.

use crate::config::Ctx;
use harness_core::verdict::Required;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The running executable: absolute and an existing file.
fn current_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot resolve the hostaudit executable path: {e}"))?;
    if !exe.is_absolute() {
        return Err(format!(
            "the hostaudit executable path {} is not absolute",
            exe.display()
        ));
    }
    if !exe.is_file() {
        return Err(format!(
            "the hostaudit executable path {} is not an existing file",
            exe.display()
        ));
    }
    Ok(exe)
}

/// (Re)create the stable `$HOME/.hostaudit/bin/hostaudit` symlink to the
/// running executable (canon v1.1 §9 item 13, CONTRACT §12.11 item 3) and
/// return its path. Creates `~/.hostaudit/bin`, replaces a missing, stale or
/// dangling symlink (atomically, via rename), refuses to replace anything
/// that is not a symlink, and checks that the link resolves to the running
/// executable. `Err` on any failure.
pub fn refresh_stable_link() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or("HOME is not set; cannot place ~/.hostaudit/bin/hostaudit")?;
    if !home.is_absolute() {
        return Err(format!("HOME {} is not absolute", home.display()));
    }
    let exe = current_exe()?;
    let dir = home.join(".hostaudit").join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let link = dir.join("hostaudit");
    match std::fs::symlink_metadata(&link) {
        Ok(m) if !m.file_type().is_symlink() => {
            return Err(format!(
                "{} exists and is not a symlink; refusing to replace it",
                link.display()
            ))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect {}: {e}", link.display())),
    }
    let tmp = dir.join(format!(".hostaudit.tmp-{}", std::process::id()));
    // A leftover temp link from a crashed run with the same pid.
    if std::fs::symlink_metadata(&tmp).is_ok() {
        std::fs::remove_file(&tmp)
            .map_err(|e| format!("cannot remove stale {}: {e}", tmp.display()))?;
    }
    std::os::unix::fs::symlink(&exe, &tmp)
        .map_err(|e| format!("cannot create symlink {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &link) {
        // Best-effort cleanup; the error that matters is the rename's.
        if let Err(c) = std::fs::remove_file(&tmp) {
            return Err(format!(
                "cannot move symlink into {}: {e} (and {} was left behind: {c})",
                link.display(),
                tmp.display()
            ));
        }
        return Err(format!("cannot move symlink into {}: {e}", link.display()));
    }
    let (a, b) = (std::fs::canonicalize(&link), std::fs::canonicalize(&exe));
    match (a, b) {
        (Ok(a), Ok(b)) if a == b => Ok(link),
        (a, b) => Err(format!(
            "symlink {} does not resolve to the running executable ({a:?} vs {b:?})",
            link.display()
        )),
    }
}

/// `path` as a shell word for `sh -c`: single-quoted only when it contains a
/// character outside `[A-Za-z0-9/._+@:,=-]`. `Err` for a non-UTF-8 path or a
/// control character.
fn shell_word(path: &Path) -> Result<String, String> {
    let s = path
        .to_str()
        .ok_or_else(|| format!("the path {} is not UTF-8", path.display()))?;
    if s.chars().any(char::is_control) {
        return Err(format!("the path {s:?} contains a control character"));
    }
    let plain = s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '/' | '.' | '_' | '+' | '@' | ':' | ',' | '=' | '-')
    });
    Ok(if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    })
}

/// A TOML basic string (`"…"`) holding `s`.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `hostaudit daily-task --print` (CONTRACT §10, §12.11). Never includes
/// `aws apply`. Refreshes the stable symlink first; every task command
/// starts with its absolute path `$HOME/.hostaudit/bin/hostaudit`. `Err`
/// when the symlink cannot be provided (nothing is printed then).
pub fn daily_stanza() -> Result<String, String> {
    let exe = shell_word(&refresh_stable_link()?)?;
    let task = |name: &str, verb: &str| {
        format!(
            "[[task]]\nname = {}\ncommand = {}\n",
            toml_str(name),
            toml_str(&format!("{exe} {verb}"))
        )
    };
    Ok(format!(
        "# hostaudit — append to ~/.daily/config.toml (runs once a day; read-only audits).\n\
         # The commands use the stable path ~/.hostaudit/bin/hostaudit (daily's task shell\n\
         # does not have plugin bin dirs on its PATH). It is a symlink to the current\n\
         # hostaudit binary that this command and the SessionStart hook re-point on every\n\
         # run, so this stanza survives plugin upgrades. If the link target is gone the\n\
         # task fails with exit 127 in daily.\n\
         # A non-zero exit means a violation or an undetermined check — see\n\
         # ~/.hostaudit/reports.jsonl or `overwatch review-queue`. The last output line\n\
         # counts violation / undetermined / warning / clean separately.\n\
         \n\
         {}\n{}\n{}",
        task("hostaudit-audit", "audit"),
        task("hostaudit-local", "local"),
        task("hostaudit-aws", "aws")
    ))
}

/// The SessionStart line. Never silent: no report and an unreadable report
/// both say the state is UNKNOWN.
pub fn session_message(ctx: &Ctx) -> String {
    let p = ctx.state_dir().join("reports.jsonl");
    let text = match harness_core::boundary::read_to_string(&p).require() {
        Required::Determined(Some(t)) => t,
        Required::Determined(None) => {
            return "hostaudit: no audit recorded yet — server/AWS/local state UNKNOWN (run `hostaudit audit` / `local` / `aws`, or register `hostaudit daily-task --print`)".to_string()
        }
        Required::Blocked(why) => {
            return format!("hostaudit: reports.jsonl unreadable — state UNKNOWN ({why})")
        }
    };
    // Latest row per finding id.
    let mut latest: BTreeMap<String, (i64, String)> = BTreeMap::new();
    let mut bad = 0usize;
    let mut newest = i64::MIN;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            bad += 1;
            continue;
        };
        let (Some(id), Some(verdict), Some(time)) =
            (v["id"].as_str(), v["verdict"].as_str(), v["time"].as_i64())
        else {
            bad += 1;
            continue;
        };
        newest = newest.max(time);
        let e = latest
            .entry(id.to_string())
            .or_insert((i64::MIN, String::new()));
        if time >= e.0 {
            *e = (time, verdict.to_string());
        }
    }
    let viol = latest.values().filter(|(_, v)| v == "violation").count();
    let warn = latest.values().filter(|(_, v)| v == "warning").count();
    // Any verdict string other than the four known ones counts as undetermined.
    let und = latest
        .values()
        .filter(|(_, v)| {
            !matches!(
                v.as_str(),
                "violation" | "warning" | "clean" | "not_applicable"
            )
        })
        .count();
    let when = chrono::DateTime::from_timestamp(newest, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown time".to_string());
    // canon v1.1 §1: warnings are counted separately, never folded into clean.
    let mut msg = format!(
        "hostaudit: {viol} violation / {und} undetermined / {warn} warning (latest per finding; newest report {when})"
    );
    if latest.is_empty() {
        msg = "hostaudit: reports.jsonl has no readable rows — state UNKNOWN".to_string();
    }
    if bad > 0 {
        msg.push_str(&format!(
            "; {bad} unreadable report rows — counts may be incomplete"
        ));
    }
    if viol + und > 0 {
        msg.push_str(" — details: ~/.hostaudit/reports.jsonl / `overwatch review-queue`");
    }
    msg
}
