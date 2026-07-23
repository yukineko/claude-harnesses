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
| `check-prompt-injection.py`（injectguard） | 実装正しさ（是正済み。旧: 総合判断） | pre-commit（advisory）/ CI（`injectguard.yml`、非バイパスの本ゲート） | 正規表現によるパターン走査＋近傍行/見出しの決定論的ウィンドウ判定（`DEFENSE_WINDOW`）のみで判定する。LLM 呼び出し・主観評価は一切なく、`check-fail-open.py`（実装正しさ）と同一手法（receiver-aware パターン照合）。旧分類理由「文脈判断を伴う複合判定」は実装のアルゴリズム的性質（決定論的固定ロジック）を「複合判断」という言葉で holistic 側に誤誘導していたため是正した（詳細は下記「階層混在監査」節） |
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

## 階層混在監査（2026-07-23）

各ゲート（Stop hook crate 7つ + `scripts/check-*.py` 11本）の実装を読み、上記3階層分類との「階層混在」
（実装正しさ用の機械的ゲートに LLM 判断/曖昧な総合基準が明確に混入している。あるいは総合判断ゲートが
実は機械的閾値だけで済む判定を holistic scope に含めている）がないかを監査した。

是正基準: 「明確な混在のみ是正必須」。機械的ゲートに LLM 判断/曖昧な総合判断基準が**明確に**混入している
箇所のみパッチを当てる。グレーゾーン設計（機械的だが複合的な判定を束ねている）自体は混在とみなさず、
コードは変更せず理由をここに明記するに留める。

### 是正した混在: `check-prompt-injection.py`（injectguard）の分類誤り

**発見**: 分類は「総合判断」、分類理由は「プロンプト資産中の隠蔽・検証バイパス・exfiltration 文言という
パターンを走査するが、文脈判断（防御 framing の除外等）を伴う複合判定」だった。しかし実装
（`scripts/check-prompt-injection.py`）を読むと、判定はすべて次の決定論的な要素だけで構成されている:

- `scripts/check-prompt-injection.py:46-76` の `MALICIOUS` — 正規表現パターンのリスト（LLM 呼び出しなし）。
- `scripts/check-prompt-injection.py:80-87` の `DEFENSE_MARKERS` — 偽陽性抑制も同様に正規表現。
- `scripts/check-prompt-injection.py:89` の `DEFENSE_WINDOW = 4  # lines above/below a hit to look for a defense marker`
  — 「文脈判断」の実体は固定行数ウィンドウ内の正規表現マッチという、完全に決定論的なアルゴリズム。
- `scripts/check-prompt-injection.py:150-159` の `line_is_defended` — 見出し (`nearest_heading`) と近傍行の
  `DEFENSE_MARKERS` マッチのみで判定し、主観評価・LLM 推論は一切呼んでいない。

これは同じ「pre-commit / CI で駆動される静的解析＋receiver-aware な偽陽性抑制」という設計の
`check-fail-open.py`（実装正しさに分類済み、`scripts/check-fail-open.py:31-34`
「the patterns are matched RECEIVER-AWARE, not by a bare keyword」）と手法的に同一である。
両者とも「曖昧さの余地が小さい」機械的判定であり、旧分類理由の「複合判定」という言葉が実装の
決定論的な性質を holistic 側に誤誘導していた（誤診断であって、実際に LLM 判断や曖昧な総合基準を
呼んでいるわけではない）。

**是正**: 上記「ゲート一覧」の該当行を「実装正しさ（是正済み。旧: 総合判断）」に変更し、分類理由を
実装の逐語引用に基づいて書き換えた。**コード自体（`scripts/check-prompt-injection.py`）は変更していない**
（判定ロジックそのものは元から正しく決定論的だったため、直すべきは taxonomy 側の記述のみ）。

### 監査したが混在なしと判断したゲート

- **donegate**（`crates/donegate/src/gate.rs`）— `GateReport::verdict`
  (`crates/donegate/src/gate.rs:62-69`) は `blocking()` の exit code 判定のみで `Verdict` を構成する。
  LLM 判断・曖昧基準は無し。実装正しさとして妥当。
- **tdd**（`crates/tdd/src/gate.rs`）— `classify`/`Report::verdict`
  (`crates/tdd/src/gate.rs:60-77`, `112-170`) は git diff のスキャン結果と正規表現/グロブ照合のみで
  ブロック判定する。テスト適合性として妥当。
- **mutategate**（`crates/mutategate/src/lib.rs`）— `evaluate`
  (`crates/mutategate/src/lib.rs:212-278`) は `cargo-mutants` の `outcomes.json` から算出した
  kill-rate と閾値の数値比較のみ。テスト適合性として妥当。
- **budgetguard**（`crates/budgetguard/src/gate.rs`）— `verdict`
  (`crates/budgetguard/src/gate.rs:209-246`) はコスト USD と閾値の数値比較のみ。実装正しさとして妥当。
- **propguard**（`crates/propguard/src/gate.rs`, `derive.rs`）— 導出 (`derive_properties`) と
  閾値判定 (`below_threshold`, `crates/propguard/src/gate.rs:44-49`) はいずれも決定論的だが、各
  プロパティが実際に「成り立つか」の判定自体は inject/subprocess の総合評価に委譲されている
  （`crates/propguard/src/derive.rs:11-17` が「Honest ceiling」として明文化済み）。この委譲構造こそが
  「総合判断」分類の根拠であり、混在ではなく設計として正しい記述。
- **reviewgate**（`crates/reviewgate/src/main.rs`, `review.rs`）— inject モードは running agent の
  自己レビュー、subprocess モードは独立レビュアー subprocess の自由記述判定に依存する
  (`crates/reviewgate/src/main.rs:1-24` のモジュール doc)。閾値・カウントに還元できない複合判断であり、
  総合判断として妥当。
- **precommit-audit**（`crates/precommit-audit/src/checks/*.rs`）— 個々のチェック（`check_hardcoded_ip`
  `crates/precommit-audit/src/checks/mod.rs:149-173`、`check_hardcoded_secret` 同 175-200、
  `check_swallowed_error` 同 202-226 等）はすべて正規表現・grep・exit code ベースの静的解析であり、
  `checks/review.rs` の subagent review contract (`crates/precommit-audit/src/checks/review.rs:16-95`)
  もハッシュ照合（`diff_hash` が計算した SHA-256 と `<review_path>` に書かれた値の一致確認）のみで、
  precommit-audit 自身は LLM 判断を一切行わない（LLM 判断は `/precommit` コマンド側の責務であり、
  precommit-audit はその成果物の存在・一致を機械的に検査するだけ）。したがって個々のルールは
  すべて機械的。下記「グレーゾーン設計」節の理由により、束ねて total verdict とする設計自体は
  混在とみなさずリファクタリングしない。
- スクリプトゲート（`check-doc-claims.py`, `check-test-weakening.py`, `check-plugin-versions.py`,
  `check-version-bumped.py`, `check-bin-reproducibility.py`, `check-bench-regression.py`,
  `check-gate-crates-sync.py`, `check-plugin-rollout.py`, `check-ci-red.py`, `check-fail-open.py`）
  — いずれも実装を確認し、文字列/バージョン比較・path:line 照合・数値閾値・run 履歴カウントなど
  決定論的な判定のみで構成されていることを確認した。分類（実装正しさ or テスト適合性）と実装は一致。

### グレーゾーンとして是正しない設計（理由の明記のみ）

- **precommit-audit**（総合判断）— 個々のルールはいずれも機械的（正規表現・grep・exit code）だが、
  10種以上の独立した検査（missing-test / hardcoded-ip / hardcoded-secret / swallowed-error /
  duplicate-function / local-capture / markdown-links / line-endings / file-length / custom-rules /
  linters / subagent-review-contract、`crates/precommit-audit/src/checks/mod.rs:604-641` の
  `run_static_checks` が束ねる一覧）を1回の diff に対して総合し、「この diff は clean か」という
  単一の複合判定へ集約する。個々のルールを分解して「実装正しさ」に再分類しても、ゲートとしての
  振る舞い（複数の独立した機械的シグナルを1つの block/allow に統合するオーケストレーション自体）は
  総合判断的性質を持ち続けるため、リファクタリングの実益が薄い。このグレーゾーンはタスクの
  是正基準が明示的に許容する設計判断として維持する。
- **budgetguard**（実装正しさ）— `session_cost` (`crates/budgetguard/src/gate.rs:158-178`) は
  gauge の `SessionRecord` キャッシュと transcript 再パースという複数のコスト情報源を
  fresh 判定 (`record_is_fresh`, 同 186-198) で選択し、`Determination<Option<f64>>` の三値
  （測定済み/計測対象なし/判定不能）に正規化してから閾値比較する、という点で「複合的」ではあるが、
  各分岐は if/else の決定論的ロジックであり LLM 判断・曖昧基準は一切登場しない。「機械的だが
  複数のソース/分岐を束ねている」というグレーゾーンはタスクの是正基準に照らして混在とみなさない。

## 関連ドキュメント

- [GLOSSARY.md](GLOSSARY.md) — 用語・クレート早見表
- [OVERVIEW.md](OVERVIEW.md)「検証ゲート（Stop フック群）」節 — 各 Stop フックゲートの詳細挙動
