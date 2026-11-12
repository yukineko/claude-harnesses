#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::*;

/// Realistic long /flow-style result report in Japanese. Prose is Japanese; ids, versions,
/// file:line, and verbatim English gate/test quotes are in code spans / blockquotes / quote lines.
fn report_ja() -> String {
    r#"# /flow 実行結果レポート

## 概要

- backlog タスク c96f571a と e843a6b4 を処理し、run-20261112-072718-37987 で統合まで完了しました。
- 変更したクレートは fde と harness-core の二つで、バージョンは 0.1.38 と 0.4.12 に上げました。
- 統合コミットは a1fdc674 と b6e6fe77 と 45eeaca7 で、すべて main に取り込み済みです。

## 実装の内容

- 証拠ファイルの閉じ込め処理を `crates/fde/src/evidence.rs:42` に追加しました。
- 判定不能の経路は `crates/fde/src/main.rs:118` で制限側に倒すようにしました。
- 共有三値型の利用箇所を `crates/harness-core/src/verdict.rs:57:9` に揃えました。
- 設定の読み込みで失敗した場合は、空集合ではなく未判定を返します。
- 閾値の下限を床付きで丸める処理を入れ、常に合格になる状態を防いでいます。

## テスト結果

RED を先に観測してから GREEN にしました。実行結果は次のとおりです。

> test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
> test result: FAILED. 3 passed; 2 failed; 0 ignored

コマンドの出力をそのまま引用します。

$ cargo test -p fde --test evidence_confine
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
warning: unused variable: `scratch`
error[E0425]: cannot find value `confine_root` in this scope

## ゲートの判定

- donegate はすべての完了基準を満たしたと判定し、通過させました。
- reviewgate は独立した検証者の指摘を二件出し、どちらも修正済みです。
- version bump のゲートは `check-version-bumped.py` が成功を返しました。
- ロールアウトは `scripts/rollout-plugins.sh` を実行して完了しています。

## 残課題

- 並行セッションの帰属問題は backlog に 7f3a9c21 として残しました。
- 計測待ちのプラグインは parked として宣言済みで、再検討の起点も書きました。
- 次回は `crates/fde/src/report.rs:200` の整形処理を見直す予定です。
- 未計測の項目は未計測と明記し、推測で埋めていません。

## 次の一手

- compass の北極星に照らして、証拠の検証経路を次の焦点にします。
- 担当する worktree は別に用意し、統合は main で行います。
- 作業はすべて分離した worktree 上で行い、最後に merge して削除します。
- 以上で今回の実行は完了です。確認が必要な点があればお知らせください。
"#
    .to_string()
}

/// Same structure, prose translated to English; ids / quotes / code spans identical.
fn report_en() -> String {
    r#"# /flow run result report

## Summary

- I processed backlog tasks c96f571a and e843a6b4 and finished the merge in run-20261112-072718-37987 completely.
- The changed crates are fde and harness-core, and I bumped their versions to 0.1.38 and 0.4.12 respectively.
- The integration commits are a1fdc674 and b6e6fe77 and 45eeaca7, and all of them have been merged into main already.

## What was implemented

- I added the evidence file confinement handling in `crates/fde/src/evidence.rs:42` for this change.
- The undetermined path is now resolved to the restrictive side in `crates/fde/src/main.rs:118` as requested.
- I aligned every usage of the shared three-valued type in `crates/harness-core/src/verdict.rs:57:9` as well.
- When loading the configuration fails, it returns undetermined instead of returning an empty set.
- I added a floor to the threshold clamp so that the gate can never be reduced to always passing.

## Test results

I observed RED first and only then made it GREEN. The execution results are the following.

> test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
> test result: FAILED. 3 passed; 2 failed; 0 ignored

I am quoting the command output exactly as it was printed.

$ cargo test -p fde --test evidence_confine
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
warning: unused variable: `scratch`
error[E0425]: cannot find value `confine_root` in this scope

## Gate verdicts

- The donegate judged that every completion criterion was satisfied and let the stop through.
- The reviewgate raised two findings from an independent verifier, and both of them have been fixed.
- The version bump gate `check-version-bumped.py` returned success for this change.
- The rollout has been completed by running `scripts/rollout-plugins.sh` after the merge.

## Remaining work

- The attribution problem of concurrent sessions was left in the backlog as 7f3a9c21 for later.
- Plugins that wait for measurement are declared as parked, and the starting point for reconsidering is written down.
- Next time I plan to review the formatting logic in `crates/fde/src/report.rs:200` carefully.
- Unmeasured items are explicitly marked as unmeasured, and I did not fill them in by guessing.

## Next step

- In light of the compass north star, the evidence verification path will be the next focus.
- A separate worktree will be prepared for the owner, and the integration happens on main.
- All the work is done on isolated worktrees, and in the end each branch is merged and deleted.
- That completes this run, so please let me know if anything needs to be confirmed.
"#
    .to_string()
}

fn condukt_ja() -> String {
    let mut s = String::from("# condukt 完了報告\n\n");
    for i in 0..12 {
        s.push_str(&format!(
            "- タスク t{i} は worktree で実装され、コミット {:07x}1 に統合されました。検証者は合格と判定しました。\n",
            0xa1fdc67 + i
        ));
        s.push_str(&format!(
            "  - 変更箇所は `crates/answerlang/src/stop.rs:{}` と `crates/answerlang/src/strip.rs:{}:3` です。\n",
            10 + i,
            20 + i
        ));
    }
    s.push_str("\n> error: aborting due to previous error; 2 warnings emitted\n");
    s.push_str("> test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured\n\n");
    s.push_str("run-20261113-101500-4242 の結果として、バージョン v0.1.0 を配布しました。\n");
    s.push_str("全ての完了基準が満たされ、残課題はありません。以上で報告を終わります。\n");
    s
}

fn condukt_en() -> String {
    let mut s = String::from("# condukt completion report\n\n");
    for i in 0..12 {
        s.push_str(&format!(
            "- Task t{i} was implemented in a worktree and integrated into commit {:07x}1. The verifier judged it as passing.\n",
            0xa1fdc67 + i
        ));
        s.push_str(&format!(
            "  - The changes are located in `crates/answerlang/src/stop.rs:{}` and `crates/answerlang/src/strip.rs:{}:3` respectively.\n",
            10 + i,
            20 + i
        ));
    }
    s.push_str("\n> error: aborting due to previous error; 2 warnings emitted\n");
    s.push_str("> test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured\n\n");
    s.push_str(
        "As the result of run-20261113-101500-4242, I distributed version v0.1.0 to everyone.\n",
    );
    s.push_str("All completion criteria are satisfied and there are no remaining issues. That is the end of this report.\n");
    s
}

fn check(report: &str, expect_block: bool, label: &str) {
    assert!(report.lines().count() >= 30, "{label}: report must be long");
    let env = Env::new();
    env.write_config(JA_CONFIG);
    let t = env.answer_transcript("t.jsonl", report);
    let out = env.stop("s1", Some(&t), false);
    assert_eq!(out.code, 0);
    if expect_block {
        assert!(
            out.is_block(),
            "{label}: English report must block, got {:?}",
            out.stdout
        );
        assert!(out.reason().to_lowercase().contains("japanese"));
    } else {
        assert!(
            !out.is_block(),
            "{label}: Japanese report must not block, got {:?}",
            out.stdout
        );
        assert!(
            out.is_empty(),
            "{label}: clean must be silent, got {:?}",
            out.stdout
        );
    }
}

#[test]
fn japanese_flow_report_with_ids_and_quotes_does_not_block() {
    check(&report_ja(), false, "flow-ja");
}

#[test]
fn english_flow_report_blocks() {
    check(&report_en(), true, "flow-en");
}

#[test]
fn japanese_condukt_report_does_not_block() {
    check(&condukt_ja(), false, "condukt-ja");
}

#[test]
fn english_condukt_report_blocks() {
    check(&condukt_en(), true, "condukt-en");
}

#[test]
fn japanese_report_survives_a_realistic_reentry_sequence() {
    // The user's prompt was Japanese; auto detection + inject + clean Stop.
    let env = Env::new();
    env.write_config("language = \"auto\"\n");
    let p = env.prompt("s1", "backlog を処理して結果を報告してください");
    assert!(p.context().contains(JA_LINE), "stdout: {}", p.stdout);
    let t = env.answer_transcript("t.jsonl", &report_ja());
    assert!(env.stop("s1", Some(&t), false).is_empty());
    let t2 = env.answer_transcript("t2.jsonl", &report_en());
    assert!(env.stop("s1", Some(&t2), false).is_block());
}
