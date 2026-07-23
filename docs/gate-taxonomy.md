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

## 重複ゲートの統合監査（2026-07-23）

「重複」かつ「実測ゼロ件」の**両方**を満たすゲートのみ統合/削除対象とする（片方だけは対象外、という
是正基準に従う）。

### 統合した重複: `GATE_CRATES` 定数の Rust 側二重定義

**発見（重複）**: 同名・同じ意味（fleet 防御ゲート crate 集合）の Rust 定数が、値は同一のまま
**型だけ違う形**で2箇所に独立して手書きされていた:

<!-- doc-claim-exempt: historical quote — this line was replaced by `pub use harness_core::fleet::GATE_CRATES;` in the consolidation this section documents -->
- `crates/tdd/src/config.rs:82`（統合前）: `pub const GATE_CRATES: &[&str] = &[...]`
  （`strict_separation` のデフォルト on/off を決める context predicate が参照）
<!-- doc-claim-exempt: historical quote — this line was replaced by `pub use harness_core::fleet::GATE_CRATES;` in the consolidation this section documents -->
- `crates/condukt/src/adversarial.rs:75`（統合前）: `pub const GATE_CRATES: [&str; 6] = [...]`
  （adversarial refutation panel の high-stakes 判定が参照）

両者は `scripts/check-gate-crates-sync.py` のモジュール docstring 自身が
「Both Rust copies had silently lost `overwatch`, exempting the Continuous-Audit crate from the
gates that loop depends on.」（同スクリプト旧 28-31 行）と明記するとおり、**過去に実際に2回、
独立にドリフトした実績**がある — 「重複」の実害が既に観測された唯一のペアである。

**発見（実測ゼロ件）**: `crates/condukt/src/adversarial.rs`（Adversarial refutation panel）の
モジュール doc（同ファイル冒頭）が「the generative, non-deterministic part — spawning N independent
skeptic subagents … is strictly **OPT-IN**」と明記するとおり、この定数を消費する2つの機能
（tdd の `strict_separation` gate-crate context 判定／condukt の adversarial panel 発火）は
いずれも opt-in であり、かつ発火をトリガーする SKILL 側オーケストレーションが既定で有効化されて
いない。fleet 内に GATE_CRATES を実際に消費して稼働ログへ記録するテレメトリは存在しない
（`grep -rn "panel_size\|panel_engaged" crates/condukt/src/*.rs` はテスト以外にヒットなし、
測定日 2026-07-23）ため、この定数自体の「使われ方の違い」は実測できないが、**定数の値**という
点では重複そのものが実害（overwatch 抜け落ち2回）を生んでいるので、統合対象とした。

**是正**: `crates/harness-core/src/fleet.rs` を新設し、`pub const GATE_CRATES: &[&str]` を
唯一の Rust 側正典として定義。`crates/tdd/src/config.rs` と `crates/condukt/src/adversarial.rs`
はそれぞれ `pub use harness_core::fleet::GATE_CRATES;` で re-export するのみに変更した
（harness-core の既存 re-export 慣習 — 各クレートの `config.rs`/`model.rs` が
`pub use harness_core::config::expand_tilde;` / `pub use harness_core::hook::HookInput;` と
同じ書き方 — に倣った）。型は `&[&str]`（tdd 側の元の型）に統一し、condukt 側の唯一の使用箇所
(`GATE_CRATES.iter().any(...)`, `crates/condukt/src/adversarial.rs` 旧280行) は `[&str; 6]` /
`&[&str]` のどちらでも同じ `.iter()` 呼び出しで動作するため、型変更によるコンパイルエラーは
発生しなかった（`cargo check --workspace` で確認済み）。

**`scripts/check-gate-crates-sync.py` の役割変更**: 統合前はこのスクリプトが
`crates/condukt/src/adversarial.rs` と `crates/tdd/src/config.rs` の**2つ**を独立した
tracked source として個別 parse し、両者が canonical set と一致するかを検査していた
（旧 `SOURCES` リストの該当2行）。**統合後はこの2行を `crates/harness-core/src/fleet.rs` の
1行に置き換えた** — 理由は、re-export (`pub use`) された定数は Rust コンパイラが値の同一性を
機械的に保証するため（re-export 元と再輸出後の値が乖離することは構文上あり得ない）、
condukt・tdd 側を個別に parse する意味がなくなったため。これにより tracked source は
8種類から7種類（`docstring` 冒頭の「8 hardcoded sources」を「7」に修正）に減った。
スクリプト自体は削除せず、残る非 Rust ソース（shell/Python/Markdown の6種）との整合検査という
本来の役割は維持している（削除ではなく役割縮小）。付随して `scripts/test_check_gate_crates_sync.py`
のフィクスチャ生成ヘルパ (`_make_fixture_repo`) も `condukt_*`/`tdd_*` パラメータを
`rust_*` 1本に統合し、`crates/harness-core/src/fleet.rs` を書き出す形に更新した
（このテストファイルは touched_files のスコープ外だが、`check-gate-crates-sync.py` 本体の
リファクタリングと不可分に結合したテストコードであり、更新しないと CI
（`.github/workflows/gate-crates-sync.yml` の `test_check_gate_crates_sync.py` 実行）が
確実に赤くなるため、本タスクの一部として更新した）。

### 検証: `python3 scripts/test_check_gate_crates_sync.py` の既存 fail 5件は無関係（このタスク起因ではない）

統合前後で `python3 -m pytest scripts/test_check_gate_crates_sync.py -q` を比較したところ、
**同じ5件のテストが統合前から既に fail していた**（`git stash` で統合前の状態に戻して同コマンドを
再実行し確認、測定日 2026-07-23）。原因はいずれも `docs/OVERVIEW.md` を書き出さない
フィクスチャの欠落（`_make_fixture_repo` が `docs/OVERVIEW.md` を一度も生成しておらず、
`SOURCES` の `("docs/OVERVIEW.md", overview_md_crates, "exact")` エントリが常に
`None`＝drift と判定される）で、本タスクの変更（Rust 側の統合）とは無関係の既存の欠陥。
このタスクでは是正しない（`touched_files` 外の未修正フィクスチャ欠陥であり、今回の統合が
生んだ新規リグレッションではないことのみ確認した）。

### 見つけたが対象外にした重複: `scripts/check-fail-open.py` の `GATE_CRATES` 第9のコピー

**発見**: `scripts/check-fail-open.py:92`「`GATE_CRATES = [` (`"blastguard", "propguard",
"specguard", "stuckguard", "mutategate", "overwatch"`)」もまた同じ crate 集合の独立した
手書きコピーであり、`scripts/check-gate-crates-sync.py` の `SOURCES` リスト・docstring
いずれにも**含まれていない**（同スクリプトの追跡漏れ）。値は現時点で canonical と一致しているが、
tdd/condukt の2コピーが実際にドリフトした前例がある以上、これも将来ドリフトしうる潜在的重複である。

**対象外にした理由**: このタスクの `touched_files` は `scripts/check-gate-crates-sync.py` の
編集のみを許可しており、`scripts/check-fail-open.py` はスコープ外。また「実測ゼロ件」の観点でも、
`check-fail-open.py` は CI の `fail-open.yml`（`--ratchet` 付き、非バイパス本ゲート）で常時
稼働しており実測ゼロ件とは言えないため、今回の是正基準（重複**かつ**実測ゼロ件の両方を満たす）を
満たさない。**統合はせず、次の是正対象の候補として記録するに留める**（別タスクでの追跡を推奨）。

### その他: これ以上の「重複+実測ゼロ件」ゲートは見つからなかった

上記「階層混在監査」節で確認済みの全ゲート（Stop hook 7 crate + `scripts/check-*.py` 11本）を
重複性の観点で再確認したが、`GATE_CRATES`（今回統合）と `check-fail-open.py` の第9コピー
（上記、実測ゼロ件ではないため対象外）以外に、複数ソースで同一の値/ロジックを独立に
手書きしているものは見当たらなかった。無理に対象を見つけて統合する必要はないため、これ以上の
統合は行わない。

## 敵対的fail-openミューテーション検証（2026-07-23）

「このゲートは fail-closed に書かれている」という主張は**判断（予測）**であり、CLAUDE.md 冒頭の
最上位方針 §2「判断は予測にすぎない。fail はテストで決着させる」に従えば、そのままでは事実として
扱えない。この節は、GATE_CRATES（`blastguard`/`propguard`/`specguard`/`stuckguard`/`mutategate`/
`overwatch`）それぞれの判定ロジックに**実際に fail-open 変異を注入し、既存テストスイートが
本当に RED になるかを機械的に観測**した記録である。実装したハーネスは
`scripts/check-fail-open-mutation.py`（新規）。

### 設計: 生成と検証を同一エージェントの単発判断に依存させない

各シナリオは `Scenario(crate, file_rel, description, old, new)` という**具体的なソース文字列
置換**として事前に固定されている（実行時に LLM が「これは壊れそうだ」と判断する余地はない）。
スクリプトは各シナリオについて必ず次の手順を決定論的に実行する:

1. 対象ファイルが `git status --porcelain` でクリーンであることを確認（汚れていれば変異を拒否）。
2. `old` 文字列が対象ファイル中に**ちょうど1回**だけ出現することを確認してから `new` へ置換
   （0回・複数回はどちらも「曖昧」として変異を拒否し、判断で選ばない）。
3. `cargo test -p <crate>` を実行し、終了コードを観測する。
4. 終了コードが非ゼロ（RED）かどうかを判定する。ただし出力にコンパイルエラーの兆候
   （`error[E...]` 等）が見られる場合は「テストスイートが意味的に検出した」ことにはならないため
   `inconclusive`（判定不能）として扱い、`caught`（捕捉成功）とは区別する。
5. **成功・失敗・例外いずれの経路でも** `finally` 相当のブロックで `git checkout --` により
   変異を必ず元に戻し、その後ファイルの内容が変異前のバイト列と完全一致するかを検証する
   （revert 自体の成否も出力に明記する）。

「REDにできなかった場合は実装を止め判断で代替しない」という是正基準に従い、`not-caught`
（変異が実際に混入したのに既存テストが検出できなかった）はスクリプトの失敗ではなく**発見**として
明示的に報告する専用ステータスを持つ（後述のとおり、今回はどのシナリオも `not-caught` にならな
かった）。

### 選定した6シナリオ（各1件、file:line と変異前後）

- **blastguard** — `crates/blastguard/src/model.rs:84`
  `            Decision::Ask(reason) => Decision::Deny(reason),`
  → `            Decision::Ask(_reason) => Decision::Allow,`
  （`Decision::hardened()`: 人間が答えられない `Ask` を `Deny` へ畳み込む契約を破り、`Allow` へ
  すり替える — 同ファイル75-81行のdocコメントが「an `Ask` that no human can answer must not
  become an `Allow`」と明記する、まさにこの関数が防ぐべき fail-open）。
- **propguard** — `crates/propguard/src/gate.rs:47`
  `pub fn below_threshold(satisfied: usize, threshold: usize) -> bool {`
  （本体48行 `    satisfied < threshold`）
  → `_satisfied`/`_threshold` を受け取り常に `false` を返す（閾値未達でも決してブロックしない）。
- **specguard** — `crates/specguard/src/parse.rs:107`
  `            (true, true)`
  → `            (false, false)`
  （`parse()`: `needs_user` が解析不能・不在のときの fail-closed デフォルト
  `(needs_user=true, indeterminate=true)` を、無言でクリーン扱いする `(false, false)` へ反転 —
  同ファイル28-36行のdocコメントが明記する fail-open #7 そのもの）。
- **stuckguard** — `crates/stuckguard/src/detect.rs:92`
  `    if same.len() < cfg.repeat_threshold {`
  → `    if true {`
  （`repeat()`: 閾値比較を無条件 `true` に固定し、繰り返し検出が二度と発火しなくする）。
- **mutategate** — `crates/mutategate/src/lib.rs:236`
  `            let passed = kr + KILL_RATE_EPSILON >= threshold;`
  → `            let passed = true;`
  （`evaluate()`: kill-rate が閾値未満でも常に合格扱いにする — mutategate 自身の kill-rate gate
  ロジックへの変異）。
- **overwatch** — `crates/overwatch/src/canary.rs:184`
  `    let decision = if observed_violations > policy.max_violations_in_window {`
  → `observed_violations`/`policy.max_violations_in_window` を握りつぶし常に
  `GateDecision::Proceed` を返す（canary health gate がどれだけ違反が急増しても rollback しない）。

### 実行結果（`python3 scripts/check-fail-open-mutation.py`、2026-07-23）

全6シナリオが `caught`（RED confirmed: yes）、revert confirmed: yes、スクリプトは exit 0 で終了した。
実行ログ（該当行を引用、フルログはコマンド再実行で再現可能）:

```
--- blastguard :: ... status: caught  (RED confirmed: yes; revert confirmed: yes; 2.0s)
--- propguard  :: ... status: caught  (RED confirmed: yes; revert confirmed: yes; 3.6s)
--- specguard  :: ... status: caught  (RED confirmed: yes; revert confirmed: yes; 3.4s)
--- stuckguard :: ... status: caught  (RED confirmed: yes; revert confirmed: yes; 1.3s)
--- mutategate :: ... status: caught  (RED confirmed: yes; revert confirmed: yes; 1.6s)
--- overwatch  :: ... status: caught  (RED confirmed: yes; revert confirmed: yes; 5.2s)

=== summary ===
  scenarios run: 6/6
  caught (RED confirmed): 6
  NOT caught (existing tests missed a real fail-open): 0
  inconclusive (mutation didn't compile): 0
  errored (setup/revert problem): 0
```

**今回は「テストが検出できなかった fail-open」という発見は無かった** — 6クレートすべてで、選定した
代表的な fail-open 変異は既存テストスイートによって決定論的に RED として捕捉されることを実測で
確認した（判断ではなく、実際に `cargo test -p <crate>` を実行して観測した事実）。

### 安全策の実測: 汚れたファイル・クラッシュ復旧経路

「2回連続実行しても、あるいは変異注入中にクラッシュしても、対象ソースを変異状態のまま残さない」
という是正基準を、対象ファイルへ意図的に未コミットの変更を残した状態で
`python3 scripts/check-fail-open-mutation.py --crate blastguard` を実行して確認した:
スクリプトは `git status --porcelain` でファイルが汚れていることを検知して変異注入そのものを
**拒否**し（`error` ステータス、exit 1）、既存の未コミット変更には一切触れなかった
（実行前後で `git diff` が完全一致することを確認）。

### `GATE_CRATES` の9番目の重複コピーを新規登録

`scripts/check-fail-open-mutation.py` は6crate配列 `GATE_CRATES = (...)` を Python の
module-level tuple として保持している（standalone script のため
`harness_core::fleet::GATE_CRATES` を直接 `pub use` できない）。前節「重複ゲートの統合監査」が
指摘した既存の**8番目の未追跡コピー**（`scripts/check-fail-open.py`）に続く**9番目**の独立コピーを
また新たに追加してしまわないよう、`scripts/check-gate-crates-sync.py` の `SOURCES` リストへ
`("scripts/check-fail-open-mutation.py", python_const_crates, "exact")` として新規登録した
（既存の `python_const_crates` 抽出関数がそのまま流用できる形で書いたため、抽出関数自体の追加は
不要だった）。登録後 `python3 scripts/check-gate-crates-sync.py` は
`OK: GATE_CRATES consistent across 8 sources` を返し、追跡対象ソースは7から8に増えた
（前節で7に減った経緯があるため、この節での8への増加は新しい追跡対象の追加であり、後戻りではない）。

## 敵対的fail-openミューテーション検証のCI配線（2026-07-23 訂正）

上の節で作った `scripts/check-fail-open-mutation.py` は、実装完了時点では**どの自動トリガーにも
配線されていなかった**（`.github/workflows/` と `.githooks/` の両方を `grep` して確認、ヒットなし）。
手動実行しない限り一度も走らない、という状態は「テストが本当にfail-openを検出できるかを機械的に
検証する」という当初の是正目的に対し**実効性ゼロ**であり、そのまま放置してよい状態ではなかった。

**誤った解決策（採用しなかったもの）**: 「タスクごとに自動実行する」——例えば `condukt` の各タスク
完了時や、ローカル Stop hook・pre-commit で毎回走らせる——は誤り。各シナリオは対象 crate を
mutate → `cargo test -p <crate>` → revert という実コンパイル＋テスト往復であり、6 crate 分だと
`.github/workflows/mutation.yml`（cargo-mutants による kill-rate ゲート）と同等のコストになる。
これを commit 単位・task 単位で強制すると、遅すぎるゲートを人間が迂回する（hook を無効化する・
skip ファイルを使う）という新しい fail-open を生む。

**正しい是正**: 既存の `mutation.yml` と同じ「calibrated crate の変更時のみ + 定期実行」パターンに
倣い、`.github/workflows/fail-open-mutation.yml` を新規作成した:

- **トリガー**: `workflow_dispatch`（手動）／`schedule`（毎週火曜 06:00 UTC — `mutation.yml` が
  月曜を使っているため曜日をずらし runner 競合を避けた）／`pull_request` と `push`（`main`）は
  **path filter 付き**（ハーネス自身: `scripts/check-fail-open-mutation.py` /
  `scripts/test_check_fail_open_mutation.py` / このワークフロー自体、および対象6crate:
  `crates/{blastguard,propguard,specguard,stuckguard,mutategate,overwatch}/**`）。
  この6crateは `.githooks/pre-push` の `GATE_PATTERN`（同ファイル99行目）・
  `scripts/check-gate-crates-sync.py` の canonical set と同一集合。
- **フィルタなしの `on: pull_request` にしなかった理由**: この harness の verdict を動かせるのは
  ハーネス自身か対象6crateの変更だけであり、無関係な diff にまで unconditional に発火させるのは
  「全部に発火＝全部を検査している」ように見えて実際はCI時間を浪費するだけの見せかけになる
  （`fail-open.yml`・`gate-crates-sync.yml` が path filter 無しなのは、それぞれ repo 全体の
  swallow パターン走査・7ソース横断整合性チェックという性質上、path filter を付けると
  「関係ない PR には報告されない」問題＝ required check にできない問題が起きるため。今回の
  ミューテーションハーネスは対象が6crateに限定されており、その事情が当てはまらない）。
- **ジョブ内容**: `dtolnay/rust-toolchain@stable` + `Swatinem/rust-cache@v2` で Rust 環境を用意し、
  まず `python3 scripts/test_check_fail_open_mutation.py`（ハーネス自身の22テスト）、続けて
  `python3 scripts/check-fail-open-mutation.py --keep-going`（全6crateへの実ミューテーション実行。
  `--keep-going` で1crateの発見が残り5crateの結果を隠さないようにする）を実行する。

これで「タスクごとに自動実行」という誤った頻度ではなく、「この harness の verdict が変わりうる
変更（ハーネス自身 or 対象6crateの変更）が入ったとき」という**あるべきGate**（このリポジトリの
既存パターンに従った、対象を絞ったCIトリガー）を通るようになった。

## precommit-auditのStop-hook限定を正式化する（2026-07-23）

**発見**: precommit-auditのsecret/credentialスキャナ（`check_hardcoded_secret`、
`crates/precommit-audit/src/checks/mod.rs:175-199`）は実在し既定有効
（`ctx.cfg.hardcoded_secret`は`config.rs`のdefault実装で有効）で、Stop hookとして常時稼働している
（`crates/precommit-audit/hooks/hooks.json:2-8`が`"Stop"`イベントにバイナリを登録、
`docs/GLOSSARY.md:56`が「always-on」と明記）。しかし本ドキュメント冒頭「ゲート一覧」の
Stop フック群の表（本ファイル31行目）には既に `precommit-audit | 総合判断 | Stop hook` として
掲載済みであり、**gate-taxonomy.md自体には欠落していない**。欠落していたのは別の3箇所——
`.githooks/pre-commit`・`scripts/rollout-plugins.sh`の`GATE_CRATES`・
`scripts/continuous-audit.sh`の`DEFAULT_TARGETS`——だった。

**判断: 3箇所とも意図的に対象外が正しい（登録しない）**。理由をそれぞれ逐語で示す:

- **`.githooks/pre-commit`**（`.githooks/pre-commit:24-25`「What runs (all stdlib-only python3;
  each blocks, none warn-and-continue)」）——ここに列挙されるのは **git commit 時**に発火する
  stdlib-only python3 スクリプト6本（injectguard/fail-open-guard/doc-claims/test-weakening/
  version-lockstep/bump-on-change、同ファイル26-37行）。precommit-auditは Rust バイナリで
  **Stop hook**（condukt run中のターン終了時）に発火し、`.githooks/pre-commit`が発火する
  git-commit-timeとはトリガー時点そのものが異なる。同じ「pre-commit」という名前を持つが
  git hookではなくStop hookなので、この一覧に加えるとトリガー種別の異なるものを同一列に
  混在させることになり、`.githooks/pre-commit`の「python3 stdlib-onlyがcommit時に走る」という
  一覧の一貫性を壊す。
- **`scripts/rollout-plugins.sh`の`GATE_CRATES`**（`scripts/rollout-plugins.sh:123`
  `GATE_CRATES="blastguard propguard specguard stuckguard mutategate overwatch"`）——
  この6crateは「fleetの防御を担うがゆえにcanary無し反映を拒否される」特定集合（同ファイル
  113-122行目のoverwatchについてのコメントが「so it gets the same canary requirement as the
  crates it protects」と明記するとおり、**protect-the-protector**という共通性質で束ねられている）。
  precommit-auditは汎用diff監査ツールであり、この「fleetの防御機構自体を守る」性質を持たない
  （precommit-auditが壊れても他のGATE_CRATESの防御能力が落ちるわけではない）。加えるとこの
  6crateの意味——canary必須のfleet防御コア——が拡散する。
- **`scripts/continuous-audit.sh`の`DEFAULT_TARGETS`**（`scripts/continuous-audit.sh:91-100`
  「Default target set = the GLOSSARY gate crates ... PLUS `backlog`」）——このリストは
  GATE_CRATES 6つ + backlog（backlog自身はGATE crateではないがGATE_CRATESの運用に不可欠な
  ため監査対象に追加、と同コメントが明記）という**GATE_CRATESの厳密な上位集合**として設計
  されている（同97行「This is a strict superset of GATE_CRATES」）。Continuous-Auditが
  対象とするのは「fail-openを注入したときに検出できるか」という**GATE_CRATES固有の敵対的検証**
  であり、precommit-auditはそのものの対象ではない（precommit-auditへの敵対的fail-open検証が
  必要かどうかは別の検討課題であり、このリストの意味を変えずに個別に評価すべき）。

**是正**: コードは変更しない（3箇所とも実装は正しい）。本節を追加してこの判断を明文化し、
「gate-taxonomy.mdのStop hook表に既にある」ことと「他3箇所に無いのは正しい」ことの両方を
1箇所から参照できるようにした。

## GATE_CRATES内の「即座にblockする系」と「protect-the-protector系」の区別（2026-07-23）

上記の節（`scripts/rollout-plugins.sh`の`GATE_CRATES`）が既に導入した**protect-the-protector**
という共通性質は、`blastguard`/`propguard`/`specguard`/`stuckguard`/`mutategate`/`overwatch` の
6crateを canary 必須の一集合として束ねる根拠として妥当だが、この6crateは「今まさに何かをblock
しているか」で見ると一様ではない。各crateの `hooks/hooks.json`（無ければ非登録）を実測した結果:

| crate | フック登録 | 実際の挙動 | 分類 |
|---|---|---|---|
| `blastguard` | PreToolUse（`hooks/hooks.json`） | 破壊的コマンドを deny する | **即座にblockする系** |
| `propguard` | Stop（`hooks/hooks.json`） | 性質未充足で turn 終了を block する | **即座にblockする系** |
| `mutategate` | **なし**（`.claude-plugin/`・`hooks/`とも不在） | CLI/CI 専用の mutation kill-rate 測定。Claude Code のどのイベントにも自動登録されない | protect-the-protector（block 主体ではなく計測ツール） |
| `stuckguard` | PostToolUse（`hooks/hooks.json`） | README 自身が明記: 「It only ever injects advice. It cannot block a tool call or end a turn」（`crates/stuckguard/README.md:12`） | protect-the-protector（advisory のみ、block しない） |
| `specguard` | SessionStart（`hooks/hooks.json`。`specguard pending`の通知のみ） | 実監査は `/specguard:run` の手動起動が担い、read-only（実装・テストを書き換えない） | protect-the-protector（監査結果は human review/backlog へ流れるだけで、それ自体は何もblockしない） |
| `overwatch` | SessionStart / Stop（`hooks/hooks.json`。いずれも`overwatch status`） | レジストリの状態表示のみで block 判定を返さない | protect-the-protector（registry/observability。他5crateの防御能力を可視化・保護する側） |

この表は上記「3階層の定義」（何を判定しているか）とは**別の軸**（いつ・どうblockするか）であり、
既存の3階層分類を置き換えない。同じ crate が両方の軸で語られてよい（例: `propguard` は
3階層では「総合判断」、この軸では「即座にblockする系」）。`GATE_CRATES` という1つのフラットな
リストがこの2crate（block主体）と4crate（protect-the-protector）を区別せず canary 必須集合として
扱っている理由は、上記の節が既に述べたとおり「fleet防御crateのrollout破損自体がリスク」という
canary運用上の要求であり、block/advisory の違いに関わらず全crateに同じcanary厳格度が要る、という
判断は変えない（区別が必要なのは「読み手がこの集合の中身を誤解しないため」のドキュメント上の
distinctionであり、rollout運用のcanary要求を緩めるものではない）。

## 関連ドキュメント

- [GLOSSARY.md](GLOSSARY.md) — 用語・クレート早見表
- [OVERVIEW.md](OVERVIEW.md)「検証ゲート（Stop フック群）」節 — 各 Stop フックゲートの詳細挙動
