//! Deterministic derivation of semantic *properties* (invariants) from a task's
//! `done_criteria`.
//!
//! `tdd` runs concrete tests; that proves specific cases but never formalizes the
//! *semantic* invariants the generated code must satisfy. Following PGS
//! (Property-Generated Solver, <https://arxiv.org/pdf/2506.18315>), propguard
//! turns the free-text done_criteria into a small, capped set of checkable
//! properties — idempotence, error-path-returns-Err, output-schema stability,
//! bounds/monotonicity, no-partial-write, determinism — via a keyword taxonomy.
//!
//! Honest ceiling: the *derivation* here is deterministic (a rule set over a
//! bilingual keyword catalog), and so is the *count → threshold* block decision
//! in `gate.rs`. Deciding whether a given property actually *holds* for a chunk
//! of generated code is a semantic judgement, and is delegated to inject-mode
//! (the running agent self-verifies against the injected checklist) or to a
//! configured subprocess checker. Richer, task-specific properties beyond this
//! catalog are likewise delegated to the inject-mode agent.

use std::path::Path;

use crate::config::Config;

/// One derived semantic property (invariant) the generated code must satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property {
    /// Stable short id (used in the subprocess `PROP <id>: PASS|FAIL` protocol
    /// and the JSONL log).
    pub id: &'static str,
    /// Human-facing statement of the invariant (bilingual-friendly).
    pub title: &'static str,
    /// How to check it — the hint injected into the checklist / checker prompt.
    pub check_hint: &'static str,
    /// Keywords (already lowercase) in done_criteria that surface this property.
    keywords: &'static [&'static str],
    /// A baseline invariant any generated code should satisfy. Used to pad the
    /// derived set up to `min_properties` when the done_criteria is thin.
    universal: bool,
}

impl Property {
    fn matches(&self, lower_criteria: &str) -> bool {
        self.keywords.iter().any(|k| lower_criteria.contains(k))
    }
}

/// The property catalog. Order is deterministic and stable; derivation preserves
/// it. Universal (baseline) properties are used to pad thin criteria — there are
/// three of them, so a `min_properties` of 3 is always satisfiable.
pub const CATALOG: &[Property] = &[
    Property {
        id: "error-path",
        title: "失敗パスは panic せず Err/エラーを返す (error handling)",
        check_hint: "エラー・境界・異常入力の経路が unwrap/expect/panic せず、Err か明示的エラーを返して伝播することを確認する。",
        keywords: &[
            "error", "panic", "unwrap", "expect", "fail", "exception", "handle",
            "失敗", "エラー", "例外", "握りつぶ", "異常",
        ],
        universal: true,
    },
    Property {
        id: "output-schema",
        title: "出力スキーマ/フォーマットが安定している (schema stability)",
        check_hint: "出力の型・フィールド・JSON 形状・順序が仕様どおりで、後方互換を壊さないことを確認する。",
        keywords: &[
            "schema", "json", "output", "format", "contract", "field", "shape",
            "response", "compat", "serialize",
            "出力", "フォーマット", "スキーマ", "契約", "互換", "形式",
        ],
        universal: true,
    },
    Property {
        id: "determinism",
        title: "決定論的: 同一入力は同一出力を返す (determinism)",
        check_hint: "同じ入力に対して実行のたびに同じ結果になること (隠れた時刻・乱数・イテレーション順序依存が無いこと) を確認する。",
        keywords: &[
            "determinist", "reproducib", "stable", "same input", "same output",
            "consistent", "決定論", "同一", "同じ入力", "再現", "一貫",
        ],
        universal: true,
    },
    Property {
        id: "idempotence",
        title: "冪等: 複数回実行しても結果が変わらない (idempotence)",
        check_hint: "同じ操作を二回以上適用しても、一回のときと同じ最終状態になることを確認する。",
        keywords: &[
            "idempoten", "run twice", "re-run", "rerun", "repeat", "again",
            "retry", "冪等", "再実行", "繰り返し", "二回", "再試行",
        ],
        universal: false,
    },
    Property {
        id: "bounds-monotonicity",
        title: "境界・単調性・閾値が守られる (bounds/monotonicity)",
        check_hint: "値が指定範囲 (上限・下限・閾値) に収まり、ソート順・単調性・カウンタの向きなどの順序不変条件を破らないことを確認する。",
        keywords: &[
            "bound", "limit", "range", "monoton", "threshold", "cap", "clamp",
            "sort", "order", "count", "max", "min",
            "単調", "上限", "下限", "閾値", "範囲", "境界", "並び",
        ],
        universal: false,
    },
    Property {
        id: "no-partial-write",
        title: "部分書き込みが起きない (atomic / no-partial-write)",
        check_hint: "途中で失敗しても半端な状態を残さないこと (書き込みが atomic、あるいは失敗時にロールバックされること) を確認する。",
        keywords: &[
            "write", "atomic", "persist", "save", "transaction", "commit",
            "partial", "flush", "fsync", "rollback", "file",
            "書き込み", "保存", "部分", "永続", "トランザクション",
        ],
        universal: false,
    },
];

/// Derive between `min_properties` and `max_properties` semantic properties from
/// `criteria`. Matched catalog properties come first (in catalog order); if
/// fewer than the minimum match, the set is padded with baseline universal
/// invariants not already present. The result is deterministic and capped.
pub fn derive_properties(criteria: &str, min: usize, max: usize) -> Vec<Property> {
    let lower = criteria.to_lowercase();
    let mut out: Vec<Property> = CATALOG
        .iter()
        .filter(|p| p.matches(&lower))
        .copied()
        .collect();

    if out.len() < min {
        for p in CATALOG.iter().filter(|p| p.universal) {
            if out.len() >= min {
                break;
            }
            if !out.iter().any(|q| q.id == p.id) {
                out.push(*p);
            }
        }
    }

    out.truncate(max.max(1));
    out
}

/// Resolve the current task's done_criteria from, in priority order:
///
/// 1. the `PROPGUARD_CRITERIA` environment variable,
/// 2. a `criteria_file` in the project root (condukt / the agent writes it),
/// 3. the inline `done_criteria` config value.
///
/// Returns `None` when no non-empty source is found — the caller then has
/// nothing to derive properties from and allows the stop.
pub fn source_criteria(cfg: &Config, root: &Path) -> Option<String> {
    if let Ok(v) = std::env::var("PROPGUARD_CRITERIA") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let p = root.join(&cfg.criteria_file);
    // `Known(Some(text))`: use it if non-empty. `Known(None)` (file absent) and
    // an empty/blank file both fall through to the inline `done_criteria`
    // below, unchanged from before this migration. `Undetermined` (the file
    // exists but could not be read — permission denied, invalid UTF-8, …) is
    // NOT folded into "absent": this function has no way to distinguish
    // "nothing to derive from" (which `gate.rs` documents as an intentional
    // allow) from "there IS a criteria file but we could not read it", so the
    // latter is at least surfaced loudly here rather than silently treated as
    // if the file had never existed.
    match harness_core::boundary::read_to_string(&p) {
        harness_core::verdict::Determination::Known(Some(text)) => {
            let t = text.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
        harness_core::verdict::Determination::Known(None) => {}
        harness_core::verdict::Determination::Undetermined(reason) => {
            eprintln!(
                "propguard: cannot read criteria_file {}: {}; falling back to inline \
                 done_criteria",
                p.display(),
                reason.as_str()
            );
        }
    }
    let inline = cfg.done_criteria.trim();
    if !inline.is_empty() {
        return Some(inline.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // These tests mutate the process-global env var PROPGUARD_CRITERIA. cargo runs
    // tests in one binary concurrently, so without serialization a remove_var in one
    // test can race a set_var/read in another. Guard every env-touching test with it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn derives_matching_properties_from_criteria() {
        // A criteria mentioning idempotence, error handling and bounds should
        // surface exactly those semantic properties.
        let dc = "The migration must be idempotent, never panic on a bad row, and \
                  keep the retry count within the configured limit.";
        let props = derive_properties(dc, 3, 5);
        let ids: Vec<&str> = props.iter().map(|p| p.id).collect();
        assert!(
            ids.contains(&"idempotence"),
            "idempotence expected: {ids:?}"
        );
        assert!(ids.contains(&"error-path"), "error-path expected: {ids:?}");
        assert!(
            ids.contains(&"bounds-monotonicity"),
            "bounds expected: {ids:?}"
        );
    }

    #[test]
    fn japanese_criteria_derive_properties() {
        let dc =
            "出力スキーマを安定させ、失敗時は panic せずエラーを返し、冪等に再実行できること。";
        let props = derive_properties(dc, 3, 5);
        let ids: Vec<&str> = props.iter().map(|p| p.id).collect();
        assert!(ids.contains(&"output-schema"));
        assert!(ids.contains(&"error-path"));
        assert!(ids.contains(&"idempotence"));
    }

    #[test]
    fn thin_criteria_are_padded_to_the_minimum() {
        // "documents the flag" matches no property; derivation must still yield
        // at least `min` baseline invariants so a set of properties always exists.
        let props = derive_properties("update the docs", 3, 5);
        assert!(
            props.len() >= 3,
            "must pad to min_properties, got {}",
            props.len()
        );
        assert!(
            props.iter().all(|p| p.universal),
            "padding must use baseline universal invariants"
        );
    }

    #[test]
    fn derivation_is_capped_at_max() {
        // A criteria hitting every catalog keyword must still be capped at max.
        let dc = "idempotent error panic schema json bound limit atomic write \
                  deterministic reproducible";
        let props = derive_properties(dc, 3, 5);
        assert!(props.len() <= 5, "must not exceed max, got {}", props.len());
    }

    #[test]
    fn derivation_is_deterministic() {
        let dc = "idempotent, never panic, output schema stable";
        let a = derive_properties(dc, 3, 5);
        let b = derive_properties(dc, 3, 5);
        assert_eq!(
            a.iter().map(|p| p.id).collect::<Vec<_>>(),
            b.iter().map(|p| p.id).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn env_criteria_take_priority() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PROPGUARD_CRITERIA", "must be idempotent");
        let cfg = Config {
            done_criteria: "inline that should lose".to_string(),
            ..Config::default()
        };
        let got = source_criteria(&cfg, Path::new("/nonexistent-root-xyzzy"));
        std::env::remove_var("PROPGUARD_CRITERIA");
        assert_eq!(got.as_deref(), Some("must be idempotent"));
    }

    #[test]
    fn inline_criteria_used_when_no_env_or_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("PROPGUARD_CRITERIA");
        let cfg = Config {
            done_criteria: "handle errors and keep the schema stable".to_string(),
            ..Config::default()
        };
        let got = source_criteria(&cfg, Path::new("/nonexistent-root-xyzzy"));
        assert_eq!(
            got.as_deref(),
            Some("handle errors and keep the schema stable")
        );
    }

    /// An unreadable (but existing) `criteria_file` must fall through to the
    /// inline `done_criteria`, exactly like a missing file — Undetermined is
    /// not silently promoted to "found nothing", but this function has no
    /// channel to report failure to its caller other than falling through to
    /// the next source in priority order (unchanged from before the
    /// boundary-read migration).
    #[cfg(unix)]
    #[test]
    fn unreadable_criteria_file_falls_through_to_inline() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("PROPGUARD_CRITERIA");

        let dir = std::env::temp_dir().join(format!(
            "propguard-derive-unreadable-criteria-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let criteria_file = "criteria.txt";
        std::fs::write(dir.join(criteria_file), "must be idempotent").unwrap();
        std::fs::set_permissions(dir.join(criteria_file), std::fs::Permissions::from_mode(0o000))
            .unwrap();

        // If chmod 000 doesn't actually deny this uid (e.g. running as root),
        // the test's premise is absent — say so instead of asserting the
        // wrong thing, mirroring harness-core's own boundary tests.
        let denied = std::fs::read_to_string(dir.join(criteria_file)).is_err();

        let cfg = Config {
            criteria_file: criteria_file.to_string(),
            done_criteria: "inline fallback wins".to_string(),
            ..Config::default()
        };
        let got = source_criteria(&cfg, &dir);

        std::fs::set_permissions(dir.join(criteria_file), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            denied,
            "precondition: chmod 000 must deny this uid (running as root?)"
        );
        assert_eq!(
            got.as_deref(),
            Some("inline fallback wins"),
            "an unreadable criteria_file must fall through to inline done_criteria, \
             not be silently treated as if it had real (or no) content"
        );
    }

    #[test]
    fn no_criteria_source_is_none() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("PROPGUARD_CRITERIA");
        let cfg = Config::default(); // empty done_criteria
        assert!(source_criteria(&cfg, Path::new("/nonexistent-root-xyzzy")).is_none());
    }
}
