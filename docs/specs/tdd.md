> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# tdd 仕様

## 概要

`tdd` は Claude Code の実装作業に対して「テストを先に書く（test-first）」を強制し、かつその事実を
検証可能にする決定論ハーネスである。二つの面を持つ。第一に **`tdd gate`（Stop hook 本体）** —
ターンが止まるたびに `git` の working-tree 差分を読み、テストを伴わずに実装コードが追加されていれば
その停止をブロックし、理由（`gate::block_reason`）をエージェントへ返す。第二に
**`tdd red`/`green`/`verify`/`oracle`（`/tdd` skill が駆動）** — RED→GREEN の遷移を
`<proof_dir>/<task>.{red,green}.json` 証跡として記録し、「テストが先に落ちた」ことを機械検証可能にする。
判断（API 設計・テスト記述・実装）は LLM（`/tdd` skill）が担い、決定論（テスト無し実装のブロック・
RED だったか・RED→GREEN になったか）は `tdd` バイナリと Stop hook が担う。`git` を読みテストコマンドを
起動するだけの単一 Rust 実行ファイルで、API キーは不要。

## 不変条件

- **never-break-a-turn（fail-soft の中核）** — `gate` は Claude へ常に exit 0 で返し、停止のブロックは
  exit code ではなく出力 JSON の `{"decision":"block","reason":…}` で行う（`gate_run`）。tdd 側のエラー
  （自前のバグ・git 不読）は panic guard（`harness_core::gate::run::run_guarded`）が hook モードで exit 0
  に潰し、手動 CLI（`interactive`＝stdin に hook payload が無い）でのみ exit 1／panic を表面化する。
- **test-first の非捏造性** — `proof::judge_red` はテストが既に通れば RED 記録を拒否し（`bail!` "not
  test-first"）、`proof::judge_green` は先行 RED 証跡（`<task>.red.json` の存在）が無ければ拒否する。
  よって `<task>.green.json` は RED→GREEN の順序を捏造できない成果物として残る。
- **有効な oracle は Fail→Pass のみ** — `transition::Transition::is_valid_oracle` は `FailToPass` のみ
  真。`oracle_report` は RED/GREEN の `passed` から4遷移を分類し、`valid_fp_oracle` はこの1遷移でのみ真。
  証跡欠落・破損時は `transition:"unknown"`・`valid_fp_oracle:false` を返し panic しない（fail-soft）。
- **project `test_cmd` は workspace-trust ゲート下** — `test_cmd` は `tdd red`/`green` で verbatim 実行
  されるため、`Config::load` は project `tdd.toml` を `harness_core::trust::is_trusted` が真のときだけ
  採用する。未信頼なら一度だけ警告（`warn_untrusted`）を出し home config→built-in default へフォールバック。
  `tdd trust` で明示信頼、`HARNESS_TRUST_ALL=1` で全信頼。
- **キルスイッチと逃げ道** — `TDD_DISABLE=1`（`Config::disabled_env`）で全停止、config `enabled=false`
  でも停止許可。純リファクタ/リネーム/docs 向けに project root の 1 行 `.tdd-skip` ファイル
  （`consume_skip` で1回だけ消費）がある。
- **stuck agent を罠にしない** — セッション単位の attempt counter（`state::bump`／共有
  `harness_core::gate::state`）が `max_attempts`（既定3）連続ブロックで諦めて停止を許す。
  `reset_after_secs`（既定600）のアイドルでカウンタはリセット。
- **決定論・純関数境界** — `gate::classify` は git を呼ばず（`evaluate` が呼ぶ）、changed files＋added
  lines を verdict へ写像する純関数。`transition::classify`／`oracle_report`／`git::parse_unified_diff`
  も純関数で単体テスト可能。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main`）で定義。

- **`gate`（Stop hook 本体）** — stdin の hook payload を `HookInput::parse` → `cwd_or_current` で root
  解決。disabled/`.tdd-skip` を先に処理し、`gate::evaluate`（`git::changed_files`＋`git::added_lines` →
  `classify`）で verdict を得る。`Verdict::blocks` が真（git スコープ有り・追加実装行 ≥
  `min_added_impl_lines.max(1)`・テスト証跡無し）なら attempt を bump し、`max_attempts` 超過で許可、
  それ以外は `decision:block` を出す。テスト証跡は「追加された test-marker 行（`#[test]`/`def test_`/
  `func Test…`/`it(...)` 等の `test_markers` 正規表現）」または「`test_path_globs` に一致する test file の
  変更」（`Verdict::has_test_evidence`）。
- **`red --task <id> [--cmd]`** — `runner::run_cmd` でテスト実行（`resolve_cmd` は `--cmd`＞config
  `test_cmd`）→ `judge_red`（失敗を要求）→ `<proof_dir>/<id>.red.json`（task/phase/cmd/passed/exit_code/
  ts/output_tail）を書く。既に通れば exit 1。
- **`green --task <id> [--cmd]`** — 先行 RED 証跡の存在を確認 → テスト実行 → `judge_green`（RED あり かつ
  pass を要求）→ `<id>.green.json`（red_proof パス含む）を書く。
- **`verify --task <id>`** — RED と GREEN 両証跡が存在すれば exit 0、無ければ exit 1（`proof::verify`）。
- **`oracle --task <id>`** — `proof::read_passed`（fail-soft: missing/corrupt/非 bool は `None`）で両 phase
  の `passed` を読み、`transition::oracle_report` を JSON 出力。有効な Fail→Pass のみ exit 0、他は exit 1。
  `/tdd` skill のフェーズ外の機械オラクルで、`condukt` の Fail→Pass ゲートが呼ぶ想定。
- **`status`** — 解決済み config（source/enabled/max_attempts/min impl lines/test_cmd/proof_dir/state_dir）と
  cwd に対する gate verdict（`gate::human_report`）を表示。
- **`init [--force]`** — starter `./tdd.toml`（`STARTER_CONFIG`）を書く。既存かつ非 force で bail。
- **`install`/`uninstall [--dry-run]`** — `~/.claude/settings.json` の Stop hook を idempotent に
  マージ/除去（`install::MARKERS`＝`"tdd gate"`/`"/tdd "`、`harness_core::install`）。プラグインユーザーは
  不要（`hooks/hooks.json` が `${CLAUDE_PLUGIN_ROOT}/bin/tdd gate` を timeout 30s で配線）。
- **`trust`** — 現在の project root を共有 trust list に追加（`harness_core::trust::add`）し、その
  `tdd.toml` の `test_cmd` を honor させる。

### module 責務

- **`config`** — `Config`（enabled/max_attempts/reset_after_secs/state_dir/proof_dir/test_cmd/
  default_timeout_secs/output_tail_lines/impl_globs/test_path_globs/test_markers/min_added_impl_lines）を
  project `tdd.toml`＞`~/.tdd/config.toml`＞言語対応 built-in default で解決。`FileConfig` を optional
  上書きとして層化し、parse error は黙ってフォールバック（gate はクラッシュしない）。project config は
  workspace-trust ゲート下。値の sanitize（0→既定）を行う。
- **`gate`** — test-existence 判定。`Verdict`（added_impl_lines/test_marker_added/test_file_changed/
  impl_files/git_unscoped）、`classify`（純関数）/`evaluate`（git 実行）/`blocks`/`block_reason`/
  `human_report`。suite は走らせない（それは donegate の責務）。
- **`git`** — working-tree 差分の subprocess 読み取り。`changed_files`（diff/cached/untracked を統合）、
  `added_lines`（`-U0` diff ＋ untracked ファイル全行）、`parse_unified_diff`/`strip_diff_prefix`。
  非 git repo は `None`（gate は許可）。
- **`proof`** — RED/GREEN 証跡の記録・読み取り。`red`/`green`/`verify`/`read_passed`（fail-soft）、
  `judge_red`/`judge_green`、`artifact_path`（task を `safe()` で sanitize）。
- **`transition`** — RED→GREEN 遷移分類。`Transition` enum・`classify`・`is_valid_oracle`・
  `oracle_report`（純関数）。
- **`runner`** — `harness_core::gate::runner` の薄いアダプタ。テストコマンドを timeout 付き subprocess で
  実行し `Outcome`（passed/exit_code/timed_out/spawn_error/output_tail）へ写像。危険な spawn/timeout/tail
  ロジックは harness-core 側。
- **`install`** — Stop hook の settings.json マージ/除去（`harness_core::install` 委譲、idempotent）。
- **`state`** — セッション attempt counter。`harness_core::gate::state` の `bump`/`reset` 再エクスポート。
- **`model`** — `harness_core::hook::HookInput`（`parse`/`cwd_or_current`/`session_key`）の再エクスポート。
