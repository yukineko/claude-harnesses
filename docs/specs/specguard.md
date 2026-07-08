> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# specguard 仕様

## 概要

`specguard` は「仕様↔実装」のドリフトを監査する project-agnostic なハーネスである。`specguard.toml`
（`config::Config`）でプロジェクト固有情報（area / canon ポインタ / 不変条件 / agent 起動方法）を宣言し、
バイナリ自体は汎用に保つ。`run` は `git diff` から変更駆動のスコープを解決し（`scope::resolve`）、shard 単位の
read-only 監査プロンプトを描画し（`prompt::render_shard`）、read-only agent を駆動して findings を得て、
report と（人間レビューが要るときは）sentinel を書く。判定は agent 側に置き（agent が live な canon を読み逐語引用する）、
本バイナリはその周囲の決定論的ハーネスに徹する。加えて、drift 監査とは独立した「spec/feature↔impl↔test↔API」
マッピング store（`specmap::SpecMap`, `specguard map`）を持ち、それを情報源とする correctness 監査
（`auditmap`, `specguard audit`）を提供する。

## 不変条件

- **read-only 監査境界** — agent は変更を書けない。`config::AgentConfig::default()` の argv
  （`config::DEFAULT_AGENT_ARGS`）は Read/Glob/Grep と read-only な git のみ allowlist し、Edit/Write/
  NotebookEdit/WebFetch を deny する。`bypassPermissions` は渡さない（`--print` で allowlist 外ツールは
  auto-deny）。map/audit 系（`run_map`/`run_audit`/`auditmap::build_envelope`）は agent を spawn せず
  何も書き換えない（Human-on-the-loop）。
- **agent command のトラスト境界** — 非デフォルトの `agent.command` に shell metacharacter（`config::AGENT_COMMAND_METACHARS`）
  や `$(` が含まれると `config::validate_agent_command` が拒否する。spawn は shell を介さない直接 `Command::new` のため。
- **agent 失敗の隠蔽不可** — agent の生 exit code は伝播せず、常に `EXIT_AGENT_FAILED`(4) に写像し実 code は stderr へ
  （`finish` 内）。specguard 自身の exit code（`EXIT_OK`0/`EXIT_USAGE`2/`EXIT_NO_MARKER`3/`EXIT_AGENT_FAILED`4/
  `EXIT_UNRATIFIED`5/`EXIT_NO_FIX_COMMIT`6/`EXIT_TESTAUDIT_FINDINGS`7）は agent のそれと必ず disjoint。
- **map store は specmap.rs に委譲** — `specguard map` の永続化・同期は `specmap::SpecMap`（`load`/`load_or_init`/
  `save`/`sync`/`apply_changes`）が一手に担う。`sync` の唯一の副作用は `git log --name-status baseline..HEAD` の呼び出しで、
  `parse_name_status`/`apply_changes` は純関数。`SpecMap` は `BTreeMap` + sorted vec で決定論的に serialize され、
  drift/audit ワークフローから独立している。
- **決定論** — スコープ・shard 描画・fingerprint はすべて決定論。`scope::fingerprint_files` は FNV-1a 64bit で
  入力ファイル（`scope::shard_input_files`）をハッシュしメモ化キーとする。`scope::resolve_baseline` の precedence は
  固定（override > `baseline_ref` > recorded `.last-ref` > `fallback_ref`）。`scope::is_safe_ref` が baseline ref から
  先頭ダッシュ・空白・shell metacharacter を弾く。
- **fail-soft** — `pending`（SessionStart hook 入口）は best-effort で、config 欠落等どんなエラーでも何も出さず exit 0。
  `scope::code_index_files`（fugu-router 連携）は欠落・エラーで空を返し fail-soft。relevant-file map / escalation
  機能（`SPECGUARD_RELEVANT_MAP`, `scope::relevant_file_map`）は既定 OFF で、未設定なら従来挙動と厳密に不変。
- **baseline hold** — findings が出た run では baseline を advance せず sentinel を立てる。次 run で同じ drift が
  再検出される。人間が `specguard ack`（既定で修正コミットの存在=`report::has_new_commits` を要求）で解除する。
- **ratification ゲート** — `[prompt].require_ratification` が真なら、prompt（メタ正典）が未批准／批准後に drift した状態で
  `run`/`ingest`/`prompt --json` は監査を拒否する（`ratification_block` → `EXIT_UNRATIFIED`）。批准は
  `accept-prompt` が契約チェック（`prompt::missing_placeholders` で必須 placeholder 検証）を通してから pin する。

## 振る舞い

- **`run`（既定）** — `scope::resolve` でスコープ解決 → `prompt::shards` で shard 列挙 → 各 shard を
  `agent::run_shards` で read-only agent へ dispatch → `finish` で parse/verify/merge/sentinel。content-hash
  メモ化（`classify_cache`/`persist_fingerprints`）で入力不変な green shard は agent 呼び出しを省き
  clean 結果を合成する（`cached_shard_output`）。marker（`parse::MARKER`）欠落 shard があれば report は保存するが
  baseline を進めず `EXIT_NO_MARKER`。
- **`scope`** — 解決済みスコープ（baseline / 変更ファイル数 / in-scope area / 不変条件 / decision 記録）を表示するのみ。agent 非呼び出し。
- **`prompt [--json]`** — 描画済み shard プロンプトを表示。`--json` は `{project, baseline, head, date, marker,
  shards:[{label, prompt}]}` envelope を出し（`emit_prompt_json`）、plugin が read-only subagent へ配る。`--json` は
  ratification ゲート対象。
- **`ingest [--from]`** — subagent が集めた per-shard 出力（JSON, stdin または `--from`）を label で shard に整列し
  （`read_ingest`）、agent を spawn せず `finish` の parse→report→sentinel パイプラインを回す。`run` と同じ exit code。
- **`map build|sync|list|set-spec|resolve|prune`（`run_map`）** — 独立した spec↔impl↔test↔API マッピング store を保守。`build` は
  full window から seed（`.last-ref` 無視）、`sync` は増分（audit と同じ baseline precedence）。`build`/`sync` はどちらも
  `[map].exclude` グロブに一致する追加/変更パスを新規 entry として計上せず（`specmap::filter_excluded`。削除/リネームは
  既存 entry の掃除のため残す）、併せて既存の一致 entry を prune するので、config churn ではなく genuine な spec drift だけが
  `changed` に残る。`list [--json] [--filter]` は表示（`filter_map`＋`specmap::entry_matches` の共有述語）。
  `set-spec <selector> <doc>` は selector（厳密 key or グロブ、例 `crates/foo/src/**`）に一致する entry へ spec-doc を紐付け
  `tracked` にする（impl があり spec を書き起こした後の解決＝追記の反映）。`resolve <selector>` は spec-doc 不要と判断した
  entry を `tracked` にする（レビュー済み・drift 無し）。`prune` は `[map].exclude` 一致 entry を除去する（exclude 設定前に
  seed した map の掃除）。永続化は `specmap::SpecMap`。
- **`audit [--json] [--filter]`（`run_audit`）** — map store を情報源とする read-only correctness 監査。drift（整合）と
  異なり「実装・仕様が正しいか」を見る。`auditmap::build_envelope` が構造的 findings
  （`StructuralKind::{Undocumented,DanglingReference,Untested}`, `structural_findings`）と per-entry LLM 監査 shard
  （`render_audit_shard`, `MAX_AUDIT_SHARDS`=50 で cap）を組み立てる。`--json` は ingest 互換 envelope。何も修正しない。
- **`brief <task> [--prompt]`** — 着手前 read-only 仕様ブリーフィング（`prompt::render_brief`）。全 area + 不変条件を対象に、
  report/sentinel を出さず drift を未然防止する（run の事後監査の前線）。
- **`decide <title> [--force]`** — 現在の canon commit に pin した ADR を scaffold（`decision::scaffold`）。
- **`accept-prompt -m <reason>`** — prompt テンプレート（メタ正典）を契約チェック後に批准し、fingerprint・canon commit・理由を
  lock に pin（`ratify::write_lock`）。有効なゲートのポリシーのみ pin する。
- **`testaudit [--json]`** — 実装済みだが実行されていないテスト（`#[ignore]`・cfg 除外・未 `mod` 宣言など）を走査
  （`testaudit::scan_repo`）。clean で 0、findings で `EXIT_TESTAUDIT_FINDINGS`(7)。
- **`ack [--force]`** — 対応済み sentinel をクリア。既定は sentinel 立ち上げ以降の新規コミット
  （`report::sentinel_raised_at`/`has_new_commits`）を要求、無ければ `EXIT_NO_FIX_COMMIT`(6)。`--force` で回避。
- **`pending`** — SessionStart hook 入口。sentinel が pending なら fix-offer ブロックを表示、無ければ／エラー時は無出力 exit 0。
- **`init [--force]`** — starter config + SessionStart hook を scaffold（`init::run`）。冪等。

### module 責務

- **`config`** — `Config` と全サブテーブル（`Project`/`AgentConfig`/`ScopeConfig`/`OutputConfig`/`PromptConfig`/
  `VerifyConfig`/`MapConfig`/`DecisionsConfig`/`Area`/`Invariant`）を TOML から deserialize・validate。
  `deny_unknown_fields`。agent command のトラスト境界を担う。`MapConfig` は `path`/`spec_doc_dir` に加え
  `exclude`（spec-bearing でないパス＝lockfile/manifest/生成物/docs/scripts/fixtures の repo-root 相対グロブ列。
  既定空＝従来どおり全変更を tracking）を持つ。
- **`scope`** — 変更駆動スコープ解決。`resolve`（baseline→diff→classify の全パイプライン）、`resolve_baseline`、
  `changed_files`（3-tier fallback、`whole_tree_fallback_max_files` 予算超過で Err）、`classify`（純: 変更ファイル→area）、
  `shard_input_files`、`fingerprint_files`、`relevant_file_map`/`shard_query`/`code_index_files`（コードインデックス連携）。
- **`prompt`** — テンプレート（data）+ 解決済みスコープからプロンプトを描画。`Shard`（`Area`/`Invariants`/`Decisions`）、
  `shards`/`shard_label`/`render_shard`/`render_shard_with_map`/`render_brief`/`render_refute`/`render_completeness`、
  各 `*_TEMPLATE`/`*_PLACEHOLDERS`、`signals_insufficient_context`（`NEEDS_WIDER_SCOPE_SIGNAL` 検出）。canon の中身は
  プロンプトに焼き込まず agent が live で読む。
- **`auditmap`** — map 駆動 correctness 監査のスコープ・構造的 findings・shard 組み立て。`SpecMap` の read-only 消費者で、
  変更も修正も agent spawn もしない。`is_undocumented`/`is_untested`/`dangling_refs`/`structural_findings`（純）、
  `scan_map_filtered`（FS 存在チェック付き）、`render_audit_shard`、`build_envelope`（`AuditEnvelope`/`AuditShard`）。
- **`specmap`** — 独立・再利用可能な spec/feature↔impl↔test↔API store。`SpecMap`（`entries: BTreeMap`, `last_synced`）、
  `MapEntry`（`kind`/`spec_doc`/`status`/`impl_files`/`test_files`/`client_refs`/`api`）、`Status`/`EntryKind`/`FileRole`/
  `ApiRef`/`Change`、`classify_path`/`parse_name_status`/`entry_matches`/`compile_globs`/`filter_excluded`（純）、
  `load`/`load_or_init`/`save`/`sync`（`exclude: &GlobSet` を取り追加/変更を除外フィルタ）/`apply_changes`/`prune_excluded`/
  `set_spec`/`resolve`、`DEFAULT_MAP_PATH`=`.specguard/spec-map.toml`。意味的帰属（spec-doc 紐付け・drift 解決）は
  `set_spec`/`resolve` を通じ LLM 消費者が行い、store は決定論的に永続化・同期・除外のみ。
