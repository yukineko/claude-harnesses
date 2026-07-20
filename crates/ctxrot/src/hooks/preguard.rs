//! `ctxrot preguard` — PreToolUse hook (preventive gate).
//!
//! Where `toolguard` (PostToolUse) nudges *after* a huge payload already landed
//! in context, this fires *before* the load and can actually stop it. It targets
//! the one rot vector we can measure ahead of time: a `Read` of a pathologically
//! large LOCAL file with no `limit`. Those are almost always logs / data dumps /
//! minified blobs — exactly what should never enter main context whole.
//!
//! Policy (deliberately narrow to avoid false positives on normal source files):
//!   * Only `Read`.                     (URLs can't be sized before fetch.)
//!   * Only when `limit` is absent.     (An explicit slice = the model is careful.)
//!   * Only at/above `gate_file_bytes`  (default 1MB; 0 disables the gate).
//!
//! Everything else defers to the normal permission flow (we stay silent).
//!
//! PreToolUse cannot inject additionalContext, so the deny *reason* is the only
//! steering channel — we make it actionable (route via sub-agent, or re-Read a
//! bounded slice).
//!
//! Bash gate (opt-in, `gate_bash`): `Read` byte-sizing has no Bash analogue —
//! output size is unknown before the command runs — so we deny only commands that
//! are *obviously* unbounded dumps by their shape (`cat huge.log`, `journalctl`
//! with no `-n`/`--since`, recursive `grep` with no `-m`, full `tail -n +1`, …)
//! AND carry no downstream bound (`| head`, `| wc`, `| sed -n`, `-m N`, …).
//! Deliberately conservative: when in doubt, allow.

use regex::Regex;

use crate::config::Config;
use harness_core::hook::HookInput;

/// Reason returned when the analyser itself panics.
///
/// "Never break the turn" and "never block the call" are NOT the same
/// contract (see `blastguard::main` for the sibling fix). `main`'s outer
/// `run_hook` already catches any panic that escapes this hook and turns it
/// into a silent `exit 0` — but for a PreToolUse gate, a silent exit 0 IS an
/// allow. So a panic anywhere in `check_read`/`check_bash` (glob matching,
/// regex, filesystem metadata) used to silently ALLOW the very Read/Bash call
/// it had failed to analyse — exactly the huge-unexamined-dump case this hook
/// exists to stop. `analyse` below catches that panic and turns it into a
/// deny instead, which is a normal, non-breaking outcome.
pub const INTERNAL_ERROR_REASON: &str =
    "[context-rot guard] 内部エラーで解析に失敗したため、安全側に倒してこの呼び出しを拒否しました \
     （原因不明のまま通すと巨大な読み込みが未検証で context に流れ込む恐れがあるため）。 \
     Explore か general-purpose sub-agent 経由で読み直すか、limit を付けて再試行してください。";

/// Run [`run`] behind a panic barrier: a panic in the detector becomes a deny
/// instead of falling through to `main`'s outer `run_hook` backstop, which
/// would otherwise turn it into a silent allow (see `INTERNAL_ERROR_REASON`).
/// Mirrors blastguard's `analyse`. This is the entry point `main` calls.
pub fn analyse(input: &HookInput, cfg: &Config) -> Option<String> {
    analyse_barrier(std::panic::AssertUnwindSafe(|| run(input, cfg)))
}

/// Testable core of `analyse`: the panic barrier over an injected closure, so
/// tests can force the panic path without needing a real panic-triggering
/// input through `run`'s glob/regex/filesystem logic.
fn analyse_barrier<F>(f: F) -> Option<String>
where
    F: FnOnce() -> Option<String> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(v) => v,
        Err(_) => Some(INTERNAL_ERROR_REASON.to_string()),
    }
}

/// Returns a deny reason (the model is told to reroute), or None to stay silent
/// and let the normal permission flow proceed.
pub fn run(input: &HookInput, cfg: &Config) -> Option<String> {
    match input.tool_name.as_str() {
        "Read" => check_read(input, cfg),
        "Bash" if cfg.gate_bash => check_bash(input, cfg),
        _ => None,
    }
}

fn check_read(input: &HookInput, cfg: &Config) -> Option<String> {
    let ti = input.tool_input.as_ref()?;
    let raw_path = ti.get("file_path").and_then(|v| v.as_str())?;
    let has_limit = ti.get("limit").is_some();

    let expanded = crate::config::expand_tilde(raw_path);
    let cwd = input.cwd_or_current();
    let path = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(&expanded)
    };

    // Rule-based DENY (feature ①) — wins over everything, independent of size.
    // A deny rule means "never load this into main context"; by default it holds
    // even for a bounded `limit` slice (load_deny_even_with_limit), so secrets /
    // vendored dirs / dumps stay out entirely. Fires even when the size gate is
    // disabled (gate_file_bytes=0) and even if the file can't be sized.
    if !cfg.load_deny.is_empty()
        && (cfg.load_deny_even_with_limit || !has_limit)
        && crate::glob::any_match(&cfg.load_deny, raw_path, &path, Some(&cwd))
    {
        crate::metrics::emit(
            cfg,
            &input.session_id,
            "gate",
            serde_json::json!({
                "tool": "Read",
                "file": path.to_string_lossy(),
                "rule": "deny",
                "decision": "deny",
            }),
        );
        return Some(format!(
            "[context-rot guard] `{raw_path}` は load_deny ルールに一致するため main context への \
             読み込みを拒否しました（設定で「絶対に載せない」と指定されたパスです）。 \
             どうしても中身が要るなら Explore か general-purpose sub-agent に読ませて要約・該当行・ \
             結論だけ受け取ってください。誤検知なら ~/.ctxrot/config.toml の load_deny から外せます。"
        ));
    }

    // An explicit `limit` means the model is already bounding the read — never
    // size-gate those (reading 50 lines out of a 2MB file is fine).
    if has_limit {
        return None;
    }

    // Rule-based ALLOW (feature ①): an explicitly-trusted path bypasses the size
    // gate, so a large-but-wanted file (e.g. a key design doc) loads whole.
    if !cfg.load_allow.is_empty()
        && crate::glob::any_match(&cfg.load_allow, raw_path, &path, Some(&cwd))
    {
        return None;
    }

    if cfg.gate_file_bytes == 0 {
        return None;
    }

    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() < cfg.gate_file_bytes {
        return None;
    }

    crate::metrics::emit(
        cfg,
        &input.session_id,
        "gate",
        serde_json::json!({
            "tool": "Read",
            "file": path.to_string_lossy(),
            "file_bytes": meta.len(),
            "decision": "deny",
        }),
    );

    let mb = meta.len() as f64 / 1_048_576.0;
    let tok = meta.len() / 4;
    Some(format!(
        "[context-rot guard] このファイルは ~{mb:.1}MB（推定~{tok}tok）で、全文を main context に \
         載せると rot の原因になります。次のいずれかで読み直してください: \
         (1) Explore か general-purpose sub-agent に読ませ、該当行・要約・結論だけ受け取る \
         (2) 本当に直接読むなら Read に offset/limit を付けて必要な範囲だけに絞る。 \
         （巨大な生データを丸ごと本文に入れない方針です）"
    ))
}

// --------------------------------------------------------------- Bash gate (P2)

/// A downstream/inline bound that caps how much reaches context: `head`, `wc`,
/// `sed -n`, a max-count (`-m N` / `--max-count`), or a *counted* `tail -[nc] N`
/// (NOT `tail -n +K`, which is a full dump — see `is_full_tail`).
fn has_bound(cmd: &str) -> bool {
    fn re(p: &str) -> Regex {
        Regex::new(p).expect("static regex")
    }
    re(r"\bhead\b").is_match(cmd)
        || re(r"\bwc\b").is_match(cmd)
        || cmd.contains("sed -n")
        || re(r"(?:^|\s)(?:-m\s*\d|--max-count)").is_match(cmd)
        || re(r"\btail\b[^|;&]*-[nc]\s*\d").is_match(cmd)
}

/// `tail -n +K` / `tail -c +K`: streams from an offset to EOF — an unbounded dump.
fn is_full_tail(cmd: &str) -> bool {
    Regex::new(r"\btail\b[^|;&]*-[nc]\s*\+")
        .expect("static regex")
        .is_match(cmd)
}

/// Output is redirected to a file (`> f`, `>> f`, `>f`) or backgrounded (trailing
/// `&`) — it never enters context, so never gate it. `2>&1` is stderr-dup, not a
/// file redirect, so its token (`2>&1`) is intentionally not matched here.
fn redirects_out(cmd: &str) -> bool {
    cmd.split_whitespace()
        .any(|t| t == ">" || t == ">>" || (t.starts_with('>') && t.len() > 1) || t == "&")
}

/// The kind of unbounded-dump pattern in `cmd`, or None if it looks safe/bounded.
/// Order doesn't matter; the first match wins for the message label.
fn unbounded_dump_kind(cmd: &str) -> Option<&'static str> {
    fn re(p: &str) -> Regex {
        Regex::new(p).expect("static regex")
    }

    // `tail -n +1` is a dump regardless of other bounds preceding it.
    if is_full_tail(cmd) {
        return Some("tail");
    }

    let bounded = has_bound(cmd);

    // cat/bat/less/more reading a file (has an argument), with nothing capping it.
    if re(r"\b(?:cat|bat|less|more)\s+[^|>&;]").is_match(cmd) && !bounded {
        return Some("cat");
    }
    // journalctl with no line/since bound and no downstream bound.
    if cmd.contains("journalctl")
        && !re(r"(?:^|\s)(?:-n\b|-n\s*\d|--lines|--since)").is_match(cmd)
        && !bounded
    {
        return Some("journalctl");
    }
    // dmesg with no `-n` and no downstream bound.
    if cmd.contains("dmesg") && !re(r"(?:^|\s)-n\b").is_match(cmd) && !bounded {
        return Some("dmesg");
    }
    // recursive grep (`-r`/`-R` in a flag cluster) or ripgrep (recursive by
    // default), with no max-count and no downstream bound.
    let recursive_grep = re(r"\b(?:grep|egrep|fgrep)\b[^|;&]*\s-[A-Za-z]*[rR]").is_match(cmd)
        || re(r"\brg\b").is_match(cmd);
    if recursive_grep && !bounded {
        return Some("grep");
    }
    None
}

fn check_bash(input: &HookInput, cfg: &Config) -> Option<String> {
    let cmd = input
        .tool_input
        .as_ref()?
        .get("command")
        .and_then(|v| v.as_str())?;

    // Output that won't enter context (file redirect / background) is never gated.
    if redirects_out(cmd) {
        return None;
    }
    let kind = unbounded_dump_kind(cmd)?;

    // Log only a short excerpt — never the full command.
    let excerpt = harness_core::transcript::truncate_chars(cmd, 120);
    crate::metrics::emit(
        cfg,
        &input.session_id,
        "gate",
        serde_json::json!({
            "tool": "Bash",
            "kind": kind,
            "command_excerpt": excerpt,
            "decision": "deny",
        }),
    );

    Some(format!(
        "[context-rot guard] このコマンド（{kind} 系）は出力量が無制限で、全文が main context に \
         流れ込むと rot の原因になります。次のいずれかにしてください: \
         (1) Explore か general-purpose sub-agent に実行・要約させ、結論だけ受け取る \
         (2) `| head -n N` / `| tail -n N` / `| sed -n` / `-m N` / `| wc` などで出力を有界化する \
         (3) どうしても全量が要るならファイルへリダイレクト（`> out.txt`）して context には載せない。 \
         （opt-in の Bash ゲートが発火しました。誤検知なら config の gate_bash=false で無効化できます）"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn big_temp_file(name: &str, bytes: usize) -> std::path::PathBuf {
        // Place the file in a unique dir created via atomic `mkdtemp` (no
        // pid-collision TOCTOU); `.keep()` returns the dir path.
        let dir = tempfile::Builder::new()
            .prefix("ctxrot-preguard-")
            .tempdir()
            .expect("tempdir")
            .keep();
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&vec![b'x'; bytes]).unwrap();
        p
    }

    fn read_input(ti: serde_json::Value) -> HookInput {
        HookInput {
            tool_name: "Read".into(),
            tool_input: Some(ti),
            ..Default::default()
        }
    }

    #[test]
    fn denies_huge_unbounded_read() {
        let cfg = Config::default();
        let p = big_temp_file("huge.log", 1_200_000);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(out.unwrap().contains("sub-agent"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn allows_when_limit_is_set() {
        let cfg = Config::default();
        let p = big_temp_file("huge2.log", 1_200_000);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy(), "limit": 50 })),
            &cfg,
        );
        assert!(out.is_none(), "an explicit limit must bypass the gate");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn allows_normal_sized_file() {
        let cfg = Config::default();
        let p = big_temp_file("small.rs", 60_000); // > large_file_bytes but << gate
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(out.is_none(), "a 60KB source file must not be gated");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn gate_disabled_when_zero() {
        let cfg = Config {
            gate_file_bytes: 0,
            ..Config::default()
        };
        let p = big_temp_file("huge3.log", 1_200_000);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(out.is_none(), "gate_file_bytes=0 disables the gate");
        let _ = std::fs::remove_file(&p);
    }

    // ----- rule-based allow/deny (feature ①) -----

    #[test]
    fn deny_rule_blocks_small_file() {
        let cfg = Config {
            load_deny: vec!["**/*.log".into()],
            ..Config::default()
        };
        // A tiny file (well under the size gate) is still denied by the rule.
        let p = big_temp_file("rule.log", 100);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(out.unwrap().contains("load_deny"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn deny_rule_holds_even_with_limit_by_default() {
        let cfg = Config {
            load_deny: vec!["**/*.log".into()],
            ..Config::default()
        };
        let p = big_temp_file("rulelim.log", 100);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy(), "limit": 10 })),
            &cfg,
        );
        assert!(
            out.is_some(),
            "deny rule should hold even for a bounded slice"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn deny_rule_yields_to_limit_when_configured() {
        let cfg = Config {
            load_deny: vec!["**/*.log".into()],
            load_deny_even_with_limit: false,
            ..Config::default()
        };
        let p = big_temp_file("rulelim2.log", 100);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy(), "limit": 10 })),
            &cfg,
        );
        assert!(
            out.is_none(),
            "with the flag off, a slice of a denied file is allowed"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn allow_rule_bypasses_size_gate() {
        let cfg = Config {
            load_allow: vec!["**/*.md".into()],
            ..Config::default()
        };
        let p = big_temp_file("huge.md", 1_200_000);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(
            out.is_none(),
            "a huge but allow-listed file must bypass the gate"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn deny_fires_even_when_size_gate_disabled() {
        let cfg = Config {
            gate_file_bytes: 0,
            load_deny: vec!["**/*.log".into()],
            ..Config::default()
        };
        let p = big_temp_file("nogate.log", 100);
        let out = run(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(out.is_some(), "deny rules are independent of the size gate");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn ignores_other_tools() {
        let cfg = Config::default();
        let mut input = read_input(json!({ "file_path": "/etc/hosts" }));
        input.tool_name = "Bash".into();
        // gate_bash defaults off, and this isn't a dump anyway.
        assert!(run(&input, &cfg).is_none());
    }

    // ----- Bash gate (P2) -----

    fn bash_cfg() -> Config {
        Config {
            gate_bash: true,
            ..Config::default()
        }
    }

    fn bash_input(cmd: &str) -> HookInput {
        HookInput {
            tool_name: "Bash".into(),
            tool_input: Some(json!({ "command": cmd })),
            ..Default::default()
        }
    }

    fn denied(cmd: &str) -> bool {
        run(&bash_input(cmd), &bash_cfg()).is_some()
    }

    #[test]
    fn bash_gate_off_by_default() {
        let cfg = Config::default(); // gate_bash = false
        assert!(run(&bash_input("cat /var/log/huge.log"), &cfg).is_none());
    }

    #[test]
    fn denies_unbounded_cat() {
        assert!(denied("cat /var/log/huge.log"));
    }

    #[test]
    fn allows_bounded_cat() {
        assert!(!denied("cat /var/log/huge.log | head -n 50"));
    }

    #[test]
    fn journalctl_needs_a_bound() {
        assert!(denied("journalctl -u ssh"));
        assert!(!denied("journalctl -u ssh -n 100"));
        assert!(!denied("journalctl -u ssh --since '1 hour ago'"));
    }

    #[test]
    fn recursive_grep_needs_max_count() {
        assert!(denied("grep -rn pattern ."));
        assert!(!denied("grep -rn -m 20 pattern ."));
        // Non-recursive grep is not a dump.
        assert!(!denied("grep -n pattern file.txt"));
    }

    #[test]
    fn dmesg_and_full_tail() {
        assert!(denied("dmesg"));
        assert!(!denied("dmesg | head"));
        assert!(denied("tail -n +1 /var/log/huge.log"));
        assert!(!denied("tail -n 100 /var/log/huge.log"));
    }

    #[test]
    fn redirect_is_allowed() {
        assert!(!denied("cat x > y"));
        assert!(!denied("cat /var/log/huge.log >> out.txt"));
    }

    // ----- panic barrier (backlog e5623f14: panic must deny, not silently allow) -----

    #[test]
    fn analyse_denies_on_panic_instead_of_silently_allowing() {
        // Before this fix, `main` called `run` directly; a panic anywhere in the
        // detector unwound past this hook entirely and was caught only by
        // `run_hook`'s outer backstop, which exits 0 with no output — a silent
        // allow of the very Read/Bash call the detector failed to analyse. This
        // pins the barrier itself, independent of finding a real panic-triggering
        // input through `run`'s glob/regex/filesystem logic.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // suppress panic noise in test output
        let out = analyse_barrier(std::panic::AssertUnwindSafe(|| -> Option<String> {
            panic!("simulated analyser panic");
        }));
        std::panic::set_hook(prev);
        assert_eq!(
            out.as_deref(),
            Some(INTERNAL_ERROR_REASON),
            "a panicking analyser must deny, not silently allow"
        );
    }

    #[test]
    fn analyse_matches_run_on_the_happy_path() {
        // No panic → `analyse` must behave identically to `run`.
        let cfg = Config::default();
        let p = big_temp_file("analyse-huge.log", 1_200_000);
        let out = analyse(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(out.unwrap().contains("sub-agent"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn analyse_allows_normal_sized_file() {
        let cfg = Config::default();
        let p = big_temp_file("analyse-small.rs", 60_000);
        let out = analyse(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg,
        );
        assert!(out.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_gate_unaffected_by_bash_flag() {
        // The Read path must keep working with the default (Bash-off) config.
        let cfg = Config::default();
        let p = big_temp_file("readstill.log", 1_200_000);
        assert!(run(
            &read_input(json!({ "file_path": p.to_string_lossy() })),
            &cfg
        )
        .is_some());
        let _ = std::fs::remove_file(&p);
    }
}
