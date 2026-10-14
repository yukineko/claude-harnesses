//! Independent guard for backlog 7b6bcfe6 (Japanese tokenization in
//! `harness_core::lessons::text_similarity`), written by the verifier, not the
//! implementer (CLAUDE.md §2a).
//!
//! The fix tokenizes CJK runs as character bigrams. That raises Japanese scores,
//! so the risk it introduces is the opposite of the defect it fixes: unrelated
//! Japanese titles sharing common bigrams/particles ("する", "の", "が") could be
//! pushed over the 0.6 duplicate threshold used by backlog `dedup` and overwatch
//! `possible_duplicate`. This file pins both directions with REAL backlog titles
//! (taken from `backlog list --status pending` on 2026-10-14):
//! - unrelated real titles stay below 0.6,
//! - near-identical Japanese titles reach 0.6,
//! - identical mixed JP/ASCII content scores exactly 1.0,
//! - ASCII-only scores are byte-for-byte what the pre-fix tokenizer produced.

use harness_core::lessons::text_similarity;

const THRESHOLD: f64 = 0.6;

// Real pending titles (id in the name).
const T_233F819C: &str = "帰属のバグ: stop-verify-worktree.py が他セッションの未コミット変更で私の停止をブロックする (CLAUDE.md 8 が名指しする形)";
const T_6A668D24: &str = "claim-skip ゲートの deadlock: is-claimed は期限切れしないが backlog は 1 時間で requeue する — 51 日前の死んだ claim が task を恒久的に着手不能にする";
const T_FF98FED0: &str = "gauge/harness-core P2: 未知の model id が 0 ドルに価格付けされ、予算ゲートが恒久的に無効化される (unknown_model_is_free が仕様として固定している)";
const T_016C2562: &str =
    "backlog: wontfix と裁定された項目が pending のまま next --claim の候補に上がる";
const T_ECE51B35: &str = "衝突マーカー入り (TOML として読めない) の .backlog/tasks.toml が commit でき、main まで fast-forward された — merge 完了 commit を検査するゲートが無い";
const T_E9262D91: &str = "condukt lock の待機ループが poll ごとに kill サブプロセスを spawn し、負荷下で自分の獲得を遅らせる";
const T_3AA215E1: &str = "blastguard: rm の判定を「形」から「壊す対象（資産か・復帰可能か）」へ再設計する（ユーザー裁定）";
const T_864EB3E8: &str = "docs/gate-taxonomy.md が GitHub Actions workflow (injectguard.yml / fail-open.yml 等) を『本ゲート』と記述し CLAUDE.md §7 の GHA 全面禁止と矛盾する";
const T_B280CFA2: &str = "blastguard: 安全な置き場の confined Ask が、シンボリックリンクの後の .. を文字列で消し、削除先を取り違える";
const T_F2EDE46C: &str = "Stop gate の block は repeat ledger により同じ理由の 2 回目が通る。各 gate の文書と表明がこれを書いていない";
const T_71A880A2: &str = "backlog の起票と作業なし close の両方が、観測済みか未観測かを区別して記録しない — 疑い (未観測) が事実として積まれ・閉じられる";
const T_960F40BD: &str = "backlog notes が成果物を session scratchpad に預けるため、待機中の p1 が存在しないファイルを前提にし続ける";
const T_8A4FE104: &str =
    "condukt claim.rs backlog_pending maps a non-zero backlog list to an empty Vec";
const T_F7A0CADC: &str = "condukt editgate::tests::a_clean_crate_still_passes fails every full-binary run (rustup cannot choose cargo: HOME race with with_home)";

const ALL_UNRELATED: &[&str] = &[
    T_233F819C, T_6A668D24, T_FF98FED0, T_016C2562, T_ECE51B35, T_E9262D91, T_3AA215E1, T_864EB3E8,
    T_B280CFA2, T_F2EDE46C, T_71A880A2, T_960F40BD,
];

fn assert_below(a: &str, b: &str) {
    let s = text_similarity(a, b);
    assert!(
        s < THRESHOLD,
        "unrelated real titles scored {s} >= {THRESHOLD}:\n  {a}\n  {b}"
    );
}

fn assert_at_least(a: &str, b: &str) {
    let s = text_similarity(a, b);
    assert!(
        s >= THRESHOLD,
        "near-identical titles scored {s} < {THRESHOLD}:\n  {a}\n  {b}"
    );
}

// --- unrelated real titles must NOT be flagged ------------------------------

#[test]
fn unrelated_real_titles_attribution_bug_vs_claim_deadlock() {
    assert_below(T_233F819C, T_6A668D24);
}

#[test]
fn unrelated_real_titles_gauge_pricing_vs_wontfix_candidates() {
    assert_below(T_FF98FED0, T_016C2562);
}

#[test]
fn unrelated_real_titles_conflict_marker_vs_lock_poll() {
    assert_below(T_ECE51B35, T_E9262D91);
}

#[test]
fn unrelated_real_titles_rm_redesign_vs_gha_doc() {
    assert_below(T_3AA215E1, T_864EB3E8);
}

#[test]
fn unrelated_real_titles_symlink_ask_vs_repeat_ledger() {
    assert_below(T_B280CFA2, T_F2EDE46C);
}

#[test]
fn unrelated_real_titles_observation_record_vs_scratchpad_notes() {
    assert_below(T_71A880A2, T_960F40BD);
}

/// Every pair among the 12 unrelated titles above (66 pairs) stays below 0.6.
#[test]
fn every_pair_of_unrelated_real_titles_stays_below_threshold() {
    for (i, a) in ALL_UNRELATED.iter().enumerate() {
        for b in &ALL_UNRELATED[i + 1..] {
            assert_below(a, b);
        }
    }
}

// --- near-identical Japanese titles MUST be flagged -------------------------

/// Real title with exactly one kanji substituted (上 -> 挙).
#[test]
fn one_char_substitution_in_real_mixed_title_is_flagged() {
    assert_at_least(
        T_016C2562,
        "backlog: wontfix と裁定された項目が pending のまま next --claim の候補に挙がる",
    );
}

/// Real title with exactly one kanji substituted (私 -> 僕).
#[test]
fn one_char_substitution_in_real_title_is_flagged() {
    assert_at_least(
        T_233F819C,
        "帰属のバグ: stop-verify-worktree.py が他セッションの未コミット変更で僕の停止をブロックする (CLAUDE.md 8 が名指しする形)",
    );
}

/// Real title with one particle inserted (期限切れしない -> 期限切れはしない).
#[test]
fn inserted_particle_in_real_title_is_flagged() {
    assert_at_least(
        T_6A668D24,
        "claim-skip ゲートの deadlock: is-claimed は期限切れはしないが backlog は 1 時間で requeue する — 51 日前の死んだ claim が task を恒久的に着手不能にする",
    );
}

/// Pure-Japanese clause with one particle inserted (を).
#[test]
fn inserted_particle_in_pure_japanese_is_flagged() {
    assert_at_least(
        "分かち書きしないため文節が丸ごと一つのトークンになる",
        "分かち書きをしないため文節が丸ごと一つのトークンになる",
    );
}

/// Pure-Japanese, one character differs at the end (る -> た).
#[test]
fn one_char_difference_in_pure_japanese_is_flagged() {
    assert_at_least(
        "観測済みか未観測かを区別して記録しない",
        "観測済みか未観測かを区別して記録しなかった",
    );
}

/// REAL duplicate pair from this repo's own backlog (81888b5a / 2522b0ba: the
/// second is the same item re-filed "(棄却の誤りにより復活)"). The pre-fix
/// tokenizer scored it 0.727 (caught at 0.6). Observed at e40b5798: 0.545
/// (NOT caught) — the bigram expansion of the appended Japanese parenthetical
/// dilutes the Jaccard union. A tokenizer change meant to make Japanese
/// near-duplicates visible must not make a real one invisible.
#[test]
fn real_refiled_duplicate_pair_stays_flagged() {
    assert_at_least(
        "enforce_edit_gate の malformed verdict の既定値を restrictive にする",
        "enforce_edit_gate の malformed verdict 既定値を restrictive にする (棄却の誤りにより復活)",
    );
}

// --- identical mixed JP/ASCII content scores exactly 1.0 --------------------

#[test]
fn identical_mixed_title_scores_one() {
    assert_eq!(text_similarity(T_ECE51B35, T_ECE51B35), 1.0);
}

/// Same content, differing only in ASCII case and punctuation (which are not
/// tokens), must be a perfect match.
#[test]
fn mixed_title_differing_only_in_case_and_punctuation_scores_one() {
    assert_eq!(
        text_similarity(
            "blastguard: mv ~/.claude/plugins /tmp/p が許可される (9271d739 の双子)",
            "Blastguard — mv ~/.Claude/Plugins /tmp/p が許可される。9271D739 の双子!",
        ),
        1.0
    );
}

// --- ASCII-only scores are unchanged from the pre-fix tokenizer -------------

/// Pre-fix tokenizer (2a0167f6 lessons.rs: split on non-alphanumeric, keep
/// len >= 3 bytes, drop STOP), computed by hand:
/// A = {condukt, claim, backlog, pending, maps, non, zero, list, empty, vec} (10)
/// B = {condukt, editgate, tests, clean, crate, still, passes, fails, every,
///      full, binary, run, rustup, cannot, choose, cargo, home, race} (18;
///      "with" is a stopword, "a" is too short, "home" is deduplicated)
/// |A∩B| = 1 (condukt), |A∪B| = 27.
#[test]
fn ascii_only_real_pair_matches_pre_fix_value() {
    assert_eq!(text_similarity(T_8A4FE104, T_F7A0CADC), 1.0 / 27.0);
}

/// A = {implement, overwatch, lease, scope}, B = {implement, overwatch, lease,
/// done, criteria}: 3 / 6.
#[test]
fn ascii_only_partial_overlap_matches_pre_fix_value() {
    assert_eq!(
        text_similarity(
            "implement overwatch lease scope",
            "implement overwatch lease done_criteria"
        ),
        0.5
    );
}

// --- degenerate input: no NaN, no panic -------------------------------------

#[test]
fn degenerate_inputs_are_finite_and_in_range() {
    for (a, b) in [
        ("", ""),
        ("", "日本語"),
        ("😀", "😀"),
        ("。、！", "・ー"),
        ("製", "製"),
    ] {
        let s = text_similarity(a, b);
        assert!(
            s.is_finite() && (0.0..=1.0).contains(&s),
            "{a:?} vs {b:?} -> {s}"
        );
    }
}
