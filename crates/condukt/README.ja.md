# condukt

Claude Code 向けの決定論的オーケストレーションエンジン。大きな課題を解釈・分割・並列実装・検証・完了ゲートまで一サイクルで回す。

## 目的

condukt は、複数ステップ・複数ファイルにまたがる大きめの課題を、合意駆動で最後まで回すオーケストレーターである。

大きなタスクは多数の小さなタスクへ分解される。リクエストを解釈し、各ピースを実装し、基準に照らして検証するという判断は LLM の仕事だ。しかし、*どのタスクを並列実行できるか*の決定、*git ワークツリーの管理*、*実行状態の追跡*、そして*本当に完了したかどうかの判断*は、言語モデルの目視に頼るべきではない。condukt はこの二つを明確に分離する。

```
LLM  (/condukt スキル + interpreter/researcher/worker/verifier エージェント)
  ├ リクエストを解釈する          ─┐
  ├ タスクへ分解する (JSON)         │   condukt バイナリ（決定論的）
  ├ 各タスクを実装する             ├──▶ スケジューリング: 競合分析 → 並列/直列バッチ
  └ 基準に照らして検証する          │    ワークツリー: 作成 / マージ / 削除 / クリーンアップ
                                  ─┘    状態管理: 実行追跡 + 完了ゲート
```

バイナリは単一の Rust 実行ファイルで、ジョブごとに 1 つのサブコマンドを公開する。サブスクリプションネイティブな設計のため、プラグインユーザーは `ANTHROPIC_API_KEY` も追加インストールも不要だ。処理はスキル、4 つのエージェント、1 つの SessionStart フック (`restore`) と 1 つの Stop フック (`state record-run --all`) を介して Claude Code の中で実行される。

## どうして必要か

LLM 単体で大きな課題をオーケストレーションさせると、決定論的に扱うべき判断まで言語モデルの目視に委ねてしまい、次のような失敗モードに陥る。

- **並列化の取り違え。** どのタスクを同時に走らせてよいかをモデルが「だいたい」で判断すると、同じファイルに触れるタスクが衝突する。condukt は `touched_files` の競合分析と依存関係をもとに、衝突しないタスクだけを同一バッチへ入れる（`schedule`）。プロジェクト全体に関わるファイルは `shared_globs` 設定で直列実行に降格させ、ワーカーから保護する。
- **ワークツリーの取り回し。** 並列実装には worktree が要るが、リポジトリ外への配置・1 ディレクトリ = 1 ブランチといった規律を手作業で守るのは脆い。condukt が作成・マージ・削除・クリーンアップのライフサイクルを強制する。
- **「終わった」の誤判定。** 全タスクが本当に検証済みで、ダーティな worktree や未削除の worktree が残っていないか——この完了判定をモデルの感覚に任せると取りこぼす。condukt は `state gate` がそれを満たすまで非ゼロで終了し、完了宣言を物理的に止める。
- **セッションをまたいだ状態の喪失。** クラッシュや中断で実行が止まると、どこまで進んだか分からなくなる。condukt は実行状態を永続化し、再開・stale 状態の自動修復（マージ済みブランチを `verified` へ昇格）・進捗の集計を担う。

判断（解釈・実装・検証）は LLM、決定論（衝突解析・スケジュール・worktree・状態・完了ゲート）はバイナリ、と役割を割り切ることで、再現性と安全性を担保しつつ LLM を本来得意な仕事に集中させる。

## どう使うか

### 起動

スキル `/condukt <課題>` で、解釈→分割→合意→並列実装→検証→統合を一サイクル回す。合意（`AskUserQuestion`）は main loop でしか行われず、未合意のタスクが実装に渡ることはない。`--dry-run` を付けると、スケジュール提示の段階で止まる。`--resume` で停止中の実行を再開できる。

バイナリの有無は `condukt --version` で確認できる。無ければスキルがプラグイン導入（README）を案内する。

関連スキルとして、`/condukt-loop --module <server|client|e2e>` がある。テスト失敗→コード修正→再テストを自動で繰り返し、テスト全件パス、または進捗ゼロ（`failure_count` 不変）で自動停止する。

### エンジンのサブコマンド

| サブコマンド | 目的 |
|---|---|
| `condukt schedule` | 分解 JSON を読み込み、順序付けられた並列バッチと直列/ゲートリストを出力する。2 つのタスクが同一バッチに入るのは、`touched_files` が競合せず、かつ互いに依存関係がない場合のみ。 |
| `condukt validate` | 分解 JSON を検証する（一意な ID、既知の依存関係、循環なし）。 |
| `condukt worktree create/merge/remove/cleanup/list` | git ワークツリーのライフサイクル管理。「リポジトリ外のパス」と「1 ディレクトリ = 1 ブランチ」を強制する。 |
| `condukt state init/set/show/gate/list` | 実行中のタスクステータスを永続化する。`gate` はすべてのタスクが検証済みで、ダーティ/未削除のワークツリーがなくなるまで非ゼロで終了する。`state set` は `--model`/`--cost` を受け付け、記録された結果に実モデルとコストを反映できる。`set --status verified` は下記の F→P 再現性ゲートも強制し、有効な Fail→Pass オラクルを持たない `fix`/`feature` タスクの verified 昇格を拒否する。 |
| `condukt state check-oracle --run <id> --task <id>` | `fix`/`feature` タスクが有効な Fail→Pass 再現証明を持つかを判定する。対象タスク（`kind` が `fix`/`feature`）かつ `reproduction_tests` があるとき、そのタスクのワークツリー内で `tdd oracle --task <id>` を実行し、`{"required","valid_fp_oracle","fallback","transition","reason"}` を出力する。フェイルソフト: `tdd` が不在/到達不能・判定が読めない場合は `fallback:true`（従来ゲートへ縮退）を返し、panic も非ゼロ終了もしない。 |
| `condukt state check-criteria --run <id> --task <id>` | タスクの `done_criteria` に対し機械的ゲートを実行する。満たせば（または非機械的なら）exit 0、失敗すれば exit 1。JSON の `skip_verifier` は「純粋に機械的な基準が pass したとき」だけ true になり、スキルは LLM verifier を省略できる（振る舞い系の基準は常に `skip_verifier:false`）。 |
| `condukt state conflict-check/abandon/list-tasks/cancel/pause/resume` | クロスセッションの安全性と実行の編集。`init` 前のファイル/ゴール競合検出、スタックした `running` タスクの `pending` への差し戻し（`--all-stuck`）、タスクの一覧/キャンセル、競合する実行の一時停止/再開。 |
| `condukt state claim/release/heartbeat/claims` | **クロスセッションのファイル占有レジストリ**（`<state>/<project>/claims.json`）。`conflict-check` の「一度きりの助言的スナップショット」を、生きた強制リースに変え、同一マシン上の 2 つの session が同じ作業を二重に処理しないようにする。`claim --run <id> [--file ...]` はタスクの `touched_files`（省略時は run の decomposition 全体）を占有し、別 run の **live な保有者**が持つファイルは **hard-skip**（exit 1・衝突相手を JSON 出力）する。`release` は解放（全体 or `--file`）、`heartbeat` は生存更新（多忙な session が reap されないように）、`claims` は現在のレジストリを表示。強制は自動：`state set --status running` がタスクのファイルを自動占有し、live 衝突時は **skip JSON を出して exit 1**、terminal 遷移で自動解放する。stale な占有（heartbeat が stuck-TTL より古い）は reap される（liveness は ephemeral な CLI pid ではなく heartbeat でアンカーする）。全操作 fail-soft で、per-run ロックにより直列化される。 |
| `condukt state claim-task --run <id> [--session <s>] [--title <t>] --hashkey <h>...` | **タスク単位**のクロスセッション占有（同じ `claims.json` の `task_claims` テーブル、ファイル占有とは別枠）。opaque な `hashkey`（backlog 側で計算）でタスクを占有し、ClaimOutcome を JSON 出力。別 run の live 保有者がいれば hard-skip（exit 1）。backlog/flow はこの exit code をゲートとして使う。 |
| `condukt state release-task --run <id> --hashkey <h>...` | 指定 `hashkey` のタスク占有を解放し、解放件数を出力する。 |
| `condukt state is-claimed --hashkey <h>` | 指定 `hashkey` を live な claim が（どの run からでも）保持していれば exit 0、保持していなければ exit 1。`{"hashkey","claimed","holder_run"}` を JSON 出力するが、**契約は exit code**。backlog は重複投入防止に `condukt state is-claimed --hashkey <h>` を呼ぶ。 |
| `condukt state execution-state` | live なタスク占有を backlog の pending タスクと突き合わせ、`execution-state.json` に書き出して結果行を JSON 出力する（誰が何を実行中かのビュー）。 |
| `condukt state autonomy-check` | condukt が autonomous モードかを報告する（config `autonomous` + 環境変数 `CONDUKT_AUTONOMOUS`）。`{"autonomous":<bool>}` を出力し、autonomous なら exit 0、そうでなければ exit 1。これによりスキルは autonomous のときだけ人間ゲート（Phase 3 の合意など）を決定論的に縮退できる。既定は false（既存の `AskUserQuestion` はすべて発火＝後方互換）。 |
| `condukt state worktree-mode-check` | condukt が single-worktree モードかを報告する（config `single_worktree` + 環境変数 `CONDUKT_SINGLE_WORKTREE`）。`{"single_worktree":<bool>}` を出力し、on なら exit 0 / off なら exit 1。有効時のみスキルは全タスクを main ツリーで実行する（選択的ステージング、タスク毎の worktree/merge なし）。 |
| `condukt state checkpoint/rollback --run <id>` | autonomous 続行のための可逆性セーフティネット（charter #7）。`checkpoint` は run の状態＋各タスクのブランチ SHA を永続スナップショットしイベントを journal に記録、新しい seq を出力する。`rollback` はスナップショットした状態を復元し、各 worktree を記録した SHA へ best-effort で `git reset` する（`--to <seq>` で指定、既定は最新）。 |
| `condukt state verifier-model --worker <model> [--suggested <model>]` | verifier モデルが worker モデルと決して一致しないよう解決する（共有ブラインドスポット対策）。異なる `--suggested` は尊重し、無ければ別ティアを選ぶ。選んだモデルを出力する。 |
| `condukt consensus plan/vote` | マルチサンプル self-consistency（opt-in のコストガード）。`plan` はタスクを N 個の候補実装に fan-out すべきかを決める（exit 0 = fan-out、1 = 単一サンプル）。`vote` は N 個の verifier 判定を決定論的な多数決の勝者＋合意率に集計し、全 fail・同票・閾値未満の合意率のときは opus へエスカレーションする。 |
| `condukt policy decide/answer/answers` | 中央集権的な **graded-autonomy ポリシー**: 決定の risk × reversibility × confidence を `auto`/`escalate`/`block` に写像する（exit code 契約: 0=auto, 2=escalate, 3=block, 1=不正入力）。`answer` は `auto` 判定のとき 1 問を非対話的に解決し（選択を journal に記録）、それ以外はフォールスルーして呼び出し側が実際の `AskUserQuestion` を出す。`answers` は auto 応答の監査証跡（人間に問わず self-answer した全質問）を出力する。 |
| `condukt verify digest/runtime/launch/regressions/confidence/checks` | 決定論的な verifier ステージのヘルパー（整形のみ。修正の判断は LLM worker に残る）。`digest` は生のテスト出力を構造化 `FailureDigest` に蒸留、`runtime` はターゲットのランタイム出力（exit code・panic/例外行・stderr/stdout の末尾）を蒸留し `--reflux` で pass/fail 判定、`launch` は blastguard 検証済みのエンベロープ内で実ターゲットを起動（破壊的な `--cmd` は fail-closed で拒否）しランタイム信号を reflux する（`--health-url` 指定時は exit を待たず HTTP 200 をポーリング）。`--docker`（既定 image `--image alpine:latest`）を付けると `--cmd` を `docker run --rm --network=none` の隔離コンテナ内で実行する（blastguard ゲートは docker 起動前に同様に適用、docker 自体が不在/デーモン不通なら `note:"docker_unavailable"` で fail-soft）。`regressions --baseline <f> --current <f>` は2つの失敗テスト集合を純粋な集合差分（`current - baseline`）で比較し、verifier の回帰判定を目視でなく決定論化する。`confidence --check-executed --exit-zero --no-regressions` は LLM の自己申告ではなく観測事実から `high|medium|low` を導出する。`checks --file <task.json> [--cwd <dir>]` はタスクが宣言した `checks[]`（下記スキーマ参照）を機械オラクルとして実行し `{"all_passed":bool,"results":[...]}` を出力する。すべて fail-soft（exit 0）。 |
| `condukt replan handoff/stats` | 決定論的な reflux カスケードのヘルパー（分類/整形のみ。再分解の判断は LLM に残る）。`handoff` は失敗タスクの reflux 事実を `escalate_model` か `replan` に分類し、`replan` のときだけ interpreter に「新しい分解を作れ」と指示する handoff を組み立てる（`--run <id>` で決定を journal 記録）。`stats --run <id>` はそのログを directive 毎の件数に集計する。 |
| `condukt circuit check --run <id> [--streak-cap N] [--idle-ttl-secs S] [--budget-cap-usd C]` | 決定論的な CIRCUIT-BREAKER 停止条件ゲート: run の連続失敗ストリーク・idle/stall・(任意) 予算超過の3信号をすべて fail-soft に収集し、純粋な `decide_circuit` コアを実行、判定＋信号を JSON 出力して journal に記録し、breaker が作動したら非ゼロで終了する（`if ! condukt circuit check --run RID; then stop; fi`）。 |
| `condukt gate check --run <id> --task <id>` | `gated` タスク向けの決定論的 GATE-EXEC 判定: アクション文（risk × reversibility）を分類し autonomy policy を読む（すべて fail-soft）。`decide_gate_exec` を実行して判定＋信号を JSON 出力し、auto-exec 前に run をチェックポイント（可逆にする）した上で、escalate なら非ゼロで終了する（`if ! condukt gate check --run RID --task T; then escalate; fi`）。 |
| `condukt escalate add/list/resolve` | 永続的な非同期エスカレーションチャネル（`<state_dir>/<project>/escalations.json`、atomic write・fail-soft）。`add --run --task --question --option <o> [--recommend N]` は out-of-band な質問を登録し `id` を出力、`list --run [--json]` は run の未解決エスカレーションを表示、`resolve --id --choice` は選択した回答を記録し、ブロック/gated タスクがインラインの `AskUserQuestion` で止まらず再開できるようにする。 |
| `condukt pr create --title <t> [--execute]` | 外部ループの終端ステップ: `gh` CLI で PR を開く。`--execute` なしでは dry-run で実行される argv を出力するだけ。`/condukt` スキルは人間の GATED 承認後にのみ `--execute` を渡すため、autonomous 実行が独断で PR を開くことはない。gh 自身の認証を使う（API key 不要）。gh 不在/未認証なら local-commit-only に縮退し exit 0（fail-soft）。 |
| `condukt state stats` | すべての実行（完了・未完了）を集計する: 完了率、タスク数、ステータス分布。ビフォーアフターのベンチマークとして有用。 |
| `condukt state reconcile --run <id> [--dry-run]` | 対象ブランチがデフォルトブランチへマージ済み、または worktree ごと削除済みのタスクを自動的に `verified` へ昇格させる。手動の `state set` なしに、セッションクラッシュ後の古い状態を修正する。**クロス run 重複ガード:** この自動昇格の前に、この run が完了（`done`/`verified`）させた hashkey を、別の `run_id` が**この run の `claimed_at` より後に**同じく完了させていないか兄弟 run を横断走査する。見つかった場合は何も変更せず、`{"duplicate_completion":[{hashkey,runs:[run_id...]}]}` を出力して **exit 2**（escalate → どちらの実装を残すかは人間 / HOTL が選ぶ。condukt の 0=auto / 2=escalate / 3=block 慣例に従う）で終了する。重複が無い通常パスは従来どおり（自動 verified、exit 0）。 |
| `condukt state resume-context --run <id>` | 停止した実行をセッションをまたいで再開するために、保留中/失敗/完了タスクを JSON として出力する。 |
| `condukt state record-run --all` | fugu-router 向けに実行結果を決定論的に記録する（Stop フックが発火、`recorded_at` で冪等、fugu-router 不在ならソフトに no-op）。 |
| `condukt learning-signal` | replan ログの `replan_count` × 検索台帳の `hit` フラグを `run_id` 単位で突き合わせ、`mean_replan_reduction_ratio = 1 - (mean_hit / mean_miss)` を算出する — cross-task 学習の決定論的な計測面（フェイルソフト、グループが空か `mean_miss == 0` のときは `ratio` が `null`）。 |
| `condukt state test --run <id>` | リポジトリルートからプロジェクトのテストスイートを実行し、終了コードを伝播する。優先順位は `[test].command` → 自動検出（`cargo test` / `npm test` / `pytest`、最後は `cargo test` にフォールバック）。`sh -c` 経由のためパイプ・クォート・環境変数展開が使える。 |
| `condukt loop --module <server\|client\|e2e>` | 指定モジュールのテスト修正サイクルを 1 イテレーション実行し JSON を返す。`/condukt-loop` が修正ステップを挟んで繰り返す。 |
| `condukt knowledge` | インタープリター/ワーカープロンプトへ注入するプロジェクト固有の規約/落とし穴を出力する（ソフト、無ければ空）。 |
| `condukt editgate` | PostToolUse フック: worker が live worktree 内の Rust ファイルを Edit/Write した直後、**edit-time コンパイルゲート**がその編集でクレートが壊れたかを決定論的に判定する。本当に壊れた判定のときだけ `{"decision":"block","reason":<診断>}` を出力し、worker が同じターンで直せるようにする。それ以外はすべて fail-soft（何も出力せず exit 0）。 |
| `condukt restore` | SessionStart フック: 未完了の実行や孤立した worktree を通知する。 |
| `condukt statusline` | `statusLine` 設定用の 1 行実行進捗表示。 |
| `condukt status [--all]` | open run とそのタスクを ASCII ツリーで表示する（`--all` でクローズ済み run も含む）。 |
| `condukt init / install / uninstall` | `~/.condukt` を作成し、手動でフックを設定する（プラグインユーザーは不要）。 |

インタープリターエージェントが出力し、`schedule` が消費する分解スキーマ:

```json
{ "goal": "...", "linked_hypotheses": ["hid1"],
  "tasks": [
  { "id": "t1", "title": "...", "touched_files": ["path/or/glob"],
    "deps": ["t0"], "class": "parallel|serial|gated", "kind": "fix|feature|chore",
    "suggested_model": "sonnet|opus|haiku", "done_criteria": "observable pass condition",
    "checks": [{ "cmd": "cargo test -p x", "expect_exit": 0, "expect_substring": "ok" }],
    "expected_trajectory": { "mode": "strict|unordered|subsequence", "steps": [{ "tool": "Read" }] } }
]}
```

`checks` と `expected_trajectory` はどちらも任意で後方互換（`#[serde(default)]`。
どちらも無いタスクは従来どおりに動く）。`checks[]` は verifier ステージが直接実行できる
決定論的な機械オラクルコマンドを宣言する（`condukt verify checks --file <task.json>`）。
LLM がそのコマンドの pass/fail を判定する必要がなくなる。`expected_trajectory` は worker が
辿るべき tool-call の順序を宣言する。指定されていれば `/condukt` スキルの Phase 6 が worker の
transcript をソフト依存の `trajectoryeval extract`/`check` に通し、既存の出力面のみの
`done_criteria` 検証と並行して経路面を検証する（第2の独立した検証次元）。`expected_trajectory`
が無い、または `trajectoryeval` バイナリが不在なら、このステップは丸ごと skip される
（fail-soft・no-op）。

`kind` は任意で後方互換（`#[serde(default)]`）。**F→P 再現性ゲート**の対象は `fix`
と `feature`（大小文字非依存）だけで、そのタスクは「バグのあるツリーで fail・修正後の
ツリーで pass する」タスク固有テスト（Fail→Pass 遷移）を伴わなければならない。
`condukt state check-oracle` がワーカーの `tdd` red/green 証明を分類し、`state set
--status verified` は遷移が有効な Fail→Pass でない限り昇格を拒否する。つまり「done」は
「done_criteria の文字列が一致した」ではなく「再現が実際に赤から緑へ反転した」ことを意味
する。この経路はすべてフェイルソフトで、`tdd` 不在・`reproduction_tests` なし・`fix`/
`feature` 以外のタスクでは従来の done_criteria チェックへ縮退する。

**cross-task lessons のライフサイクル。** lesson は `stuckguard` がエスカレーションした
とき（繰り返しのスタックパターンが閾値を超えたとき）に書き込まれる。`condukt replan
handoff` は決定論的な字句検索で最も一致する過去の lesson を1件だけ取得し、マッチスコアが
閾値を超えたときだけ、それを `--- UNTRUSTED PRIOR-LESSON ---` の境界マーカーで囲んで
replan handoff に注入する（`replan.rs`）——あくまで参考情報であり指示ではなく、
`done_criteria`/スコープを上書きすることはない。`condukt learning-signal`（前述）は
この同じ lessons フローに対する読み取り専用の計測レイヤーである。

### インストール

#### プラグイン（推奨）

> マーケットプレイスカタログは別の中央リポジトリにある。condukt が公開されたら次の通り。

```
/plugin marketplace add <git-url-of-the-catalog-repo>
/plugin install condukt@yukineko
```

これにより `/condukt` スキル、4 つのエージェント、SessionStart + Stop フック、ビルド済みバイナリがバンドルされる。`condukt init` を一度実行すると `~/.condukt` とデフォルトの `config.toml` を作成できる。

#### 手動（ソースからビルド）

```
cargo build --release
cp target/release/condukt ~/.cargo/bin/      # または PATH の通った場所
condukt init
condukt install --dry-run                    # settings.json の変更をプレビュー
condukt install                              # SessionStart フックをマージ（settings.json をバックアップ）
cp -r skills/condukt ~/.claude/skills/        # agents/ も ~/.claude/agents/ へ
```

削除は `condukt uninstall`。

### 設定

`~/.condukt/config.toml`（デフォルト値）:

```toml
worktree_base  = "~/.condukt/worktrees"  # リポジトリの外でなければならない
default_branch = "main"
max_parallel   = 4                        # 同時ワーカー数のアドバイザリーソフトキャップ
shared_globs   = []                       # このグロブに触れるタスクを強制的に直列実行させる
autonomous     = false                    # true にすると人間ゲート（Phase 3 の合意）を決定論的な既定へ縮退する
single_worktree = false                   # true にすると全タスクを main ツリーで実行する（選択的ステージング、タスク毎の worktree/merge なし）

# `condukt state test` が実行するコマンド（`sh -c` 経由、リポジトリルートから）。
# 省略すると自動検出（cargo test / npm test / pytest）。
# [test]
# command = "cargo test"

# マルチサンプル self-consistency（opt-in のコストガード。既定は OFF）。有効に
# すると高リスクタスクを N 回実装・検証し、多数決で勝者を選ぶ。合意率が低いと
# opus へエスカレーションする。N-sample 生成は N 倍のコスト。per-task の
# `condukt consensus plan --risk high` は enabled = false でも fan-out を強制する。
# samples は上限 5 にクランプされる。
# [consensus]
# enabled   = false
# samples   = 3
# threshold = 0.5

# opt-in のワーカーサンドボックス（既定 OFF）。有効にすると、ワーカーが
# `condukt sandbox run` 経由で走らせる build/test コマンドが、ホスト直実行では
# なく docker exec backend（`docker run --rm --network=none`、CWD を同一パスへ
# read-write bind mount）内で実行される — network + fs 隔離に加え resource limit も
# かかる。docker 不在は fail-soft の `docker_unavailable` verdict に縮退し、
# ホストへフォールバック実行しない。編集自体はホストの worktree 上のまま。
# 隔離されるのは build/test の *実行* だけ。
# [worker]
# sandbox_enabled = false
# docker_image    = "alpine:latest"
# memory_limit    = "512m"   # docker --memory  （省略 = 上限なし）
# cpus            = "1.5"    # docker --cpus    （省略 = 上限なし）
# pids_limit      = 256      # docker --pids-limit（省略 = 上限なし）
```

`shared_globs` は、何もハードコードせずにプロジェクト全体のファイルをワーカーから保護する仕組みだ。例: `["**/models.py", "**/migrations/**", "docs/glossary.md"]`。これに触れる並列タスクは警告とともに直列実行へ降格される。

設定ファイルのキーはすべて実行時に環境変数で上書きできる（`CONDUKT_WORKTREE_BASE` / `CONDUKT_DEFAULT_BRANCH` / `CONDUKT_MAX_PARALLEL`）。`CONDUKT_CONSENSUS=1`/`true` はマルチサンプル self-consistency の fan-out を有効にし（`[consensus] enabled` を上書き。opt-in で既定 OFF）、`CONDUKT_AUTONOMOUS=1`/`true` は autonomous モードで実行する（人間ゲートを縮退。config `autonomous` を上書き。`state autonomy-check` が読む）。`CONDUKT_SINGLE_WORKTREE=1`/`true` は全タスクを main ツリーで実行する（config `single_worktree` を上書き。`state worktree-mode-check` が読む）。`CONDUKT_STUCK_TTL_SECS`（既定 `1800`）は `running` タスクを stuck とみなす経過秒数で、`state abandon --all-stuck` の対象になる。`CONDUKT_WORKER_SANDBOX=1`/`true` はワーカーの build/test をサンドボックス化した docker exec backend 経由で実行する（`[worker] sandbox_enabled` を上書き。`sandbox run` が読む）。`CONDUKT_WORKER_SANDBOX_IMAGE` はサンドボックス実行の image を上書きする（`[worker] docker_image` を上書き）。`CONDUKT_DISABLE=1` はフック専用のキルスイッチで、SessionStart/statusline フックを no-op にする（CI で有用）。

`condukt-loop` のサイクル定義（`config.toml` の `[loop]`）:

| `--module` | ステップ順 |
|---|---|
| `server` | deploy → test |
| `client` | build → test |
| `e2e` | build → deploy → test |

```toml
[loop]
build_command  = "npm run build"
deploy_command = "kubectl rollout restart deployment/api && kubectl rollout status deployment/api"
max_iters      = 10   # 安全キャップ; スキルが強制する
```

内部の仕組みの詳細（Phase 0〜8 など）は `docs/internals.ja.md` を参照。

## ソフト連携

`/condukt` スキルは他のいくつかのプラグインに **ソフト依存** している。各連携はそのバイナリが `PATH` 上にあるときだけ使われ、無ければソフトに no-op になる（condukt がハード依存することはない）。

| プラグイン | スキルでの用途 |
|---|---|
| `fugu-router` | 決定論的なモデルルーティング（`route`）と playbook 検索（`procedures search`）。結果は `state record-run` で書き戻す。 |
| `gauge` | サブエージェント単位/セッション単位のコスト取得（`gauge subagents` ≥ 0.3.0、`gauge session` ≥ 0.2.0）を `state set --cost` に反映。 |
| `hypothesis` | open 仮説を interpreter に注入し、gate 後に `linked_hypotheses` を `awaiting-measurement` に遷移。 |
| `backlog` / `compass` | 引数が「次は何をする」系のとき次の一手を供給（Phase 0-next）。 |
| `schemaguard` | `validate` の前段で分解 JSON を宣言 schema にかける（1 回だけ re-ask）。 |
| `specguard` | gate 後、`specguard.toml` があれば spec-drift 監査。 |
| `deepwiki` | アーキテクチャページを interpreter に注入し、gate 後に `deepwiki refresh`。 |
| `tracekit` / `replaykit` | interpreter→worker→verifier の span を記録し、run を replay golden へ promote。 |
| `trajectoryeval` | Phase 6: transcript から worker の tool-call 軌跡を `extract` し、タスクの `expected_trajectory` と `check` で照合する（`done_criteria` と並ぶ第2の経路面 verifier 次元）。タスクに `expected_trajectory` が無い、またはバイナリ不在なら丸ごと skip。 |
| `curate` | golden 化: `done_criteria` が機械的な `verified` タスクに対し、スキルが HOTL 確認（`AskUserQuestion`）を1回提示する。明示的に yes のときだけ `curate promote "<task.title>" --dataset <name>` を実行して evalkit golden へ promote し、否なら何も書き込まない。 |

## ライセンス

MIT
