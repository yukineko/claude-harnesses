# DESIGN: Continuous-Audit finding triage — backlog に積んだ後、人間が「直すか棄却するか」を判断しやすくする

**状態**: draft（未実装、docのみ）。`docs/DESIGN-pdo-session-anchor.md` とは独立のテーマ（あちらはセッションの記憶・スコープ逸脱、こちらは Continuous-Audit の成果物の triage）。

## 1. 動機

`/continuous-audit`（`crates/overwatch/skills/continuous-audit/SKILL.md`）は gate crates への敵対的レビュー1ラウンドを回し、CONFIRMED になった finding を `overwatch review-queue --to-backlog`（`crates/overwatch/src/bridge.rs`）経由で `backlog` に積む。この設計自体（＝人間の確認窓口を `backlog` 一本に絞り、`overwatch record-disposition` のような事前ゲートを別途設けない）は妥当と確認済み——確認場所を増やすと逆に複雑になるため。

しかし、実際に backlog task を見た人間が「直すか棄却するか」を判断しようとすると、次の3つが欠けている。

### 1.1 説明が痩せすぎている

`ReviewFinding`（[review_finding.rs](../crates/overwatch/src/review_finding.rs)）は `{finding_id, source, severity, summary, file, ts}` のみを保持する。`summary` は1行要約で、verifier が Step 2 で実際に行った「なぜ CONFIRMED と判断したか」という根拠（file:line 引用、反証を退けた理由）は永続化されない。`bridge.rs` が組み立てる backlog task の notes も同様に痩せている:

```rust
let notes = format!("finding-id:{} file:{} severity:{}", ...);
```

人間は結局その場でコードを読み直さないと判断できない。

### 1.2 経過時間による陳腐化が分からない

`ReviewFinding.ts` は記録時刻を持つが、それが notes に一切表示されない。**経過日数それ自体が、人間にとって棄却を検討する材料になる**（古い指摘で、その間何も問題が起きていないなら、優先度を下げてよい可能性が高い）。これは追加のインシデント追跡を要求するものではなく、単に「確認から何日経ったか」を見せるだけで足りる、という判断で合意済み。

### 1.3 confirmed 済みの回帰テストが再現するかどうかが、判断に活きていない

Step 4（[continuous-audit/SKILL.md](../crates/overwatch/skills/continuous-audit/SKILL.md) Step 4）で追加される `#[ignore]`d 回帰テストは、実際にそのバグが**今も再現するかどうかを機械的に判定できる、最も強いシグナル**である。しかし:

- finding-id とテスト関数の対応は、doc comment 中の**人間可読な自由文言**でしか結ばれておらず（例: `"...re-review finding 2"`）、finding-id（`CA-xxx` 形式）と構造的に一致していないため、機械的に逆引きできない。
- 仮に逆引きできても、それを実行して結果を backlog task に反映する経路が存在しない。

test の FAIL は「まだ現役のバグ＝確認・修正する強い理由」、PASS は「すでに解消されている可能性＝棄却/deprioritize の理由」という、方向の異なる2つの強いシグナルになる。これが経過日数という弱いシグナルと組み合わさることで、人間の triage 判断が大きく助けられる。

### 1.4 backlog 自体に「棄却」の状態がない

`backlog` の status 語彙は `pending`/`done`/`failed` の3つのみ（[task.rs:11-13](../crates/backlog/src/task.rs#L11-L13)）。`failed` は「2日後に再浮上する一時保留」であって、「見て判断した結果、対応しない」という恒久的な却下ではない。この gap は§8（非目標）で扱い、本設計では解決しない（様子見）。

## 2. 設計原則

1. **窓口は増やさない**: 人間が確認する場所は引き続き `backlog` 一本。`overwatch record-disposition` を bridge の事前ゲートにはしない（前回合意の踏襲）。
2. **advisory-first / fail-soft**: 追加するシグナル（経過日数・テスト再実行結果）はどれも判断材料の提示に留まり、何もブロックしない。テスト実行に失敗しても bridge 自体は成功させる。
3. **決定論は軽く、意味判断は人間**: rationale の文章化・却下/対応の最終判断は人間（またはLLM）に委ね、overwatch/backlog 側はその材料を集めて渡すだけ。
4. **後方互換なスキーマ拡張**: `ReviewFinding` への追加フィールドは `#[serde(default)]` + `skip_serializing_if` で、既存の `review_findings.jsonl` を壊さず読める（`AuditRound.round` の legacy 数値対応と同じ手法）。
5. **finding-id と回帰テストの対応は機械的に逆引き可能にする**: 現状の「自由文言でのリンク」をやめ、`#[ignore = "<finding-id>: ..."]` という構造化規約に統一する。

## 3. アーキテクチャ

```
Step 2 (verifier)                Step 3 (record)                bridge (to_backlog)
  rationale 文章生成  ────────▶  ReviewFinding.rationale として記録
                                      │
Step 4 (テスト記述)                    │
  #[ignore = "<finding-id>: ..."] ────┤ (finding-id で構造的に紐づく)
                                      │
                                      ▼
                          backlog task の notes に集約:
                          ┌─────────────────────────────┐
                          │ finding-id / severity        │
                          │ rationale（verifier の根拠）  │
                          │ confirmed: <ts>（N日前）      │
                          │ regression test: <path>::<fn> │
                          │   → 再実行結果: FAIL/PASS/なし │
                          └─────────────────────────────┘
```

## 4. クレート別の変更内容

### 4.1 overwatch — `ReviewFinding` スキーマ拡張

`crates/overwatch/src/review_finding.rs`:

```rust
pub struct ReviewFinding {
    pub finding_id: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// verifier が CONFIRMED と判定した根拠（file:line 引用・反証を退けた理由）。
    /// 追加フィールドにつき既存レコードとの後方互換のため Option + default。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub ts: i64,
}
```

`record_finding` / `record-finding` CLI に `--rationale <text>` を追加（省略可、既存呼び出しは無変更で動く）。

### 4.2 overwatch — 回帰テスト逆引き＋再実行

新規モジュール（案）`crates/overwatch/src/test_freshness.rs`:

- `find_ignored_test(finding_id: &str, search_root: &Path) -> Option<(String crate_name, String test_path, String fn_name)>` — `#[ignore = "<finding_id>: ...` パターンを対象クレート配下でgrep（正規表現、`ripgrep`依存を避けるなら `walkdir` + 文字列検索で十分軽い）。
- `run_ignored_test(crate_name: &str, fn_name: &str) -> TestFreshness` — `cargo test -p <crate_name> -- --ignored <fn_name>` を実行し、`{Passing, Failing{message}, NotFound, ExecutionError}` を返す。**fail-soft**: `cargo` 不在・ビルド失敗はすべて `ExecutionError` として扱い、呼び出し元を止めない。

### 4.3 overwatch — `bridge.rs` の notes 組み立てを拡張

`bridge::run_in` で backlog task を作る直前に:

1. `finding.ts` から経過日数を計算し notes に追加。
2. `finding.rationale` があれば notes に追加。
3. `test_freshness::find_ignored_test(&finding.finding_id, cwd)` を試み、見つかれば `run_ignored_test` を実行して結果を notes に追加（`FAIL: <panicメッセージ>` / `PASS（解消済みの可能性）` / 該当テストなし、のいずれか）。

これらはすべて **notes の文字列を厚くするだけ**で、`to_backlog` の中核ロジック（dedup・idempotency・fail-soft）は変更しない。

### 4.4 continuous-audit SKILL.md — 手順の更新

- Step 2（refute-verifier）: 「CONFIRMED と判定した根拠（file:line 引用込み）を `rationale` として保持し、Step 3 で `--rationale` に渡す」を追記。
- Step 4（回帰テスト化）: `#[ignore]` の理由文字列は**必ず `<finding-id>: ` で始める**という規約を明記（既存の自由文言運用からの変更）。

## 5. 非目標

- **backlog に「棄却（wontfix）」状態を追加すること**: 今回はやらない。`failed`（2日後再浮上）をそのまま代用するか、単に何もしない運用で様子を見る。実際に「同じ棄却タスクが繰り返し再浮上して困る」事態が起きたら別設計として検討する。
- **`overwatch record-disposition` を bridge の事前ゲートにすること**: 既存合意通りやらない。disposition は review-metrics 用の別ストリームのまま。
- **ファイル内容ハッシュによる高精度の陳腐化検知**（前回案の「2段構え」のうち重い方）: 経過日数表示と回帰テスト再実行で当面のニーズは満たせると判断し、優先度を下げる。将来 gap が顕在化したら追加を検討する。
- **finding が無い（テスト化されていない）ものへの陳腐化シグナル強化**: 経過日数のみで足りるとする。

## 6. 受け入れ基準

- [ ] `ReviewFinding` に `rationale: Option<String>` を追加しても、追加前に書かれた `review_findings.jsonl` の既存行がエラーなく読める（回帰テストで担保）。
- [ ] `record-finding --rationale` を省略した既存呼び出しが従来通り動く（後方互換）。
- [ ] `#[ignore = "<finding-id>: ..."]` 規約に沿ったテストが、`test_freshness::find_ignored_test` で正しく逆引きできる（正例・該当なしの両方をテスト）。
- [ ] 該当テストが実際に FAIL する状態で `run_ignored_test` を呼ぶと `Failing{message}` が返る（fixture crate か既存の `#[ignore]`d テストで検証）。
- [ ] `cargo`/対象クレートが存在しない・ビルドできない環境でも `bridge::run_in` 全体が exit 0 で完走する（fail-soft の回帰テスト）。
- [ ] `overwatch review-queue --to-backlog` 経由で作られた backlog task の notes に、経過日数・rationale（あれば）・テスト再実行結果（あれば）が含まれる（統合テスト）。
- [ ] 変更した crate（`overwatch`）の version が3ファイル lockstep で上がっている（`check-plugin-versions.py`/`check-version-bumped.py` green）。

## 7. 段階的ロールアウト

1. **Phase 1**: `ReviewFinding.rationale` フィールド追加＋後方互換テスト（独立、リスク最小）。
2. **Phase 2**: SKILL.md の `--rationale` 手順追記（doc変更のみ、コード非依存）。
3. **Phase 3**: `#[ignore]` 理由文字列の finding-id 規約化（SKILL.md記載＋既存 `#[ignore]`d テストの書式統一、任意で段階的に移行可）。
4. **Phase 4**: `test_freshness` モジュール実装（逆引き＋実行、pure関数部分とI/O部分を分離してテスト）。
5. **Phase 5**: `bridge.rs` への統合（notes 組み立ての拡張）。

各 phase は独立に PR 化可能。`overwatch` は GATE_CRATES ではないため canary 不要（`scripts/rollout-plugins.sh` 通常経路で可）。

## 8. リスク・トレードオフ

- **`cargo test` 実行コスト**: backlog task 作成のたびに対象クレートをビルド・テスト実行するのは、finding 件数が多い時に無視できないコストになりうる。対策: 結果をキャッシュする（同じ finding-id は一度実行したら再実行しない、あるいは TTL 付きキャッシュ）か、bridge 時ではなく `backlog next`（pickup 時）にだけ実行するかは実装時に選択。
- **finding-id 規約の移行コスト**: 既存の `#[ignore]`d テスト（例: commit `38f613c` のもの）は新規約に沿っていないため、逆引きできない。過去分は未対応のまま許容し、今後の新規追加分から規約を適用する（fail-soft: 逆引きできなければ「該当テストなし」として扱うだけで、エラーにはしない）。
- **rationale の情報量とコスト**: verifier に長い rationale を書かせるほど Step 2 のトークンコストが増える。severity=high など優先度の高い finding に限定して詳細な rationale を要求する、といった運用上の縛りは今後の調整余地として残す。

## 9. 未確定・オープンな問い

- テスト再実行は bridge 時（finding→backlog変換時）と pickup 時（`/flow` が実際に着手する時）のどちらで行うのが適切か。両方やると二重コストになるため、実装時に一本化する。
- `rationale` の長さに上限を設けるか（backlog の `notes` フィールド自体に長さ制約があるかは要確認）。
- finding-id 規約移行を、過去の `#[ignore]`d テストにも遡及適用するか（コスト対効果次第）。
