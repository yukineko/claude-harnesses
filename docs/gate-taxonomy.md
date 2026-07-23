# ゲート分類（gate taxonomy）

> 目的: このリポジトリに存在する全ゲート（Stop フック群の crate + `scripts/check-*.py` 系）を
> 「何を判定しているか」で3階層に分類し、「いつ発火するか」（トリガー種別）を横断的に一覧できるようにする。
> 個別ゲートの詳細挙動は `docs/OVERVIEW.md`「検証ゲート（Stop フック群）」節または各
> `crates/<name>/README.ja.md` を参照。用語（gate / fail-closed 等）は `docs/GLOSSARY.md` を参照。

## 3階層の定義

- **実装正しさ (implementation-correctness)** — build/lint/secret-scan/バイナリ再現性など、
  「コードとして機械的に正誤が決まる」判定。曖昧さの余地が小さい。
- **テスト適合性 (test-fitness)** — 「実装にテストが伴っているか」「テストが実際に fault を
  捕捉できるか」を見る判定。test-first 手順の遵守・mutation kill-rate・テスト弱体化検知など。
- **総合判断 (holistic-judgment)** — 意味的不変条件・コードレビュー・仕様との整合など、
  単一の機械的基準に還元しにくい複合判断。inject/subprocess モードで LLM や複数チェッカーの
  総合評価に依存するものが多い。

分類が一意に決まらないゲートには、表の「分類理由」列に一言添える。

## ゲート一覧

### Stop フック群（condukt run 中・エージェントのターン終了を阻止する crate）

| ゲート | 分類 | トリガー種別 | 分類理由 |
|---|---|---|---|
| donegate | 実装正しさ | Stop hook | build/test/lint コマンドの exit code のみで判定する機械的ゲート |
| tdd | テスト適合性 | Stop hook | RED→GREEN 遷移の証跡（`check-oracle`）を要求する test-first 手順の遵守ゲート |
| mutategate | テスト適合性 | Stop hook / CI（`cargo-mutants` 経由、CLI 専用） | mutation kill-rate という「テストが fault を捕捉できるか」を測る指標のゲート。プラグインではなくワークスペース内製ツール |
| propguard | 総合判断 | Stop hook | done_criteria から導出した意味的不変条件（プロパティ）の充足を判定。導出・閾値判定は決定論だが、各プロパティの成否自体は inject/subprocess の総合評価に依存する |
| reviewgate | 総合判断 | Stop hook | diff に対するコードレビュー（inject または独立レビュアー subprocess）という複合判断 |
| precommit-audit | 総合判断 | Stop hook | シークレット混入・例外握り潰し・禁止 API・巨大ファイル等の汎用＋プロジェクトルールを diff に照合する複合監査（個々のルールは機械的だが、束ねて「diff が clean か」を総合判定する点で holistic 側に置く） |
| budgetguard | 実装正しさ | Stop hook | gauge の記録値とコスト上限を比較するだけの機械的閾値判定 |

### スクリプトゲート（`scripts/check-*.py`。pre-commit / pre-push / CI で駆動）

| ゲート (スクリプト) | 分類 | トリガー種別 | 分類理由 |
|---|---|---|---|
| `check-prompt-injection.py`（injectguard） | 総合判断 | pre-commit（advisory）/ CI（`injectguard.yml`、非バイパスの本ゲート） | プロンプト資産中の隠蔽・検証バイパス・exfiltration 文言というパターンを走査するが、文脈判断（防御 framing の除外等）を伴う複合判定 |
| `check-fail-open.py`（fail-open-guard） | 実装正しさ | pre-commit / CI（`fail-open.yml`、`--ratchet` 付き） | 既知のコードパターン（swallow-and-allow 等の構文形）を機械的に検出する静的解析 |
| `check-doc-claims.py`（doc-claims） | 実装正しさ | pre-commit / CI（`doc-claims.yml`） | ドキュメント中の `path:line` 引用が実際のファイル内容と一致するかを機械照合する |
| `check-test-weakening.py`（test-weakening） | テスト適合性 | pre-commit / CI（`test-weakening.yml`） | 同一 commit 内で実装とテストが変化した際、テスト側のアサーション削除等の「弱体化」を検出する |
| `check-plugin-versions.py`（version-lockstep） | 実装正しさ | pre-commit / CI（`version-lockstep.yml`） | 3ファイルの version 文字列が一致するかだけを見る機械的整合チェック |
| `check-version-bumped.py`（bump-on-change） | 実装正しさ | pre-commit / CI（`version-lockstep.yml`） | base ref との version 比較という機械的な bump-on-change チェック |
| `check-bin-reproducibility.py` | 実装正しさ | CI（`bin-reproducibility.yml`） | ソースからの再ビルド結果と committed バイナリを比較する再現性検査（悪性デルタのみ判定に使用） |
| `check-bench-regression.py` | テスト適合性 | CI（`bench-regression.yml`） | benchkit の SWE-bench 実行結果から回帰（regression）を検出する、テスト実行結果ベースの判定 |
| `check-gate-crates-sync.py` | 実装正しさ | CI（`gate-crates-sync.yml`。および pre-push が参照する GATE_CRATES 集合の元） | GATE_CRATES 集合が複数ソース間で一致するかを機械照合する |
| `check-plugin-rollout.py` | 実装正しさ | pre-push（advisory）/ CI（`gate-crates-sync.yml` はテストのみ実行、本体は pre-push 駆動） | source version と registry version の文字列比較のみの機械判定 |
| `check-ci-red.py` | 実装正しさ | pre-push（advisory）/ CI（`gate-crates-sync.yml` はテストのみ実行、本体は pre-push 駆動） | GitHub Actions の run 履歴から連続 red 回数を数えるだけの機械判定 |

## トリガー種別の凡例

- **Stop hook** — condukt worker/agent のターン終了（Stop イベント）時に発火し、条件未達なら停止を延長する。
- **pre-commit** — `.githooks/pre-commit`（`git config core.hooksPath .githooks` で opt-in）。
  ローカルの advisory 速報層。fail-soft（python3/スクリプト不在時は素通り）。
- **pre-push** — `.githooks/pre-push`。GATE_CRATES 変更検知・rollout drift・chronically-red CI の
  advisory 通知。push 自体は止めない設計（fail-soft）。
- **CI** — GitHub Actions ワークフロー。多くは pre-commit と同じスクリプトを再実行する非バイパスの本ゲート。

## 備考

- `check-plugin-rollout.py` と `check-ci-red.py` は CI ワークフロー
  (`.github/workflows/gate-crates-sync.yml`) では対応する `test_check_plugin_rollout.py` /
  `test_check_ci_red.py`（ユニットテスト）のみが実行され、本体スクリプト自体の実運用駆動は
  `.githooks/pre-push` に限られる（測定日 2026-07-23、`.github/workflows/` grep による）。
- Continuous-Audit（GATE_CRATES = blastguard / propguard / specguard / stuckguard / mutategate の
  敵対的レビュー）は本表のような常時ゲートではなく opt-in の別ループなので、この一覧には含めない
  （`docs/OVERVIEW.md` の該当節を参照）。

## 関連ドキュメント

- [GLOSSARY.md](GLOSSARY.md) — 用語・クレート早見表
- [OVERVIEW.md](OVERVIEW.md)「検証ゲート（Stop フック群）」節 — 各 Stop フックゲートの詳細挙動
