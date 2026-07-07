# compass

ゴール再接地と「次の一手」導出を行う、Rust 製の Claude Code プラグイン。[condukt](../condukt) の**上流**に座り、「何をやるか」を決める（condukt は「どうやるか」を決める）。

## 目的

compass は、プロジェクトの方向（北極星）と完成定義（definition-of-done）を鋭く彫り直し、現状との gap を読み、そこから導いた**右サイズな一手だけ**を condukt へ渡す再オリエンテーション層である。

ゴールと完成定義は `.compass/charter.md`（リポ同居の「生きた一枚」）に保持する。各セクションは次のとおり:

| セクション | 持つもの |
|---|---|
| `north_star` | このプロジェクトが究極的に何のためか（1〜2 行） |
| `definition_of_done` | 観測可能な完成条件（condukt の `done_criteria` と同じ語彙） |
| `measuring_stick` | 次の一手をどう測るか（既定: 防御可能性 × ゴールへの近さ ÷ コスト） |
| `current_gap` | ゴール − 現状の要約（毎ラウンド再計算） |
| `next_action` | 再開時の最初の物理的な一歩（Stop の breadcrumb が書く） |
| `parked` | 保留した仕事へのポインタ（本体は taskprog の progress.md に置く） |

判定（ゴールを彫る・gap を読む・一手を選ぶ）は Claude Code セッション内の LLM 労働であり、バイナリは状態維持と決定論的な context 生成のみを担う。バイナリ自身は LLM も `AskUserQuestion` も呼ばない。**subscription で完結**し、API キーは不要。

## どうして必要か

プロジェクトには「これは何のためだっけ、次に何をすればいい？」と糸を見失う瞬間が繰り返し訪れる。compass はこれを一つの痛みの二つの顔として扱う — **ゴールが霞む**（完成の定義が言えない）と、その結果生じる**一区切り後の空白**（終わったが次が無い）。

多くのツールは候補を列挙して空白に答える（対症療法）。compass はそうではなく、ゴールを鋭く保ち、次の一手をそこからの勾配（gap）として導く:

```
次の一手 = (鋭いゴール / 完成定義) − (現状: git・progress.md・deepwiki) の 最大かつ右サイズな差分
```

harness 内での住み分け:

| 問い | 担当 |
|---|---|
| 現状は？（done / 残り / blocker） | `taskprog`（progress.md） |
| 構造は？ | `deepwiki` |
| 実装は仕様から drift したか？ | `specguard` |
| **これは何のため・「done」とは・次の一手は？** | **`compass`** |
| 渡されたタスクの分解 / スケジュール / 実行 / 完了ゲート | `condukt` |

condukt は**渡されたタスク**を構造化するが、*どの*タスクを選ぶかは誰も決めていない。compass はこの **方向 → 実行** の鎖を閉じる。compass の出力（彫れたゴール＋合意した一手）が condukt の入力になる。これが無いと、ゴールが霞んだまま候補列挙に流され、焦点が複数に分裂して「次の空白」が構造的に解消されない。

## どう使うか

slash command `/compass` を実行すると、再オリエンテーションを 1 サイクル回す。skill がループ（`AskUserQuestion` を所有）を駆動し、バイナリがステートレスで決定論的な操作を供給する:

```
evaluate ─► carve ループ ─► charter ─► gap ─► (condukt 分解) ─► route
```

1. **evaluate** — バイナリが C1/C2 の決定的 floor を `{open_questions, status, round}` として出力し、carve 状態を初期化/ロードする。skill が C3〜C5 の問いを上乗せする。
2. **carve ループ** — 問いが残り round 予算がある間、skill が **1 問ずつ** `AskUserQuestion` で問う。`compass apply --answer <JSON>` が回答を畳み込み、永続化し、C1/C2 を再評価する。
3. **charter** — `compass charter --write <JSON>` で、観測可能な DoD を持つ彫れた charter を保存する。
4. **gap** — `compass gap` が入力（DoD / 直近の活動 / progress）を決定論的に組み立て、skill が delta を推論して `compass gap --write <text>` で書き戻す。
5. **condukt 分解** — 合意した一手をタスク化し、condukt が `size`（xs|s|m|l|xl）付きの分解 JSON に分割する。
6. **route** — `compass route` が size で triage する。**焦点保護（B案）**: 右サイズ（既定 `s`/`m`）の一手を **1 件だけ** condukt へ、残りは保留へ。エッジは loop に戻る — **`GoalTooBig`**（全部 l/xl）→ ゴールを小さく彫り直す、**`OnlyNoise`**（全部 xs）→ north_star を問い直す。

保留は taskprog の progress.md「残り」へ 1 行ずつ書き戻され、次回 `/compass` の gap 入力に再浮上する（自己供給ループ）。

**計測ループ（outcome）**: 一手の完了で出荷はされるが、出荷は検証された学習ではない（build ≠ validate）。`compass outcome --verdict <forward|unchanged|backward> --evidence <計測値>` で measuring_stick 判定を `.compass/outcomes.json` に記録する（**証拠必須**で、計測なしの outcome は拒否される）。次の `compass gap` が `last_outcome` として読み戻すので、各ラウンドが「計測された進捗」を反映する。[`/flow`](../flow) 経由なら sink が自動記録する。

### 2 つの hook

どちらも決定的・非ブロッキングで、エラー時も exit 0（再接地 hook はターンを壊してはならない）:

- **SessionStart = `compass nudge`** — C1/C2 の決定的 floor のみ（LLM 不使用）。charter が無い/霞む/drift 疑いなら「`/compass` で再接地を」と一行 nudge する。
- **Stop = `compass breadcrumb`** — 本体の最終応答から明示的な ```` ```compass-next ```` ブロックを抽出し `charter.next_action` へ書き戻す。推測はせず、ブロックが無ければ何もしない。

### サブコマンド

| サブコマンド | 目的 |
|---|---|
| `compass nudge [--json]` | SessionStart 鮮度 nudge（C1/C2 floor）。`--json` は `{fresh, reason}` を出力し、下流 driver（例: flow）が同じ floor で gate できる |
| `compass breadcrumb` | Stop hook: 次の物理的な一歩を charter へ書く |
| `compass evaluate` | C1/C2 の open questions を JSON 出力し carve 状態を init/load |
| `compass apply --answer <JSON>` | 人間の回答を 1 つ畳み込み、C1/C2 再評価、永続化 |
| `compass carve-reset` | 永続化された carve 状態をクリア |
| `compass gap` / `--write <text>` | gap 入力の組み立て / gap テキストの永続化 |
| `compass route [--file <path>]` | 分解を size triage し、残りを park |
| `compass charter` / `--write <JSON>` | charter＋config の表示 / 彫れた charter の永続化 |
| `compass outcome --verdict <…> --evidence <text>` | 完了した一手の verdict を記録（証拠必須） |
| `compass pivot-check` | 直近 outcome ストリークから pivot/persevere シグナルを `{recommendation, streak, threshold, reason}` で出力（常に exit 0、flow の gate 用） |
| `compass opportunity add --title <T> [--outcome <ref>] [--weight <f>]` | active outcome（既定は charter `north_star`）配下に named bet（PDO OST）を記録 |
| `compass opportunity list [--json] [--outcome <ref>]` | active outcome 配下の named bet を一覧。`--json` は JSON 配列で出力 |
| `compass discovery record --title <T> [--session-id <ID>]` | 指定（または env 由来）のセッション配下に discovery 行を追記（cross-session dedup 用）。fail-soft、常に exit 0 |
| `compass discovery select [--fingerprint <FP>] [--title <T>]` | 発見済みタスクを `Selected` にする（fingerprint は verbatim か `--title` から導出）。fail-soft |
| `compass discovery list [--json]` | discovery 行を一覧。`--json` は JSON 配列（空なら `[]`）。fail-soft、常に exit 0 |
| `compass c3-screen` | C3「観測可能な DoD」ゲートの advisory 決定的スクリーン。曖昧/非観測可能な `definition_of_done` 項目を `{"flagged":[{"index","item","reason"}...]}` で出す。skill の C3 判定を補完するのみで、ブロックはしない |
| `compass suggest-verdict --tests-delta <N> [--regressions <N>] [--gap-closed]` | `compass outcome` の advisory な決定的デフォルト verdict を観測事実から導出。`{"suggested":"forward\|unchanged\|backward"}` を出力（skill は `outcome --verdict` で上書き可） |
| `compass score --severity <high\|medium\|low> --effort <xs\|s\|m\|l\|xl> --lens <l1..l5> --goal-proximity <f64>` | harness-core のスコアラーを公開した決定的優先度スコア — `(severity × goal_proximity) ÷ effort`、L2（security）/L5（safety）レンズは 1.5 倍で up-weight — scout skill が LLM の手計算なしで候補を順位付けできる。`{"score": f64}` を出力 |

### config（`.compass/config.toml`、すべて任意・既定値）

```toml
[freshness]
stale_commits  = 20          # charter 最終更新からの commit 数（主 drift シグナル）
stale_days     = 14          # 最終更新からの経過日数（副シグナル）
check_dod_refs = true        # DoD が参照する path/symbol の存在確認

[carve]
max_rounds     = 4           # 問答 sync round の上限。0 = 全部 sentinel（sync しない）

[routing]
right_size     = ["s", "m"]  # B案: これらの size を condukt へ、残りは park
```

### 導入

Claude Code プラグイン（推奨）として、2 つの hook・`/compass` skill・プリビルドバイナリを同梱する。subscription で完結し、API キーも別途 `cargo install` も不要:

```text
/plugin marketplace add <git-url-of-this-repo>
/plugin install compass@yukineko
```

hook は `${CLAUDE_PLUGIN_ROOT}/bin/compass <sub>` を呼ぶ。`bin/compass` は host に合った `bin/compass-<os>-<arch>` を選ぶ POSIX ランチャで、Linux/macOS どちらでも動く。一致するバイナリが無ければ無言で exit 0 する。ソースからビルドする場合は `scripts/build-plugin-bin.sh compass`（macOS バイナリは CI または Mac 上でビルドしてコミットする）。
