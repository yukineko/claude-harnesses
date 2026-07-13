# repo健全性検証レポート(2026-07-13)

`main` を最新化した直後の検証結果。対象は `docs/review-redesign-implementation-items.md`
のブリーフで最優先とした安全性修正の反映確認と、`cargo test --workspace` の全体健全性チェック。

**前提**: このリポジトリの目的は日次1万行規模の変更が出るプロジェクトで人間のレビュー負荷を
減らすこと。対象プラットフォームは **WSL / Mac / Linux**(ネイティブWindowsは対象外)。以下の
記述は全てこの前提でスコープを絞ってある。

## 結論サマリ

| 項目 | 状態 |
|---|---|
| git / バージョンlockstep / `cargo build --workspace` | クリーン |
| 問題1/1b/1c(specguard polarity gate, ratify lock, stuckguard window boundary) | **修正反映済み**、回帰テストgreen |
| condukt の calibrated-confidence フォールバック不具合 | **未修正**(実運用に影響しうる) |
| condukt の concurrent RMW lost-update race | **未修正**(実運用に影響しうる) |
| blastguard/overwatch テストの `#[cfg(unix)]` ガード欠如 | 未修正だが実害なし(規約逸脱のみ) |
| その他の `cargo test --workspace` 失敗(backlog/autoflow の `pid_alive`、fugu-router/harness-core のPOSIX絶対パス、donegate の短縮パスなど) | **対象外扱いでよい** — WSL/Mac/Linuxでは発生しない、ネイティブWindows限定の検証アーティファクト |

## 1. 安全性重要修正 — 反映確認済み(良いニュース)

`docs/review-redesign-implementation-items.md` の 問題1/問題1b/問題1c として書き出していた
3件は、別セッションによる実装作業(直近72コミット)で全て解消されていることを確認した。
以前 `#[ignore]` されていた回帰テストが、いずれも `#[ignore]` 無しでgreenになっている。

```sh
cargo test -p specguard --bin specguard --quiet zzz_adversarial
# running 5 tests; test result: ok. 5 passed; 0 failed

cargo test -p specguard --bin specguard --quiet write_lock_reports_error
# running 1 test; test result: ok. 1 passed; 0 failed

cargo test -p stuckguard --bin stuckguard --quiet near_repeat_escalates_even_past_window_boundary
# running 1 test; test result: ok. 1 passed; 0 failed
```

- **specguard polarity gate**: authz/route axis の deterministic backstop(Phase1)+
  local-context-fingerprint による shingle/Jaccard マッチング(Phase2)が実装され、
  polarity-swap によるゲートすり抜けが塞がれている。
- **specguard `ratify.rs::write_lock`**: 書き込み失敗を握りつぶして成功報告していた問題が
  修正され、書き込み失敗が正しくエラーとして伝播するようになっている。
- **stuckguard `detect.rs::Trip::key`**: ウィンドウ(既定12件)からの evict でエスカレーション
  カウンタがリセットされる問題が修正され、ウィンドウ境界を跨いでもニアリピート検出が機能する。

## 2. 実運用に影響しうる未修正バグ(condukt)

プラットフォーム非依存で、WSL/Mac/Linuxでも再現する。condukt はレビュー自動化パイプラインの
中核(ゲート判定・確信度較正・状態管理)なので、ここのバグは「人間のレビュー負荷を減らす」
という目的に直接効いてくる — 誤った確信度や失われた状態更新は、自動判定への信頼を損ない、
結果として人間の再確認を増やす方向に働く。

### 2.1 calibrated-confidence フォールバックが機能しない

- **場所**: `crates/condukt/src/main.rs:4001` 付近
- **テスト**: `calibrated_confidence_tests::flag_supplied_but_probe_unusable_falls_back`
- **症状**: probe が使用不能な状況で `None`(安全側フォールバック)を期待しているが、
  実際は `Some(Medium)` が返る。
- **懸念**: probe が壊れている/使えない状態でも確信度が「出てしまう」ため、判定不能を
  中途半端な確信度で覆い隠すリスクがある。

### 2.2 concurrent read-modify-write でのlost-update race

- **場所**: `crates/condukt/src/state.rs:1861` 付近
- **テスト**: `state::tests::concurrent_rmw_does_not_lose_updates`
- **症状**: 期待値 `"A"` に対し実際は `"seed"` が返る。panicメッセージが
  「thread A's goal update was lost (last-writer-wins race)」と明言している通り、
  並行書き込み時に更新が失われる。
- **懸念**: 複数エージェント/複数フローが同時に状態を更新する運用(このリポジトリが
  想定する高頻度・並行実行環境そのもの)で、記録されたはずの更新が消える。

## 3. 軽微な規約逸脱(実害なし)

- **場所**: `crates/blastguard/tests/overwatch_violation.rs`(L118, 125, 143, 158)、
  `crates/overwatch/tests/bridge_to_backlog.rs`(L12, L41)
- **内容**: `std::os::unix::fs::PermissionsExt::set_mode` を `#[cfg(unix)]` ガード無しで
  使用している。コードベース内の他の同種利用(`specguard/src/scope.rs`,
  `condukt/tests/ci_loop_e2e.rs`, `harness-core/src/store.rs`, `stuckguard/src/main.rs`,
  `tracekit/src/span.rs`)は全て `#[cfg(unix)]` でガードされており、この2ファイルだけが逸脱。
- **実害**: WSL/Mac/Linuxでは `cfg(unix)` が真になるため実運用上の問題は無い。ネイティブ
  Windowsでビルドしようとした場合のみコンパイルが通らない(このリポジトリの対象プラット
  フォーム外)。
- **優先度**: 低。ただし規約の一貫性を保つ観点では直しておく価値がある。

## 4. 対象外と判断した失敗(参考記録)

以下は `cargo test --workspace` をネイティブWindows機で実行した際に検出されたが、
WSL/Mac/Linuxでは発生しないため対応不要と判断したもの:

- `backlog`/`autoflow` の `pid_alive()` — `kill -0` フォールバックはWSL/Mac/Linux全てで
  動作する(ネイティブWindowsのみ非対応)。
- `fugu-router`/`harness-core` のテストが使うハードコードPOSIX絶対パス(`/repo/...` 等)—
  WSL/Mac/Linuxでは通常通り絶対パスとして認識される。
- `donegate` の Windows短縮パス(`HIROYU~1`)絡みの失敗 — ネイティブWindows環境固有の
  パス表記の問題。

## 次のアクション(未着手・提案)

1. condukt の2件(2.1, 2.2)を優先して修正する。
2. `#[cfg(unix)]` ガードの追加(3)は低優先だが、他の同種箇所との一貫性のためついでに直す。
