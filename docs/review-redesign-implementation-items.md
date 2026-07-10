# Agentic-scale レビュー再設計 — 実装項目

> このドキュメントは別セッションでの実装着手を目的とした自己完結ブリーフ。元の議論の会話ログには
> アクセスできない前提で読めるように書いてある。

## 背景（なぜこのドキュメントがあるか）

Agentic Coding によってコード生産量が現在の数倍〜1日1万行規模になり、複数開発者が同時に
Agentic Coding を行う前提に立つと、**人間によるコードレビューは universal な正しさのゲートとして
構造的に破綻する**、という議論の結論がある。根拠は以下：

1. **diff だけでは不十分** — 呼び出し元・不変条件・並行アクセスパターンなど非局所的な文脈まで
   遡らないと正しさは判定できないが、それを人力で全変更に対しやるコストは非現実的。
2. **レビュアーは容易に増やせない** — 増員は文脈の分割不可能性ゆえに非一貫性を注入する。
3. **責任分担も安全ではない** — 分割した境界を跨ぐ不整合は誰の担当でもなくなる
   （seam の所有者不在問題）。
4. **人間は疲れる** — 実証研究（Cisco/SmartBear）では 1 時間あたり 200〜400 行を超えると欠陥検出率が
   落ち、セッションが 60〜90 分を超えると質が落ちる。量を増やすほど実効検出率はむしろ下がる。
5. **「安心のための儀式」は無レビューより悪い** — 形骸化したレビューは「保証されている」という
   偽の安心感を生み、他の防御を薄くする（security theater 問題）。

この結論から「人間の判断が必要な箇所を最小化し、それ以外は機械的検証に完全に委譲する」という
再設計方針が導かれ、以下の 12 項目のフレームワークに整理された。

## 12 項目フレームワーク

1. **機械的 invariant 層** — 型・契約・PBT・spec drift 検知・capability 境界・静的解析で
   「diff + 前後文脈」を機械が保証する。
2. **リスクスコアリング/トリアージ** — 変更を自動採点し、閾値を超えたものだけ人間に回す。
3. **spec/plan 層への前倒し** — コード生成前の意図合意を主戦場にし、そこにも同じトリアージを適用
   （precedented な spec は自動承認、novel なものだけ人間へ）。
4. **事後統計サンプリングによる較正** — 自動承認された変更を無作為抽出で事後監査し、実際の欠陥率を
   測定して自動ゲートの閾値にフィードバックする。
5. **実行時/blast-radius 検証** — マージ前の完全証明を諦め、canary・feature flag・異常検知・
   自動 rollback で事後実証的に検証する。
6. **fleet 単位の相関エラー検知** — 同じ agent/パターンが複数の変更に同種のミスを横展開するリスクを、
   個別レビューではなく fleet 全体の再発パターンとして検知する。
7. **人間レビュー発火時は質問を絞る** — 「全部見て」ではなく「この invariant X との矛盾疑い」など
   具体的な理由付きで回す。
8. **AI による網羅的テスト生成・test-first** — AI は組み合わせ的なテストケース列挙が得意。
   テストコードは逐一人間が追わない。
9. **mutation testing 等によるテストの「歯」の検証** — カバレッジ数値やテスト量ではなく、
   意図的にバグを注入してテストが実際に検知できるかを機械的に測る（reward hacking / 空虚な
   テストの排除）。
10. **テスト生成者と実装者の分離** — 同じ agent が誤った実装と、それを自己確認する誤ったテストの
    両方を書いてしまう循環（reward hacking）を避ける。
11. **リスク階層化された e2e 検証** — ビジネスクリティカルな「コア」フローは毎回実際に visual/
    データ照合まで行い、それ以外は自動 visual diff やサンプリングに委ねる。
12. **意識的な守備範囲の絞り込み** — 「無謬性」ではなく「コアがビジネス上問題なく動く」ことを
    明示的な目標として設定する。

## 現状監査（このリポジトリで既にカバーされている範囲）

general-purpose agent によるリポジトリ監査結果（2026-07-09 時点）。

### COVERED
- **1. 機械的 invariant 層** — `propguard`（done_criteria から 3-5 個の semantic invariant を
  導出し fail-closed）、`tdd`（RED→GREEN 暗号学的証明つき test-first 強制）、`specguard`
  （spec↔impl drift 監査）、`blastguard`（破壊的操作への capability 境界）。5 クレートで proptest
  稼働（`crates/specguard/proptest-regressions/scope.txt` に regression seed）。
  **未カバー: 静的 call-graph 解析は不在。**
- **7. 質問を絞った人間エスカレーション** — `propguard` の PROP 単位 PASS/FAIL、`specguard` の
  該当 invariant 名指し引用、`condukt gate check` の risk×reversibility 理由つき通知。
- **8. AI 網羅的テスト生成・test-first** — `tdd` の RED-before-GREEN、`specguard testaudit`
  （実装済み未実行テストの検出）。
- **9. Mutation testing** — `mutategate` が `cargo-mutants` をラップし killed/viable 比 0.80
  閾値で CI gate 化（`.github/workflows/mutation.yml`）。現状 `harness-core` 1 クレートのみで
  pilot 中、段階拡大方針あり。
- **12. コア限定の割り切り** — `propguard`（全体証明ではなく 3-5 個の property に意図的に絞る）、
  `mutategate`（workspace 全体は遅すぎるとして 1 クレートに明示的にスコープ限定、no-silent-
  lowering の拡大方針を文書化）。

### PARTIAL（ギャップあり）
- **2. リスクスコアリング** — `blastguard::classify` が `RiskAssessment{risk, reversible}` を
  出し `condukt gate check` / `fugu-router` の gated 除外に接続。ただし破壊的操作（rm -rf,
  force-push 等）の分類のみで、「認可/課金/公開 API 破壊」のような一般 diff レベルの意味的
  スコアリングは無い。
- **3. spec 先行レビューの段階的トリアージ** — `specguard:brief`（実装前 spec 提示）、`compass`
  （condukt 上流での goal/plan 検証）はあるが、`specguard require_ratification`（exit 5）は
  二値の ratify gate であり、precedented/novel を区別した段階的トリアージではない。
- **4. 事後サンプリング較正** — `benchkit` のベンチ回帰検知、`evalkit canary` の golden set
  pass-rate 差分は「集計品質の回帰検知」であり、無作為抽出による auto-approved 変更の事後監査＋
  閾値フィードバックのループではない。
- **10. テスト生成者と実装者の分離** — `condukt state verifier-model` は verifier model ≠
  worker model を明示的に保証（共有ブラインドスポット対策）しているが、これは実装検証者の分離
  であり test 著者の分離ではない。`tdd` の RED→GREEN は同一 agent/セッションが逐次実行する。
- **11. リスク階層化 e2e** — `trajectoryeval` + condukt Phase 6 で出力・経路の二重検証、
  `specguard spec-audit` で e2e テストの存在確認は可能。visual diff/perceptual hash/screenshot
  サンプリングは無く、「コアは毎回・それ以外はサンプリング」という階層化も無い。

### MISSING
- **5. 実行時/blast-radius 検証** — canary rollout・feature flag・本番異常検知・rollback は
  存在しない。このリポジトリはデプロイされたランタイムサービスを持たない dev ツール monorepo
  なので、そもそも適用対象が無い可能性が高い（下記「実装項目 G」参照、要検討事項として残す）。
- **6. fleet 相関エラー検知** — `overwatch` は「誰が何を実行中か」のリース管理であり、複数変更に
  またがる同種ミスの再発パターン検知ではない。該当機構なし。

## 実装項目（優先度つき）

### 【優先度: 高】A. 事後サンプリング較正ループ（フレームワーク項目 4）
- **狙い**: 自動ゲート（propguard/specguard/mutategate/blastguard）が実際にどれだけ効いているかを
  検証する手段が無い。閾値（mutategate の 0.80 など）が経験則のまま固定され、劣化を検知できない。
- **アプローチ案**: 自動承認のみで通過した変更を一定割合で無作為抽出し、より厳格な監査（例:
  specguard の高精度モード、または別の adversarial AI レビュアー）にかける。発見した見逃しを
  a) 新規 invariant 候補として propguard/specguard へ提案、b) 閾値調整案として（自動適用せず）
  人間の ratify 待ちに積む、という 2 経路にフィードバックする。
- **置き場所案**: 新規クレート（例: `auditsample`）、または `benchkit`/`evalkit` の拡張。
  既存の `evalkit canary` の pass-rate 差分ロジックを流用できる可能性あり。

### 【優先度: 高】B. fleet 単位の相関エラー検知（フレームワーク項目 6）
- **狙い**: 同じ agent/model/prompt パターンが複数タスクに同種のミスを横展開するリスクへの対処が
  現状ゼロ。個別 gate 違反は「一回限りの失敗」として扱われ、横断的な再発は見えない。
- **アプローチ案**: `overwatch`（project-wide execution/lease registry）を拡張し、gate 違反
  イベント（blastguard denial、propguard PROP failure、specguard drift finding、mutategate kill
  failure）を正規化した「signature」（どの invariant/property/パターンに違反したか）付きで記録。
  同一 signature が時間窓内に N 件以上・複数タスク/セッションにまたがって再発したら、個別失敗
  ではなく systemic issue としてエスカレーションする。
- **置き場所案**: `overwatch` の既存 project-wide registry インフラを拡張するのが自然。

### 【優先度: 高】C. テスト生成者と実装者の分離（フレームワーク項目 10）
- **狙い**: `condukt` には verifier-model ≠ worker-model という良い前例があるのに、`tdd` の
  RED→GREEN（Phase 2 テスト作成 → Phase 4 実装）は同一 agent 逐次実行のままで、循環
  （reward hacking）リスクが未対処。
- **アプローチ案**: `tdd` に condukt 同様の strict モードを追加し、Phase 2（テスト作成）と
  Phase 4（実装）の agent identity が同一の場合は拒否する設定を持たせる。または `specguard
  testaudit` 側で、同一コミット/セッションでテストファイルと実装ファイルの両方が変更され、
  分離マーカーが無い場合にフラグを立てる。
- **置き場所案**: `crates/tdd`（condukt の `state verifier-model` 実装を参考にする）。

### 【優先度: 中】D. 一般化されたリスクスコアリング（フレームワーク項目 2）
- **狙い**: `blastguard::classify` は破壊的コマンド分類のみ。認可/課金/公開 API 破壊のような
  diff の意味的リスクは未カバー。
- **アプローチ案**: 設定可能な「機微パス」glob（認証/決済/PII 関連ディレクトリなど）や公開
  シンボル変更検知を `RiskAssessment` に合流させ、既存の `condukt gate check` エスカレーション
  ロジックにそのまま乗せる。
- **置き場所案**: `blastguard` 拡張、または新規レイヤーとして `blastguard` の上に乗せる。

### 【優先度: 中】E. spec 層の段階的トリアージ（フレームワーク項目 3）
- **狙い**: `specguard require_ratification` は二値ゲート。precedented な spec も novel な spec
  も同じ扱いで人間の ratify 待ちになる。
- **アプローチ案**: 新規/変更 spec を既存の ratified spec コーパスと類似度比較（構造 diff や
  semantic 類似度）し、既存パターンに強く一致する場合は自動 ratify、逸脱が大きい場合のみ人間へ
  回す段階的ゲートに拡張する。
- **置き場所案**: `specguard` の `require_ratification` ロジック拡張。

### 【優先度: 中】F. リスク階層化 e2e 検証（フレームワーク項目 11）
- **狙い**: visual diff/perceptual hash/screenshot サンプリングが皆無。「コアは毎回・それ以外は
  サンプリング」という階層化が無い。
- **アプローチ案**: 設定駆動の「コア」フロー allowlist（ビジネスクリティカルな e2e シナリオ）を
  定義し、コアフローは毎回スクリーンショット/データスナップショットを取得して実際に diff
  （perceptual hash や構造化データ照合）する。それ以外は既存の `specguard spec-audit` の e2e
  カバレッジ存在チェックのみ、または低頻度サンプリングに留める。
- **置き場所案**: 新規クレート、または `trajectoryeval` の拡張。ギャップの中で最も新規実装量が
  大きい項目。

### 【優先度: 低・要検討】G. 実行時/blast-radius 検証（フレームワーク項目 5）
- **現状判断**: このリポジトリはデプロイされたランタイムサービスを持たない dev ツール monorepo
  なので、古典的な canary rollout/feature flag はそのままでは適用対象が無い可能性が高い。
- **検討の余地**: `scripts/rollout-plugins.sh`（plugin version を live cache へ展開する機構）が
  唯一の「デプロイ」に近い操作。ここに canary 的な段階展開（一部ハーネスにのみ先行反映し、
  gate 違反率が上振れたら自動ロールバック）を持ち込む余地があるかもしれない。実装着手前に
  ユーザーとスコープを確認すること。

## 実装時の注意（リポジトリ規約）

- 各クレートを 1 行でも変更したら、`Cargo.toml` / `plugin.json` / `marketplace.json` の
  version を必ず 3 ファイル同時に micro 以上 bump する（`python3 scripts/check-plugin-versions.py`
  と `python3 scripts/check-version-bumped.py` で機械チェック）。
- 稼働ハーネスへの反映は手動 `cp` 禁止。`scripts/rollout-plugins.sh` を使う。
- 新規/変更 crate は `cargo fmt` + `cargo clippy -p <crate> --all-targets` を green にすること。
- `docs/GLOSSARY.md` に新規クレートの一言説明を追記すること。

## 実装レビュー結果（2026-07-09、コミット範囲 `9c70015..HEAD`）

別セッションによる実装がmainにマージされた後、`/code-review`（high effort、8観点 finder × 1票
verify、recall優先）でレビューを実施。20件の候補のうち18件がCONFIRMED、1件PLAUSIBLE、1件REFUTED。
severity順に記載する。**このセクションは次の実装セッションへの修正 TODO として扱ってよい。**

> **✅ 実装完了（2026-07-10、コミット `deed6d4..114f1ae`）** — 下記 CONFIRMED/PLAUSIBLE 17件
> （1〜17）を全て修正済み。REFUTED 1件（harness-core `jaccard` 共有化）は対象外のまま。クレート単位で
> 束ねて condukt 直列実行し、各件を別モデル verifier で検証（finding 1 の polarity swap 検出・finding 5 の
> near-repeat key 安定性・finding 10 の overwatch guard 冗長性は独立に反証テストで裏取り）。触った6クレート
> （specguard 0.2.18 / propguard 0.1.4 / stuckguard 0.1.9 / blastguard 0.1.7 / tdd 0.1.8 /
> trajectoryeval 0.1.6）は version を3ファイル lockstep bump。rollout-plugins.sh（3,4,14,17）は source-only
> 修正（実デプロイ未実行）。finding 8 は condukt 本体への配線をスコープ外とし、コメント＋単体テストで
> 「現実的な到達点」を固定。全 crate で fmt/clippy/test green、version 整合ゲート exit 0。

### 最優先（正しさ・セキュリティ）

1. **【CRITICAL】specguard の graded ratification gate が2箇所同時swapでバイパスされる**
   `crates/specguard/src/similarity.rs:150` — `polarity_signature()` はテキスト全体を1つの
   flat multiset に集約するため、2つの節（例: 「Xをhuman reviewなしでallow」「Yをhuman review
   なしでdeny」）の間で allow/deny を入れ替えると、バケットごとの出現数が変化せず
   `polarity_preserved()` が `true` を返し続ける。検証エージェントが実際にこのケースを
   Python で再現し、`jaccard(ratified, flipped) = 1.0`（44/44 shingle完全一致）を確認、
   既定閾値 0.85 を優に超えて `Verdict::Precedented`（人間レビュー省略）に分類されることを
   確定した。既存の adversarial テスト（`zzz_adversarial_probe_polarity_flips_route_to_human`
   等）は単一トークンの flip しかカバーしておらず、二箇所同時swapは無防備。
   **これは項目3（spec層トリアージ）の根幹——「novelなspecは人間へ」という保証そのものを
   破る欠陥なので最優先で塞ぐこと。**

2. **【HIGH】propguard の統合テストが不正JSONにより実ホームディレクトリを汚染する**
   `crates/propguard/tests/integration.rs:195`（他5箇所）— `format!(r#"{{"cwd":"{}"}}"#,
   home.display())` がWindowsのバックスラッシュパスをエスケープせずJSON文字列へ埋め込み、
   `HookInput::parse` が不正JSONとして`None`を返す→セッションキーが共有`_local`バケットに
   collapse。さらに `harness_core::config::home()` がWindowsではHOME環境変数を無視し
   `SHGetKnownFolderPath` で実ホームを解決するため、テストの `HOME` オーバーライドが効かない。
   検証エージェントが実際に `cargo test -p propguard --test integration` を実行し
   3/10テストが失敗（`propguard: allow (already-verified)`）、実ホーム配下に
   `~/.propguard/state/sessions/_local.json` 等の残留ファイルが生成されることまで確認済み。

3. **【HIGH】rollout-plugins.sh の canary JSON構築がWindowsパスで壊れる**
   `scripts/rollout-plugins.sh:429`（と490-491, 496）— `os.path.join` の結果（Windows上では
   バックスラッシュ区切り）をエスケープなしでJSON文字列リテラルへ直接展開。検証エージェントが
   このマシン上で実際に `json.decoder.JSONDecodeError` を再現。`set -euo pipefail` 下で
   ガードなしに呼ばれるため、rollback処理中にこれが起きるとcanaryバージョンが live のまま
   rollbackが中断される。

4. **【HIGH】`--canary`（非dry-run）が rebuild/sync を一度も呼ばずexitする**
   `scripts/rollout-plugins.sh:526` — `run_canary()` はコピー＋registry repointのみ行い、
   `rebuild-plugins.sh`/`sync-plugin-assets.sh` を一切呼ばずに `exit 0` する。`--no-rebuild`/
   `--no-sync` フラグはパースされるがcanary経路では到達しないコードにしか効かず実質no-op。
   結果、canaryロールアウトは「ソースを編集してversionを上げてcanary実行」しても
   実行中ハーネスは古いバイナリ（`crates/<name>/bin/` にcommit済みの古いもの）を指し続ける。
   `canary-dryrun.sh`/`canary-rollback.sh` いずれも「成功して全stage完走するcanary」を
   検証しておらず、テストで検出できない穴になっている。

5. **【HIGH】stuckguard の near-repeat エスカレーションが実質発火しない**
   `crates/stuckguard/src/detect.rs:106` — `Trip::key` が `format!("repeat:{}", cur.sig)` と
   「直近イベント自身の signature」になっているが、near-repeat（`similarity_threshold < 1.0`）
   は定義上毎回異なる `sig` を生成するため `record_nudge` が毎回新規keyとして`count=1`に戻り、
   `escalate_after` 到達（エスカレーション）に絶対到達しない。設計ドキュメント項目6
   （fleet相関エラー検知）の前段として追加された類似度ベース検知が、実装上は無限に
   nudgeするだけで一度もエスカレートしない状態。

6. **【MEDIUM】blastguard の `pub(crate)` シンボル検出が壊れている**
   `crates/blastguard/src/diffrisk.rs:135` — 検出ニードルが `format!("pub(crate) {m}")`
   （`m`は既に`"pub fn "`等キーワード込み）となり `"pub(crate) pub fn "` という実コードに
   存在しない文字列を生成。実際のソース `"pub(crate) fn "` にはマッチせず、
   `pub(crate)` 修飾された公開API変更のリスクスコアが常に過小評価される。`pub(crate)`を
   対象とするテストは1件も存在しない。

7. **【MEDIUM】specguard の ratification lock が非atomic書き込み**
   `crates/specguard/src/ratify.rs:103`（`write_lock`）— 今回のdiffでcorpusペイロードが
   肥大化したにもかかわらず、`harness_core::store::save_bytes`（atomic tmp+rename、既に
   5箇所で使用実績あり）を使わずplain `std::fs::write` のまま。クラッシュ・並行読み取り時に
   破損した lock ファイルを生成しうる。specguardは既にharness-core依存かつ
   `ratify.rs`自身が`harness_core::hash::fnv1a64`をimport済みなので、置き換えコストは低い。

8. **【MEDIUM】項目D（一般化リスクスコアリング）の公開API検知が本番経路で到達不能**
   `crates/blastguard/src/diffrisk.rs:107` — `classify_diff`の公開シンボル検知は実装・
   単体テスト済みだが、ワークスペース内の本番呼び出し元は`condukt::gate_exec::
   gather_assessment`と`condukt::schedule::schedule`の2箇所のみで、両方とも
   `diff_text=""`（空文字列）を渡している。コード中のコメントで「事前段階ではdiffが
   存在しないため意図的にスコープ外」と明記された既知の制限だが、公開API破壊の
   自動検知という項目Dの目標は実質未達成のまま。

9. **【LOW-MEDIUM】tdd の strict_separation エラーメッセージが誤誘導**
   `crates/tdd/src/proof.rs:266` — `strict_separation`の識別子チェックが「REDプロダクト
   自体が存在するか」のチェックより先に走るため、`tdd red`未実行のまま`tdd green
   --author X`を叩くと、正しい「RED proofが見つからない」ではなく「identity is missing」
   という誤ったエラーが出る（`--author`は正しく渡しているにもかかわらず）。

### 中優先（reuse / simplification / efficiency）

10. **propguard の Stop hook がproperty数分だけ冗長なファイルI/Oを行う**
    `crates/propguard/src/main.rs:257` — `emit_overwatch_violations`が3-5個のpropertyごとに
    `overwatch::store::append_violation`を呼び、各呼び出しが独立に`.git`探索の
    ディレクトリツリー走査・`canonicalize`・ファイルopen/close をやり直す。同期的にブロックする
    Stop hookパス上の冗長なレイテンシ。

11. **trajectoryeval が harness-core の共有FNV-1aを再実装**
    `crates/trajectoryeval/src/tier.rs:242` — `harness_core::hash::fnv1a64`
    （「唯一の正典」と明記されたpublic関数）と定数まで同一のprivate `fn fnv1a`を独自実装。
    trajectoryevalはharness-core未依存なので、依存追加が必要。

12. **specguard の Corpus/TemplateHashes/TemplateTexts が同一形状で3重定義**
    `crates/specguard/src/ratify.rs:61,75,86` — 4フィールド（audit/decisions/refute/
    completeness）が全く同じ形の構造体3つ。新規テンプレート追加のたびに3箇所編集が必要。

13. **specguard の「非activeゲートをマスクする」ロジックが3箇所に重複**
    `crates/specguard/src/main.rs:766,1588,1597` — 同一のmasking規則が3回書かれている。
    共有ヘルパー化はリスクが低い（各所で挙動差なしを確認済み）。

14. **rollout-plugins.sh のJSON構築ロジックが2箇所で重複**
    `scripts/rollout-plugins.sh:414` と `~485-493` — canaryの通常経路とrollback経路で
    ほぼ同一のJSON構築コードが重複。

15. **stuckguard の `record_lesson` に未使用の `count` パラメータ**
    `crates/stuckguard/src/main.rs:289,347` — 「将来のescalation tier用」とコメントされて
    いるが、`docs/`のどこにもこの計画は存在せず、導入コミット時点から投機的な死んだコード。

16. **stuckguard が既定設定（similarity_threshold=1.0）でも毎回tokenizeする**
    `crates/stuckguard/src/sig.rs:130` — near-repeat機能が既定オフでも、全tool-callイベント
    に対し無条件でtokenize + state fileへの永続化コストが発生。

17. **rollout-plugins.sh のcanary経路がregistry_patchをplugin数分呼ぶ**
    `scripts/rollout-plugins.sh:398` — 通常経路は全plugin分をバッチして1回呼ぶのに対し、
    canary経路はplugin毎に python3 subprocess を spawn（rollback時も同様）。severityは中程度
    （手動・低頻度操作のため）。

### 検証の結果REFUTEDだった候補

- stuckguard/detect.rs と specguard/similarity.rs がそれぞれ独自に `jaccard()` を実装している
  件（harness_core::lessons由来の3重実装、との指摘）— harness-core側の`jaccard`が**非pub**関数
  であり実際には外部から呼び出せないため、「既存流用を怠った」という主張自体は成立しない。
  ただし将来的に harness-core 側を `pub` にして共有化する価値はある（低優先）。

### CLAUDE.md規約・削除された保証の監査

- バージョン整合性（3ファイルlockstep）: 39プラグイン全て整合、違反なし
  （`check-plugin-versions.py`/`check-version-bumped.py` ともにexit 0）。
- specguardの二値ratifyゲート、tddのRED→GREEN保証、blastguard/conduktのrisk gateマージは
  いずれも弱体化なし（除去された保証の監査で違反0件）。
- item B/C/D/E（fleet相関エラー検知/テスト-実装分離/リスクスコアリング一般化/spec段階
  トリアージ）は、doc が指示した既存機構（overwatch registry / condukt verifier-model /
  blastguard RiskAssessment / specguard require_ratification）へ正しく接続されており、
  disconnectedな並行実装にはなっていない（唯一の例外が上記8番の項目D半完成）。

## 再レビュー結果（2026-07-10、コミット範囲 66eb9fb..HEAD — 上記1-17番修正コミットの検証）

上記の修正コミット（deed6d4, 5c4fa53, ac6b256, 197f466, a364c07, aee08ba, 114f1ae, db37289）を
1件ずつ検証。実際に `cargo test` / シェルスクリプトを実行して再現・反証した。**8/8 finder + 6/6
verifier 完了、7件がCONFIRMED（PLAUSIBLE/REFUTEDなし）。** 大半（12/17項目）は正しく修正された
ことを確認したが、4件は「修正が不完全」または「修正自体が新たな退行を生んだ」ことが判明した。

### 最優先 — ゲートが実質バイパスされたまま、または新規退行

1. **【重大】specguard polarityスワップ・バイパスは未クローズ（finding 1の修正が不十分）**
   `crates/specguard/src/similarity.rs:150` — `POLARITY_AXES`によるper-axis順序トラッキングに
   変更した`deed6d4`の修正は、同一axis内でのトークン位置スワップしか検知できていない。
   一時テストを追加し `cargo test -p specguard --bin specguard` で実際に再現・確認した2系統の
   バイパスが依然として通る:
   - (a) 動詞は元のクローズ位置に残したまま目的語フレーズだけをallow/denyクローズ間でスワップ
     → `sim=0.875`, `polarity_preserved=true`, `Verdict::Precedented`（人間レビュー不要のまま
     自動ratify）
   - (b) 出現数が各1回のmodal-axisトークン（例: `require`）とauthz-axisトークン（例: `forbid`）
     を異なるクローズ間でスワップ → `sim=0.909`, `polarity_preserved=true`,
     `Verdict::Precedented`
   
   根本原因は変わらず: `polarity_signature`はbucket/axisの識別と順序を追うだけで、トークンが
   どのクローズ・目的語を修飾しているかへの束縛が一切ない。finding 1と同じクラスのゲートバイパス
   が、別の具体的構成で依然として成立する。

2. **【高】specguard の atomic-write修正（finding 7）が新規退行を導入 — 上書き失敗が無音化**
   `crates/specguard/src/ratify.rs:168` — `write_lock`が`harness_core::store::save_bytes`
   （`Result`を返さずfail-softに内部エラーを`let _ = ...`で握り潰す）を使うよう変更され、
   成功判定を`path.exists()`のみに依存するようになった。既存のロックファイルを**再ratify**する
   （＝一般的なケース）際に`save_bytes`が権限エラー・ディスク満杯等で書き込みに失敗しても、
   古いファイルがそのまま残っているため`path.exists()`はtrueを返し続け、`write_lock`は
   成功を報告する。修正前は`std::fs::write(...).with_context(...)?`で実I/Oエラーを上書き時も
   含めて伝播していたため、これは修正前より悪化した挙動退行。

3. **【高】stuckguard near-repeatエスカレーション（finding 5）はwindow境界を超えると原バグに回帰**
   `crates/stuckguard/src/detect.rs:117` — `Trip::key`を`same.first()`（現ウィンドウ内で
   最古の一致イベント）由来に変える修正は、そのアンカーイベントがウィンドウ内に留まっている
   間だけ安定する。`SessionState::push`（`crates/stuckguard/src/state.rs:53-65`）はウィンドウ
   （既定`window=12`）が埋まると最古イベントから追い出す。near-repeat系列がwindow長を超えて
   継続する場合（＝stuckguard本来の検知対象である「長時間スタックしたループ」そのもの）、
   push毎にアンカーが追い出され、`same.first()`が別sigのイベントへローリングし続けnudge countが
   毎回1にリセットされる。到達可能な最大nudge count ≈ `window - repeat_threshold + 1`（既定値で
   10）で、`escalate_after`に到達できないまま長いループの残り全体で検知不能になる。同梱の回帰
   テストはevictionが起きない5イベントしか push しないため、この境界条件を捕捉していない。

4. **【中】blastguard item D（finding 8）は実際には未修正 — コミット履歴が誤解を招く**
   `crates/blastguard/src/diffrisk.rs:107` — public-API risk signalが本番で到達不能な問題
   （condukt側2箇所が空`diff_text`しか渡さない）について、`197f466`はユニットテストと
   「out of scope」と明記したdocコメントを追加しただけで、本番呼び出し側は無修正のまま。
   `crates/condukt/src/gate_exec.rs:83-88`と`crates/condukt/src/schedule.rs:285`は現在も
   両方とも文字通り`""`を渡しており、diffで変更なしを確認済み。コミット一覧だけを見ると
   finding 8が解決済みに見えるが、実際にはpublic-APIリスクシグナルは本番経路で恒久的に
   デッドのまま。

### 軽微 — 新規重複・デッドコード（修正自体は正しく機能する）

5. **tdd: `green()`の新規チェックが`judge_green()`内の既存チェックと重複**
   `crates/tdd/src/proof.rs:274` — finding 9の修正（strict_separation前の`has_red`チェック
   追加）は正しく機能するが、`judge_green()`（52-55行）内の同一チェックが唯一の本番経路
   （291行から常に`has_red=true`後にのみ到達）でデッドコード化した。

6. **propguard: `emit_overwatch_violations`が`overwatch::store::append_violation`をインライン
   再実装**
   `crates/propguard/src/main.rs:284` — finding 10のI/O削減修正自体は正しいが、
   `append_violation`の書き込みシーケンスをprivateにインライン複製している。省略された
   `signature_is_bucketable`ガードは上流の`normalize_signature`が既に保証しているため安全と
   確認済み（正しさの問題ではなく保守性の問題）。

7. **stuckguard: near-repeatキー導出のfallback節がデッドコード**
   `crates/stuckguard/src/detect.rs:120` — `.unwrap_or(cur.sig.as_str())`は、`repeat_threshold`
   が`Config::load`で最小2にクランプされ、かつ`same.len() < repeat_threshold`で早期returnする
   ため到達不能。挙動リスクはないが、将来の読み手が実在するエッジケースの処理と誤読する
   おそれがある。

## 再レビュー最優先3件の検証テスト（2026-07-10、コミット未push）

上記の最優先3件（specguard polarityバイパス finding 1、specguard 上書き失敗の無音化 finding 2、
stuckguard window境界退行 finding 3）について、実際にバグを再現する回帰テストを追加した。いずれも
`#[ignore = "known bug: ... 参照"]`付き（CIの通常`cargo test`はスキップ、`-- --ignored`で明示実行）。
`cargo test -- --ignored <test名>`で実行すると**現時点で実際にFAILする**ことを確認済み（つまり
バグの実在を機械的に証明している）。将来の修正時は、修正後にこれらのテストが緑になったことを
確認したうえで`#[ignore]`属性を外し、恒久的な回帰テストへ昇格させること。

1. **`crates/specguard/src/similarity.rs`
   `zzz_adversarial_probe_object_phrase_swap_still_bypasses`**（finding 1(a) — 目的語スワップ）
   — 動詞（allow/deny）はクローズ内の元位置に残したまま目的語フレーズだけを交換。
   `sim>=0.85`かつ`polarity_preserved`がまだ`true`を返すこと（=バグ）を検証。実行結果:
   `!polarity_preserved`のassertで実際にFAIL、バグを再現。
2. **同ファイル `zzz_adversarial_probe_cross_axis_single_occurrence_swap_still_bypasses`**
   （finding 1(b) — 異axis単発出現スワップ）— modal軸トークン（`require`）とauthz軸トークン
   （`forbid`）を、それぞれ全体で1回しか出現しない状態で2クローズ間で交換。各軸のシーケンスは
   単一要素のまま不変なので検知できない（=バグ）。実行結果: 同様に実際にFAIL。
   構築時の注意点として、`sign`が`POLARITY_TOKENS`の`approve`バケット（authz軸）に既に
   マップされているため、文中に迂闊に"sign off"を使うと意図せず追加のauthzトークンが混入し、
   axisシーケンスが変化して正しく再現できなくなる（最初の試行でこれにより誤ってテストが
   "ok"になった）。同様の理由で他のpolarity語彙（`POLARITY_TOKENS`の全リスト）を padding 文に
   混入させないよう注意が必要。
3. **`crates/specguard/src/ratify.rs`
   `write_lock_reports_error_when_write_silently_fails`**（finding 2）— ロックパスをあらかじめ
   ディレクトリで占有しておくと、`save_bytes`内部の`rename(&tmp, path)`が必ず失敗する
   （既存ディレクトリへのrenameは失敗する）ため、書き込み失敗を決定論的かつクロスプラットフォーム
   に再現できる。`write_lock`が`Err`を返すべき（望ましい挙動）ところ、実際には`path.exists()`が
   `true`のまま（ディレクトリが存在しているだけ）なので`Ok`を返してしまう（=バグ）。実行結果:
   `result.is_err()`のassertで実際にFAIL、バグを再現。
4. **`crates/stuckguard/src/detect.rs`
   `near_repeat_escalates_even_past_window_boundary`**（finding 3）— 既定`window=12`,
   `repeat_threshold=3`に対し`escalate_after=11`（到達可能上限
   `window - repeat_threshold + 1 = 10`の1つ上）に設定し、30回のnear-repeatループを流す。
   本来（長時間スタックの検知という主用途では）エスカレーションすべきところ、`same.first()`
   アンカーがウィンドウから追い出される度にキーがローリングしnudge countが毎回1にリセットされる
   ため到達しない（=バグ）。実行結果: 実際にFAIL（`last observed count=1`）。

**未テスト化の項目**（finding 4: blastguard item D、finding 5-7: 軽微な重複/デッドコード）—
item Dはcondukt側2呼び出し箇所（`gate_exec.rs`/`schedule.rs`）が空`diff_text`を渡す実装詳細の
確認であり、blastguard単体のユニットテストでは再現できず condukt 側の統合テストが必要
（out of scope として見送り）。finding 5-7は挙動バグではなく重複/デッドコードの指摘のため、
テストでの「検証」に馴染まない（コードレビューで直接確認済み）。

**副作用として実施した規約対応**: テスト追加はcrateソースへの変更にあたるため、CLAUDE.mdの
バージョンlockstepルールに従い `specguard` (0.2.18→0.2.19) と `stuckguard` (0.1.9→0.1.10) を
3ファイル（Cargo.toml / plugin.json / marketplace.json）同時にmicro bumpし、
`check-plugin-versions.py` / `check-version-bumped.py` の両ゲートがexit 0であることを確認済み。
`cargo fmt` / `cargo clippy --all-targets` も両crateでクリーン。
