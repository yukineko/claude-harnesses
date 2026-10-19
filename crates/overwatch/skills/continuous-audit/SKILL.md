---
name: continuous-audit
description: gate crates に対する敵対的レビュー1ラウンド(finder→refute ベース verifier→CONFIRMED/REFUTED/UNVERIFIED の三値判定)を回し、CONFIRMED を overwatch review-queue へ、UNVERIFIED を再検証待ちとして [UNVERIFIED] マーク付きで queue へ記録する Continuous-Audit driver [test: crates/overwatch/tests/verdict_tri_state.rs::unverified_findings_stay_pending_and_visible]。round metrics を audit ledger に残し収束(round 越しの new-findings 減少)を追跡する [test: crates/overwatch/tests/audit_round.rs::audit_metrics_reports_decreasing_trend_and_closure_rate]。
argument-hint: [round-id] [--target crate1,crate2] [--dry-run]
allowed-tools: Task, Bash(scripts/continuous-audit.sh:*), Bash(overwatch:*), Bash(git:*), Read, Grep, Glob
---

gate crates への **敵対的レビュー 1 ラウンド**を回し、確認された指摘 (CONFIRMED findings) を
`overwatch review-queue` に、**判定不能な指摘 (UNVERIFIED)** を review-queue に
「再検証待ち」として自動供給する [test: crates/overwatch/tests/verdict_tri_state.rs::unverified_findings_stay_pending_and_visible]。決定論の記録は `scripts/continuous-audit.sh`(+ overwatch
バイナリ) が担う [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]。

```
finder (提案)  →  refute-verifier (反証で篩う)  →  verdict 三値
                                                    ├─ CONFIRMED  → --finding             → queue
                                                    ├─ UNVERIFIED → --unverified-finding  → queue のみ (pending)
                                                    └─ REFUTED    → --unverified-finding  → queue のみ (pending)
                                                                     (REFUTED として成立するのは probe + 人間の sign-off が揃った時だけ。下記)
                                                        │
                        scripts/continuous-audit.sh …  ← 決定論 record
                                                        ▼
                        overwatch review-queue  (finding-id で dedup 済み)
```

**opt-in**: どの always-on ゲートにも配線されておらず、condukt/rollout の挙動を変えない [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_opt_in_not_wired]。呼んだときだけ走る [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_opt_in_not_wired]。

`scripts/continuous-audit.sh` が存在することを確認すること [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]。無ければ導入を案内して停止すること。

overwatch バイナリをビルド済みにしておくこと。スクリプトが自動解決する (`OVERWATCH_BIN` / PATH / `target/{release,debug}`) [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_binary_resolution]。

## 対象 crate (既定)

既定の target は GATE crates に audit 専用の backlog を加えた `blastguard,propguard,specguard,stuckguard,mutategate,overwatch,parallelguard,backlog` である [test: scripts/test_check_gate_crates_sync.py::test_current_repo_is_in_sync]。
`--target` で上書きできる [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_target_override]。

### Step 0 — 引数を決めること

- `round-id` には識別子 (例: `2026W28`、連番、日付) を渡すこと [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]。
- `--target <csv>`: 対象 crate を上書き (既定は上記 GATE crates) [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_target_override]。
- `--dry-run`: 記録せずに計画だけ表示 (finder/verifier は回してよいが record はしない) [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves] [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_unverified_finding_flag_records_unverified]。

### Step 1 — finder (敵対的発見) を起動すること

- finder に修正させないこと。
- finder の失敗・異常終了を 0 件として扱わないこと。
- 対象 crate ごとに read-only の finder subagent を並列に起動すること。
- 各 finder に、その crate のソースを敵対的に読み、**実在し根拠のある**バグ・退行・見落とし (境界条件・fail-open・無音化・near-repeat の window 依存・polarity バイパス等) を **逐語引用 (file:line) つき**で列挙させること。
- 指摘は severity (high, med, low)・summary・file:line を含む形で返させること。
- 推測・スタイル論・LGTM の水増しを避け、「実在する問題だけ」を強制すること。
- 見つからなければ空を返させ、0 件は正常として扱うこと。
- Step 2 の model diversity チェックで必要なので、finder で使用したモデルを記録すること。Task の実行結果から使用モデルを確認し、メモに記録しておくこと。

### Step 2 — refute-verifier (反証で篩う) を起動すること

> **既定を REFUTED にしてはならない。判定不能は制限側 = UNVERIFIED に倒すこと。**

finder の各指摘を、別の (finder とは独立した) verifier subagent に渡し、**反証を試みさせる**こと。

verifier に三値 (CONFIRMED / REFUTED / UNVERIFIED) で verdict を出させ、次を指示すること。

- finder と verifier に同一のモデルを指定してはならない。MUST: 別 subagent を使い、finder で記録したモデルと異なるモデルを明示的に指定すること。
- 指定後、実際に同じモデルペアになっていないことを確認してから verifier を実行すること。
- **PLAUSIBLE** という語を使用しない。
- **CONFIRMED** (verifier がコードで再現/立証できた指摘) は review-queue に載せる [test: crates/overwatch/tests/verdict_tri_state.rs::unverified_findings_stay_pending_and_visible]。
  - 立証には該当箇所の **file:line と逐語引用**、および「その状態が下流でどう permissive に作用するか」の
    経路を 1 本以上示すこと。
- **REFUTED** は、指摘が実在しないことを**積極的に立証できた**ときだけ使うこと。反証側にも立証責任を課すこと。
  - 「permissive な到達路を示せなかった」は UNVERIFIED であり、REFUTED にしてはならない。
  - 1 本でも辿れていない消費経路があれば REFUTED にしてはならない。
  - その値や状態の消費者 (consumption path) を全列挙すること。
  - 「grep した」で済ませず、各消費経路について file:line + 逐語引用を出すこと。
  - 各経路について「この経路では permissive にならない」根拠を、**引用したコードだけから**述べること。「〜のはず」「設計上そうなっていない」を根拠にしないこと。
  - 消費者が別 binary や別 crate にコンパイルされる、呼び出しが動的、という理由で追跡を打ち切った場合は UNVERIFIED とすること。
  - overwatch は `record-finding --verdict refuted` を、機械 probe の結果が `not_reproduced` (`--probe <file>`、JSON `{"result":"not_reproduced"}`) **かつ人間の sign-off** (`--signed-off-by <name>`) の両方が揃った場合にだけ `refuted` として保存する [test: crates/overwatch/tests/refuted_witness.rs::refuted_with_not_reproduced_probe_and_signoff_stands_control] [test: crates/overwatch/tests/refuted_witness.rs::refuted_with_a_probe_but_no_signoff_is_stored_unverified] [test: crates/overwatch/tests/refuted_witness.rs::refuted_without_a_probe_is_stored_unverified]。
  - probe が読めない・パースできない場合は `unverified` として保存する [test: crates/overwatch/tests/refuted_witness.rs::unreadable_or_unparseable_probe_is_never_refuted]。
  - probe が `reproduced` なら (sign-off 付きの場合) `confirmed` として保存する [test: crates/overwatch/tests/refuted_witness.rs::reproduced_probe_against_a_refuted_claim_lands_confirmed]。
  - verifier が REFUTED と判定した指摘を捨ててはならない。`--unverified-finding` で UNVERIFIED として記録すること [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_unverified_finding_flag_records_unverified]。
  - rationale の先頭に、REFUTED 判定だったこと (probe witness も human sign-off も無いこと) を書き、列挙した消費経路と逐語引用を続けること。
  - REFUTED として閉じられるのは、人間が probe を回して `overwatch record-finding --verdict refuted --probe <file> --signed-off-by <name>` を実行した時だけ [test: crates/overwatch/tests/refuted_witness.rs::refuted_with_not_reproduced_probe_and_signoff_stands_control]。
- **UNVERIFIED**: 上記いずれも満たさない判定不能のものは UNVERIFIED とすること。
  - 捨てないこと: 項目は pending のまま (done にも失敗にもしない) Step 3 の `--unverified-finding` で記録し、
    review-queue には `[UNVERIFIED]` マークつきで並び、再検証待ちとして可視化されたまま残る [test: crates/overwatch/tests/verdict_tri_state.rs::unverified_findings_stay_pending_and_visible]。
  - verifier には「何が確認できて、何が確認できなかったか (辿れなかった経路の名前)」を rationale に書かせること。
- Step 3 で finder と verifier のモデルを必ず両方渡すこと。overwatch は両者が同一モデルなら review-queue に high severity の警告 finding を1件記録する [test: crates/overwatch/tests/audit_round.rs::same_finder_verifier_model_records_warning_finding_in_review_queue]。
- 高リスク指摘では verifier を複数立てて多数決にしてもよいが、その場合も複数の verifier に異なるモデルを指定すること。
- verifier が **CONFIRMED** と判定した際は、その根拠 (file:line 引用込み・反証を退けた理由) を rationale として保持しておくこと。
  Step 3 で、渡せる場合は必ず `--rationale` にそのまま渡すこと [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_rationale_flag]。
  `--rationale` を省略しても記録自体は成功する [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_rationale_flag]。
- **UNVERIFIED** と判定した際も rationale を残すこと。内容は「辿れた経路」と**「辿れなかった経路」**の
  両方 (何が未確認のまま残っているか) を書くこと。
- **REFUTED** と判定した際は、消費経路の全列挙と各経路の逐語引用を verifier の出力に残すこと。列挙できない場合や引用できない場合は REFUTED を名乗らせず、UNVERIFIED に落とすこと。列挙できた場合も、記録は UNVERIFIED として行い、その列挙を rationale に入れること。

CONFIRMED subset と UNVERIFIED subset をそれぞれ確定し、各件を finding-id | severity | summary | file の形に整形すること。
finding-id を、CA-<crate>-<連番> や rule id のような安定なキーにすること。同じ指摘が次ラウンドでも CONFIRMED なら同じ finding-id を再利用すること。
同じ finding-id で再度記録された指摘は review-queue で 1 行に畳まれる [test: crates/overwatch/tests/review_queue.rs::review_queue_collapses_a_refound_finding_to_one_row]。
UNVERIFIED も同じ id を使い回すこと。

### Step 3 — 決定論レコーダで記録すること

verifier の REFUTED を捨ててはならない。

スクリプトの exit 0 を、全 finding が review-queue に記録された証拠とみなさないこと。

CONFIRMED subset と round metrics を `scripts/continuous-audit.sh` に渡すこと [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]。

```sh
# 記録 (実書き込み): [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]
scripts/continuous-audit.sh --round <round-id> --target <csv> \
  --new-findings <このラウンドで新規に出た件数> \
  --confirmed <CONFIRMED 件数> \
  --unverified <UNVERIFIED 件数> \
  --regression-tests-added <確定指摘を回帰テスト化した件数> \
  --finder-model <finder が使ったモデル> --verifier-model <verifier が使ったモデル> \
  --finding '<id>|<severity>|<summary>|<file>|<rationale>' \
  --unverified-finding '<id>|<severity>|<summary>|<file>|<rationale>'

# プレビュー (何も書かない): [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves] [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_unverified_finding_flag_records_unverified]
scripts/continuous-audit.sh --round <round-id> --dry-run
```

- `--finding` / `--unverified-finding` を CONFIRMED / UNVERIFIED の件数だけ繰り返し指定すること [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_finding_flags_repeat_and_format]。
- `--finding` / `--unverified-finding` の書式は同じ `id|severity|summary|file|rationale` (severity / file / rationale は省略可) [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_finding_flags_repeat_and_format]。
- **verdict は record 時に記録される**: `--finding` は `overwatch record-finding --verdict confirmed`、 `--unverified-finding` は `--verdict unverified` として書かれる [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_unverified_finding_flag_records_unverified]。
- スクリプトは `--verdict refuted` を書かない [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_script_never_writes_refuted]。witness 無しの REFUTED は overwatch 側でも UNVERIFIED に倒される [test: crates/overwatch/tests/refuted_witness.rs::refuted_without_a_probe_is_stored_unverified]。
- REFUTED は `--unverified` 件数にも数えること [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_unverified_count_flag]。
- 未知の verdict 値は `unverified` に倒れる [test: crates/overwatch/tests/verdict_tri_state.rs::unverified_findings_stay_pending_and_visible]。
- `overwatch review-queue` に `[UNVERIFIED]` マークつきで並ぶ = **捨てられない** [test: crates/overwatch/tests/verdict_tri_state.rs::unverified_findings_stay_pending_and_visible]。
- UNVERIFIED は `--confirmed` 件数に含めないこと [test: crates/overwatch/src/audit_round.rs::round_records_unverified_count_separately_from_confirmed]。
- `--finder-model` と `--verifier-model` の両方を常に渡すこと [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_script_forwards_model_flags]。
- `--new-findings` / `--confirmed` / `--unverified` / `--regression-tests-added` は
  **round ledger にそのまま記録される件数** [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_unverified_count_flag]。`--finding` / `--unverified-finding` エントリは
  review-queue に流す subset (件数入力とは独立) [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_finding_entries_independent_of_counts]。
- スクリプトは finding を review-queue へ、ラウンドを収束 ledger へ追記する [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]。
- `audit-round record` は判定を持つので、拒否されたらスクリプトはそこで停止する [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]。

### Step 3.5 — ミラー掃討を行うこと (**必須**)

掃討を黙って省略しないこと。確定した各 CONFIRMED について、その「双子」を同じラウンド内で探索すること。
各 CONFIRMED に対して、最低限この 5 軸を走査すること。

- 極性: 否定形と肯定形の両方を見ること。
- 軸と方向: 対称な相手側 (read と write など) を見ること。
- 別綴り: 同義の別表記があるか確認すること。
- 同一 store の別書き手: 同じ状態を書く他の経路があるか確認すること。
- 粒度: より細かい単位にも粗い単位にも同じ穴が無いか確認すること。

- 掃討で見つかった双子は**そのラウンドの CONFIRMED として扱う** (次回送りにしない)。
  その件数は CONFIRMED と回帰テストの両方の件数に反映すること。
- 掃討して何も出なかった場合は「掃討済み・双子なし」と報告に明記すること。

### Step 4 — 回帰テスト化すること

> **回帰テストをラウンド記録より後に landed させてはならない。** CONFIRMED を減らして辻褄を合わせることは禁止する。

- `overwatch audit-round record` は `regression_tests_added < confirmed` のラウンドを拒否する (非0終了・audit-round ledger へ一切書かない・`scripts/continuous-audit.sh` もそこで停止する) [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves]。
- confirmed が 0 のラウンド (全件 refute) は締める対象が無いので通る [test: crates/overwatch/src/audit_round.rs::closure_incomplete_lets_an_all_refuted_round_through] [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_target_override]。
- CONFIRMED のうち**挙動バグ**は、対象 crate に**回帰テストを追加して固定**すること。
  これは別タスク (backlog / condukt) に委譲してもよいが、追加できた件数を記録すること。

### Step 4.5 — closure を書き戻すこと

> 新規ラウンドを、record は 0 件で通して後から close で埋める形で記録しようとしてはならない (record 自体が拒否される [test: crates/overwatch/tests/continuous_audit_script.rs::continuous_audit_script_behaves])。
> この Step は、拒否ルール導入前に記録された既存ラウンドの backfill 専用として使うこと。
> `converging` は `closure_rate` を参照しない [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_converging_ignores_closure_rate]。

CONFIRMED を回帰テストで締めたら、その件数を**元のラウンドへ closure として書き戻す**こと。

```sh
overwatch audit-round close --round <round-id> --tests <N>
```

- N に raw なテスト関数の総数を渡してはならない。N には「回帰テストで固定した confirmed findings の数」を渡すこと。
- 同じ N で二度 close しても二重計上されない (SET であり加算ではない) [test: crates/overwatch/src/audit_round.rs::set_round_tests_is_idempotent_set_not_add]。
- N が confirmed を超えても closure_rate は 1.0 を超えない [test: crates/overwatch/src/audit_round.rs::set_round_tests_clamps_to_confirmed_so_closure_rate_never_exceeds_one]。
- 同じ round-id が重複記録されている場合は**最後に記録されたラウンド**が closure 対象になる [test: crates/overwatch/src/audit_round.rs::set_round_tests_duplicate_id_closes_most_recent]。

### Step 5 — 結果を確認すること

```sh
overwatch review-queue            # 記録した finding が [ai-finding] タグで severity 順 (同じ severity 内は最新順) に並ぶ (finding-id で dedup 済み) [test: crates/overwatch/src/review_queue.rs::stale_high_severity_outranks_fresh_low_severity] [test: crates/overwatch/tests/review_queue.rs::review_queue_merges_all_three_sources_time_ordered] [test: crates/overwatch/tests/review_queue.rs::review_queue_collapses_a_refound_finding_to_one_row]
overwatch audit-metrics           # round 越しの new-findings 推移・converging フラグ [test: crates/overwatch/tests/audit_round.rs::audit_metrics_reports_decreasing_trend_and_closure_rate]
```

- CONFIRMED は `overwatch review-queue` に `[ai-finding]` 行として現れる (systemic / rollback と統合表示) [test: crates/overwatch/tests/audit_round.rs::confirmed_finding_recorded_by_audit_loop_surfaces_in_review_queue] [test: crates/overwatch/tests/review_queue.rs::review_queue_merges_all_three_sources_time_ordered]。
- UNVERIFIED は `[UNVERIFIED]` マーク付きで現れる [test: crates/overwatch/tests/verdict_tri_state.rs::unverified_findings_stay_pending_and_visible]。
  **UNVERIFIED が積み上がるのは異常ではなく情報**: 次ラウンドで優先的に再検証する対象を指している
  (`audit-metrics` の `cumulative unverified` で追える [test: crates/overwatch/src/audit_round.rs::round_records_unverified_count_separately_from_confirmed])。
- ラウンドを重ねて `new-findings` が減少すると `converging` は true になる [test: crates/overwatch/src/audit_round.rs::converging_when_new_findings_decrease]。増加すると false になる [test: crates/overwatch/src/audit_round.rs::not_converging_when_new_findings_increase]。

`scripts/continuous-audit.sh` 冒頭と `scripts/continuous-audit.cron.example` に、週次 cron / git pre-push advisory のテンプレートがある [test: crates/overwatch/tests/prose_claims_ca.rs::claim_ca_automation_templates_exist]。
導入は手動 (コピー) で行うこと。

- overwatch バイナリが不在なら、ビルドしてから再実行すること。
