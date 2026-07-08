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
