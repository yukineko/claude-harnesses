# hypothesis

Claude Code 向けの PDO（プロダクト発見）仮説ライフサイクル管理ハーネス。Rust 製。検証したい「反証可能な賭け」を台帳で追跡し、**出荷 ≠ 検証（build ≠ validate）** を構造的に強制する。

## 目的

hypothesis は、発見作業の **未解決の問い**——「X だと信じている。本当か？」——を持つ台帳である。

[compass](../compass) が「何を目指すか（ゴール）」を、[backlog](../backlog) が「何が確定キューか」を持つのに対し、どちらも「思い込み」そのものは捕捉しない。hypothesis はその欠けた台帳であり、各レコードは状態を持つ反証可能な文だ。これにより、発見作業が「作った→ゆえに正しい」へ静かに崩れるのを防ぐ。

台帳の実体は `~/.hypothesis/hypotheses.toml`。各仮説は次の 4 状態を辿る。「出荷」から終端への 2 つの遷移をあえて分離してあり、merge が検証済みの学びになりすますことを許さない。

| 状態 | 意味 | 入口 |
|---|---|---|
| `open` | 未検証の反証可能な賭け | `hypothesis add` |
| `awaiting-measurement` | 実験を**出荷したが**シグナルは未計測 | `hypothesis await-measurement`（condukt が merge 時に設定） |
| `validated` | 計測した証拠が賭けを**支持** | `hypothesis validate --evidence …` |
| `rejected` | 計測した証拠が賭けを**反証** | `hypothesis reject --reason …` |

`validate` は空でない `--evidence` か `--measurement` が 1 つ以上必須、`reject` は空でない `--reason` が必須（無ければ拒否）。計測した内容を記録せずに終端へは動かせない。出荷だけでは `awaiting-measurement` 止まりだ（build ≠ validate）。各レコードは `id` / `text` / `status` / `evidence[]` / `linked_goal` / `condukt_run` / `success_criterion` / `kill_criterion` / `assumptions[]` / `confidence` / `created_at` / `updated_at` / `shipped_at`（`await-measurement` のたびに更新）/ `scope_write_paths` / `scope_read_paths`（宣言された影響範囲。`draft` だけが記録し、欠落は「問われていない」）を保持する。

これは **subscription-native** で、API キーは不要。判定（この賭けは検証されたか？証拠は何か？）は Claude Code セッション内の LLM／人間の仕事であり、バイナリは台帳の保持と決定論的な SessionStart コンテキストの描画のみを担う。バイナリは LLM を呼ばない。

ハーネス内での位置づけは次のとおり。

| 問い | 担当 |
|---|---|
| 何のためか・次の一手は？ | `compass` |
| 確定キューは何か？ | `backlog` |
| **何を信じていて、まだ証明していないか——その計測結果の判定は？** | **`hypothesis`** |
| タスクの分解／スケジュール／実行 | `condukt` |
| source → executor をループで束ねる | `flow` |

## どうして必要か

ゴール（compass）と確定キュー（backlog）があっても、検証していない前提は宙に浮いたままになる。これを放置すると、発見作業は次の失敗モードに陥る。

- **出荷を検証と取り違える。** 「機能を作って merge した」だけで、その背後の思い込みが正しかったことの証明にはならない。hypothesis は merge 後を `awaiting-measurement` で受け止め、`validate`／`reject` には計測した `--evidence`／`--reason` を要求することで、出荷が学びに化けるのを止める。
- **後出しのゴール改変。** `add` 時に `--success "<metric> >= <n>"` / `--kill "<metric> <= <n>"` で **出荷前に** 反証可能な基準を事前登録できる。事前登録した `--success` がある場合、`validate` には基準を満たす `--measurement` が要り、満たさなければ検証は拒否される（ゴールポストの後ずらしを防ぐ）。
- **発見がゴールから静かに逸れる。** 仮説は `--goal <keyword>` で compass charter のゴールに紐づけられる。`linked_goal` が現在の charter と合致しなくなった賭けは SessionStart で `[unlinked]` と警告される。
- **賭けの土台にある未検証の前提（leap of faith）の見落とし。** `assume` で賭けが依存する信念をリスク／証拠の強さ付きで記録し、`rat` が最もリスクの高い未検証の前提（最初に最小実験で潰すべき飛躍）を提示する。

判定は LLM／人間、台帳の保持と決定論的な描画はバイナリ、と役割を割り切ることで、発見作業の規律を再現性のある形で担保する。

## どう使うか

### プラグインとして導入（推奨）

```text
# Claude Code 内で:
/plugin marketplace add yukineko/claude-harnesses
/plugin install hypothesis@yukineko
```

プラグインは SessionStart フック（`hooks/hooks.json`）、`/hypothesis` と `/add` のスキル、ビルド済みバイナリを同梱する。**subscription** だけで動き、API キーは不要。フックは `${CLAUDE_PLUGIN_ROOT}/bin/hypothesis session-start` を呼ぶ。`bin/hypothesis` はプラットフォーム別バイナリ（`bin/hypothesis-<os>-<arch>`）を選ぶ POSIX ランチャ。該当バイナリが無いホストでは、`session-start` は「実行されなかった（open が無いのではなく不明）」と `systemMessage` + `additionalContext` で可視化し、それ以外の動詞は理由を stderr に出して exit 127 する — 静かな exit 0 にはしない。プラットフォーム別バイナリは **コミットされない**（repo の `.gitignore` 参照。追跡されるのはランチャだけ）。ローカル配備は `scripts/rollout-plugins.sh` がソースからビルドする。

### スキル

- `/add <仮説テキスト> [--goal <compassキーワード>]` — 新しい賭けを登録し、ID を返す。
- `/hypothesis` — 仮説ライフサイクル（追加・検証・棄却・一覧）を管理し、compass のゴールと連動させる。

### 主なサブコマンド

バイナリは薄く決定論的だ。

| サブコマンド | 用途 |
|---|---|
| `hypothesis add <text> [--goal <keyword>] [--success "<metric> >= <n>"] [--kill "<metric> <= <n>"] [--confidence <0..1>]` | 賭けを追加し新 ID を表示。`--success`/`--kill` は出荷前に反証可能な基準を事前登録（演算子: `>= <= > < ==`）。`--confidence` は発見の確信度を設定（既定 0.5） |
| `hypothesis draft <text> [--goal <keyword>] [--success …] [--kill …] [--write-path <p>…\|--no-write-paths] [--read-path <p>…\|--no-read-paths]` | 曖昧なタスク文を問い詰める: 未解決の点（D1 scope / D2 success 基準 / D3 kill 基準 / D4 ゴール紐づけ）を表示し、すべて解消するまで **何も書き込まない**。解消したら宣言済み scope 付きで記録する。書き込み面が空（`--no-write-paths`）は拒否、読み込み面が空は受理。未解決が残る間も exit 0 |
| `hypothesis list [--status <s>] [--json]` | 賭けを**発見順**（confidence 降順、次に created_at）で一覧（`open` / `awaiting-measurement` / `validated` / `rejected` でフィルタ可。未知の値は非0終了で拒否される）。事前登録した基準・宣言 scope（未質問なら `[scope: undeclared]`）・RAT もインライン表示。`--json` は `{id,status,text,confidence,run}` の配列を出力 |
| `hypothesis stats` | 出荷 vs 計測の健全性メトリクスを 1 つの JSON で出力（`shipped` / `validated` / `rejected` / `awaiting` / `avg_measurement_delay_days`） |
| `hypothesis confidence <id> <value>` | 賭けの発見確信度（検証の優先度。高いほど検証に早く浮上）を設定。`list` の発見順キューを並べ替える |
| `hypothesis await-measurement <id> [--run <run>]` | 出荷したが未計測の賭けに印を付ける（condukt が merge 時に呼ぶ） |
| `hypothesis validate <id> [--evidence <text>…] [--measurement "<metric>=<value>"…] [--run <run>]` | 終端: 計測した証拠が支持（evidence か measurement が 1 つ以上必須）。事前登録した `--success` があれば対応する `--measurement` が基準を満たす必要がある |
| `hypothesis reject <id> --reason <text> [--run <run>]` | 終端: 計測した証拠が反証（空でない reason が実行時に必須） |
| `hypothesis assume <id> --text <t> --risk <low\|medium\|high> --evidence <strong\|weak\|none>` | 賭けが依存する信念を記録（RAT による de-risk 用） |
| `hypothesis rat <id>` | 最もリスクの高い未検証の前提を表示（de-risk 済みなら何も出さない）。flow はフルビルド前にこれを実行する |
| `hypothesis tested <id> <index>` | `<index>` の前提をテスト済みにマーク（RAT 後など）し、飛躍として登録されないようにする |
| `hypothesis install [--dry-run]` / `hypothesis uninstall` | settings に SessionStart フックを追加／削除 |
| `hypothesis session-start` | SessionStart フックのエントリポイント（内部用） |

### SessionStart フック

`hypothesis session-start` は決定論的かつ非ブロッキング（判定を持たず、常に exit 0 する `harness_core::hook::run_hook` 経由で走る）。`SessionStart`（matcher 無し＝全ソース、timeout 5 秒）で、（project 別ではなくグローバルな）ストアの **open** と **awaiting-measurement** の仮説を、計測負債サマリ（件数＋最古の経過日数。閾値を超えると警告表示）と `validate`/`reject` のリマインダ付きでコンテキスト注入する。現リポジトリの compass charter と `linked_goal` が合致しなくなった open な賭けは `[unlinked]` と警告される。出力は `inject_limit` バイトで切り詰め。open/awaiting が無ければ何も出さない。**注意**: ストアが読めない（解析失敗・読み込み不能・lock 競合）ときも何も出さないため、沈黙は「open が無い」と区別できない。

### 設定

`~/.hypothesis/config.toml`（無ければ既定値。読めない・解析できない場合も警告なしで静かに既定値にフォールバックする）。

```toml
enabled      = true            # false で SessionStart 注入を無効化
store_dir    = "~/.hypothesis" # hypotheses.toml の置き場所
inject_limit = 2000            # SessionStart フックが注入する最大文字数
```

環境変数 `HYPOTHESIS_DISABLE=1` を設定すると、config を編集せずにフックを黙らせられる。

### flow / compass との連携

hypothesis は [`/flow`](../flow) の **source** だ。open な仮説は build すべき実験になり、`awaiting-measurement` の仮説はループを閉じる **measure ステップ** になる。検証先のゴールを持つ [compass](../compass)、賭けを build・計測する [flow](../flow) と組み合わせて初めてループが閉じる。単体でも動く。
