> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# benchkit 仕様

## 概要

`benchkit` は SWE-bench Verified 用のベンチマークランナー（skeleton slice）である。CLI は `clap` の 3 サブコマンド — `load`（JSONL split を型付き `Instance` に読み込み決定論的サマリを出力）、`download`（upstream データセットを `curl` でローカルキャッシュへ取得。唯一ネットワークに触れる経路）、`run-instance`（1 インスタンスを harness に通し `setup -> generate -> test -> score` する）— を提供する（`main.rs` の `Command` enum）。ライブラリは `model` / `loader` / `download` / `harness` / `scorer` / `dashboard` を公開する（`lib.rs`）。

## 不変条件

- **load 経路は純粋・決定論的**: network / clock / env に触れず、テストを hermetic に保つ（`lib.rs` doc の "Design invariant"、`loader::load_instances`）。
- **network に触れるのは `download` のみ、かつ明示呼び出し時だけ**: `download::execute` が `curl` へ shell out する唯一の経路（`main.rs` / `lib.rs` doc）。
- **harness に 2 つの互いに素な経路がある**: `run_instance` は pure/mockable でネットワークを開かず shell out しない（外部作用は注入トレイト `PatchGenerator` / `TestResultSource` の背後）；`run_instance_real` が明示 gate された real 経路（`harness.rs` doc）。
- **real 経路は `--real` からのみ到達**: `RealExecSource::new()` を構築するのは `run_instance_real` だけで、どのテストも構築しない（`RealExecSource` doc、`_private: ()` フィールドと専用コンストラクタで grep 可視化）。したがって `cargo test -p benchkit` は git / pytest / network に触れない。
- **scoring は fail-closed**: 判定は `scorer::is_resolved` に委譲し harness 内で再実装しない（`run_instance` step 3）。結果マップが空なら unresolved になる（`main.rs` の `DrySource` は空 `BTreeMap` を返し dry run を fail-closed にする）。
- **real 経路は over-credit せず fail-closed にパースする**: pytest のターミナル出力は純粋関数 `parse_pytest_output` が per-test の boolean map に変換する。`PASSED` のみ `true`、`FAILED`/`ERROR`（collection/setup エラー含む）は `false`、マーカーの無い行（progress dot・summary・"no tests ran"）は無視する。同一 node id が複数回出たら一度でも失敗があれば `false`（fail-closed）。パースされなかった target は map に現れないので scorer が unresolved 扱いにする。パーサ自体は I/O を持たず、pytest を実行せず fixture 文字列で直接ユニットテストされる。
- **real 経路は heavyweight ライブラリを link せず `std::process::Command` で shell out**（`download` house pattern に一致。`RealExecSource` doc）。

## 振る舞い

- **`benchkit load <path>`**: `loader::load_instances(&path)` で JSONL を読み、`loaded N instances from <path>` と各 `instance_id (repo)` を出力し exit 0；失敗時は `error:` を stderr、exit 1（`main.rs::main` の `Command::Load`）。
- **`benchkit download [--dest <p>] [--force]`**: `download::execute(dest, force)` を呼び、`Outcome::CacheHit(p)` なら `cache hit (no fetch)`、`Outcome::Fetched(p)` なら `fetched:` を出力し exit 0；default 先は `.benchkit-cache/swe-bench-verified.jsonl`、既存キャッシュがあれば `--force` 無しで再取得しない（冪等。`Command::Download` の arg doc）。
- **`benchkit run-instance <path> <instance_id> [--real]`**: `find_instance` で JSONL から id 一致の 1 `Instance` を検索（無ければ exit 1）。`--real` 無しは dry run — `StubGenerator`（空パッチ）と `DrySource`（空結果）で `harness::run_instance` を回す純経路。`--real` は `harness::run_instance_real` に dispatch し git clone + git apply + pytest の gate 経路へ入る。結果は `Verdict` を `<id>: resolved=<bool> (N test results)` で出力し exit 0、失敗時 exit 1（`Command::RunInstance`）。
- **harness パイプライン（`run_instance`）**: (1) `PatchGenerator::generate` で候補パッチ生成、(2) `TestResultSource::results` で per-test の `BTreeMap<String, bool>` 取得、(3) `scorer::is_resolved(&results, &instance.fail_to_pass, &instance.pass_to_pass)` で採点し `Verdict { instance_id, resolved, results }` を返す。各段は `with_context` でエラーに instance_id を付す。
- **`run_instance_real`**: `RealExecSource::new()` を `run_instance` の test-result seam に差すだけの薄い合成（scoring は mock 経路と共有）。`RealExecSource::results` は workdir を temp_dir に作り、`git clone`/`git checkout base_commit` を実行し、`test_patch` と candidate を（空でなければ）`git apply` で stdin から適用し、各段が失敗すれば instance_id 付きで `anyhow::bail!` する。続けて `FAIL_TO_PASS ∪ PASS_TO_PASS` を対象に `python -m pytest -rA --tb=no -q` を回し、stdout+stderr を `parse_pytest_output` で per-test map に変換して返す。map は `run_instance` の共有 scoring に渡り、resolved は「全 target test が pass」で決まる。パース未実装の `bail!` は撤去済み（`--real` はもう bail しない）。

## 事後サンプリング較正ループ (auditsample)

> **REVIEW-NEEDED**: コードから逆算 (2026-07-09 セッション)。人間レビュー前は正典としない。

### 概要

`propguard` / `specguard` / `mutategate` / `blastguard` などの auto-gate は、全部通れば即座に変更を land させるが、その gate 自体が「どれだけ効いているか」は誰も測っていない — 例えば mutategate の 0.80 のような閾値は固定の目安のままで、静かな劣化（silent decay）に気づけない。`auditsample`（`crates/benchkit/src/auditsample.rs`）はこのループを閉じるための post-hoc 較正機能で、`benchkit auditsample <changes.jsonl> [--audits <audits.jsonl>] [--fraction <f>] [--seed <u64>] [--json]` として CLI に生えている（`main.rs` の `Command::AuditSample`、既定 `fraction=0.1`・`seed=0`）。auto-gate だけを通って land した変更の母集団から決定論的にサンプルを抽出し、より厳格な監査（audit）に回した結果を、(a) propguard/specguard 向けの新規不変条件候補と (b) ratify queue 行きの閾値調整提案という 2 つの独立したフィードバック経路へ振り分ける。

### 不変条件

- **決定論的サンプリング**: `sample()` は呼び出し側が渡す `seed` と splitmix64 PRNG のみで駆動し、`Date.now()` や無シードの rand は使わない。母集団は `change_id` でソートしてから seeded Fisher–Yates を適用するため、結果は `(母集団の集合, fraction, seed)` のみに依存し、入力の行順や壁時計に一切依存しない（同じ引数なら常に同じサンプル。`sample_is_deterministic_for_a_seed` / `sample_is_independent_of_input_order` テストで担保）。`fraction` は `[0.0, 1.0]` にクランプされ、空母集団や `fraction=0.0` でも panic せず空サンプルを返す。
- **決定パスに LLM を置かない**: 監査結果 (`AuditResult`) はこのループが消費する構造化入力として扱われるだけで、監査官（adversarial reviewer）を LLM でモデル化することはスコープ外（モジュール doc）。
- **閾値提案は絶対に自動適用されない**: `route_feedback()` はどの gate の生きた設定も mutate しない純関数で、I/O を一切行わない。(b) の `ThresholdProposal` は必ず ratify queue に積まれ、人間が accept/reject するまで適用されない — これは `route_feedback` を二重に呼んでも `ratify_queue` が同じになる（副作用が無い＝冪等）ことでテスト（`ratify_queue_never_auto_applies`）されている。
- **サンプル外の監査結果はループに影響しない**: `execute()` は監査ファイル (`--audits`) 中の全 `AuditResult` のうち、実際にサンプルへ入った `change_id` のものだけを `route_feedback` に渡す。監査対象を追加で渡しても、サンプリングされていない変更はフィードバックへ反映されない（`execute_end_to_end_routes_only_sampled_misses` テスト）。
- **2 つのフィードバック経路は互いに独立**: 1 件の miss が `invariant_hint` を持てば (a) へ、`gate` を持てば (b) へ、それぞれ独立に寄与する。どちらも空文字なら miss はどちらの経路にも寄与しない（`miss_without_hint_or_gate_is_partially_routed`）。(b) は gate 単位に集約され、`miss_count` と（証跡として）辞書順最小の `change_id` を保持する。

### 振る舞い

1. `benchkit auditsample <changes>` で `GatePassedChange`（`change_id` + 通過した `gates` の一覧）の JSONL を読み込む — auto-gate のみを通過した変更の母集団。
2. `sample(population, fraction, seed)` が母集団から `round(fraction * n)` 件（`[0, n]` にクランプ）を決定論的に抽出し、`change_id` 順にソートして返す。`--audits` を渡さなければここで止まり、サンプル一覧（次に人間/より厳格な監査官が監査すべき変更）を人間可読 or `--json` で出力する。
3. `--audits <audits.jsonl>` を渡すと、より厳格な audit の verdict（`AuditResult { change_id, miss, gate, invariant_hint }`）を読み込み、サンプルに入っている `change_id` のものだけへ絞り込んでから `route_feedback()` にかける。
4. `route_feedback()` は miss (`miss == true`) だけを見て 2 経路に振り分ける:
   - **(a) 新規不変条件候補** (`invariant_candidates`): `invariant_hint` が付いている miss ごとに `InvariantCandidate { change_id, invariant }` を生成 — propguard/specguard が拾うべき新しい機械検証可能な性質の提案。
   - **(b) 閾値調整提案 = ratify queue** (`ratify_queue`): `gate` が付いている miss を gate 単位で集約し `ThresholdProposal { gate, change_id, miss_count }` を生成 — 該当 gate の閾値を締める提案だが、**人間が ratify するまで一切適用されない**。
5. 結果 (`Feedback`) は人間向けサマリ（`report_human`）か `--json`（`report_json`）で出力される。入力が読めない/壊れている場合は exit code 2、正常終了は 0（`execute()`）。
