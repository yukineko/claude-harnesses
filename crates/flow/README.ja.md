# flow

> 課題の供給（source）から解決手段の実行（executor）までを1本のループで貫く、Claude Code 向けの統合 driver（autopilot 層）。

## 目的

flow は、エージェントをセッションを通して生産的に保つための **2 つの直交した関心**——「次の課題を供給する」ことと「それを実行する」こと——を分離し、片方をもう片方へパイプする driver である。

```
SOURCE（課題の供給）                          EXECUTOR（解決手段の実行）
  compass     … 次の右サイズの一手             ─┐
  backlog     … 確定済みキュー                  ├─▶  condukt（fugu-router がモデル選択）─▶ verify
  hypothesis  … PDO 仮説の build / measure      │
  prompt      … ユーザー直の課題文             ─┘
```

flow 自身は **新しい状態を一切持たない**。ループ制御——どの source を引くか、いつ実行するか、いつ止めるか——という判断は `/flow` skill の中の **LLM** が担い、状態維持・ロック・size routing・モデル選択は **既存のバイナリ**（`compass` / `backlog` / `condukt` / `fugu-router`）に委ねる。flow はそれら決定論レイヤを束ねるだけの薄い層である。

ハーネスの中での位置づけは次のとおり。

| 関心 | 担い手 |
|---|---|
| これは何のためか・次の一手は何か | `compass` |
| 確定済みのキューは何か | `backlog` |
| build / 計測待ちの PDO 仮説は何か | `hypothesis` |
| タスクを分割・スケジュール・実行・完了ゲート | `condukt` |
| どの Claude tier が最も安く通すか | `fugu-router` |
| **source → executor をループで束ね、止め時を判断する** | **`flow`** |

flow は **`/backlog` の上位互換**である（compass の鮮度ゲートと複数 source を上乗せしたもの）。両者は backlog のロックを共有するため直列化され、同時には走らせない。

## どうして必要か

LLM 単体にセッション全体を「自走」させると、課題の選定と実行が混ざり、次のような失敗モードに陥る。

- **盲目実行。** ゴールが陳腐・矛盾・抽象すぎる状態のままキューを流し始めると、的外れな一手を量産する。flow はループ前に compass ゲート（`compass gap`）を通し、charter が鮮明でない限り自動実行せず `/compass` での再オリエンテーションを促して停止する。
- **source と executor の混線。** 「次に何をやるか」と「それをどう実装するか」を同じ判断に押し込むと、優先順位付けが実装の都合に引きずられる。flow は供給（compass/backlog/hypothesis）と実行（condukt）を直交させ、それぞれ独立したストアに保つ。
- **二重ループ。** 複数セッションが同時に課題を流すと condukt run が衝突する。flow は backlog のロックを取得してクロスセッションで直列化し、別セッションが保持中なら待機・強制奪取・中止を問う。さらに backlog item 単位でも `condukt state is-claimed`/`claim-task`/`release-task`/`heartbeat` を使い（`--run "flow-$CLAUDE_CODE_SESSION_ID"`）、同一タスクへの多重着手をピック時（claim-skip）と着手直前（TOCTOU ガード）の二段で防ぐ。
- **build と validate の取り違え。** 仮説を「出荷した」だけで「検証済み」と扱うと、計測ループが閉じない。flow は出荷した仮説を `awaiting-measurement` に残し、次サイクルの measure step が観測値を添えて初めて validate/reject する（build ≠ validate）。
- **止め時の喪失。** 連続失敗や予算超過に気づかず走り続けると無駄に消費する。flow は早期脱出条件を持ち、どの経路でもロック解放を必須にする。

判断（どの source を引くか・実行・検証・止め時）は LLM、状態とロックとモデル選択は既存バイナリ、と割り切ることで、自走の利便を保ちつつ暴走を防ぐ。

## どう使うか

### 起動

skill `/flow` でループを起動する。

- 引数なし → source（compass の主筋 → measure step → backlog → 新規 open 仮説）から優先度順に自動ピックし、condukt に流して検証・sink するループを回す。
- 課題文を直接渡す（`/flow <課題文>`）→ source 選択を飛ばし、その課題を condukt に1件だけ流して終了する（明示課題は「今これをやれ」の意味）。

ループの骨子は次のとおり。

```
0. 引数分岐 — 課題文があれば condukt に直行（1 件だけ実行）
0.5. 自律ゲート — `condukt state autonomy-check`。自律モードなら各 human gate を
     `condukt policy answer`（risk×reversible×confidence の graded 判定）に通す（下記参照）
1. compass ゲート — charter が陳腐なら自動実行せず /compass を促して停止
2. ロック取得 — backlog lock acquire（クロスセッション直列化）
3. 実行ループ — 優先度順にピック（claim-skip ゲート）→ 着手前に claim（TOCTOU ガード）
       → overwatch anchor 登録（overwatch begin）→ /condukt → 検証 → sink
       sink: backlog done / compass outcome（前進・不変・後退を記録）
             / hypothesis は出荷で awaiting-measurement、計測後に validate/reject（証拠必須）
             / fugu-router に record
             / いずれも claim を release、ループ中は heartbeat で claim を live に保つ
4. ロック解放 — source が尽きる/予算超過/中断で lock release + overwatch end + サマリ報告 + pivot-check
```

### PDO session anchor（`overwatch begin`/`end`）

Step 3-1 で課題文を組み立てた直後、**どの source を選んだか（compass 主筋 / measure step / backlog /
open 仮説）に関わらず** `overwatch begin --key <pdo-unit-id> --title <title> [--scope <csv>]
[--done-criteria <dc>]` を呼び、そのセッションの現在の責務を project-wide レジストリに登録する
（`overwatch status` で可視。DESIGN §4.2）。これにより **condukt run を起こさない measure step でも**
「今どのセッションが何を担当しているか」がレジストリに乗る。Step 4 で対応する
`overwatch end --key <k> --status <done|abandoned>` を呼び anchor のライフサイクルを閉じる。
バッチ（複数 backlog item）は item ごとに begin/end する。**fail-soft**: `overwatch` バイナリが
無ければ両方 skip して続行する（既存の condukt/backlog/compass 欠落時と同じ方針＝turn を壊さない）。

### 自律ゲート（`condukt policy answer`）

ループ中、本来ならユーザーに `AskUserQuestion` で確認する箇所はすべて、まず**グローバルな自律スイッチ**を通す。

```bash
condukt state autonomy-check   # exit 0 = 自律 / exit 1（既定）= 非自律
```

- **非自律（既定・exit 1、または `answer` サブコマンド未対応）** — 変更なし。後方互換で
  以下のゲートは従来どおり全てユーザーに確認する。
- **自律（exit 0）** — 各ゲートは固有の risk × reversibility × confidence を添えて
  `condukt policy answer` に通され、決定論的な verdict を返す:
  - **auto（exit 0）** — 推奨オプションを自答し、`gate-decisions.jsonl` に記録する
    （`condukt policy answers` で後から監査可能）。**Ask しない**。
  - **escalate（exit 2）** — 従来どおり `AskUserQuestion`（pivot 判断は必ずここに落ちる＝
    genuine な戦略判断は人間に残す）。
  - **block（exit 3）** — 誰にも聞かず拒否して停止する。
  - それ以外（不正入力・`answer` 未対応）は安全側にフォールバックして escalate 扱い。

| ゲート | 典型 verdict | auto 時の既定 |
|---|---|---|
| ロック競合（Step 2・生きた保有者） | auto | stand down（報告して clean exit。`--force` 自動奪取はしない） |
| resume 選択（複数候補） | auto | 既存の優先度 pick 順 |
| pivot-check（Step 4） | **escalate** | —（常に人間の判断） |
| 循環ブレーカー trip（早期脱出） | auto | clean stop |

自律モードでも、次の4つは常に人間で止まる: **(a)** worker blocked のエスカレーション、
**(b)** deploy/push の GATED 承認、**(c)** pivot の判断、**(d)** `policy answer` 自体が
escalate/block を返したゲート。

早期脱出の詳細:

| 状況 | 対応 |
|---|---|
| ユーザーが中断を指示 | 直ちに Step 4（ロック解放）へ |
| 循環ブレーカーが trip（`condukt circuit check` が毎イテレーション failure-streak上限・予算超過・no-progress stall を判定） | 決定論的に clean stop（人にも policy にも聞かない hard stop）。非自律での追加確認は `AskUserQuestion` フォールバックのみ |
| budgetguard が予算超過を返す | ループ終了（予算軸は上の circuit check にも統合済み） |
| compass ゲートが再スコープを示す | ループを止め `/compass` を促す |
| `backlog next` が予期しないエラー | 報告して Step 4 へ |

いずれの早期脱出でもロック解放は必ず行う。

### SessionStart hook

flow バイナリは決定論的・非ブロッキングで、エラー時も exit 0 する（driver hook がターンを壊してはならない）。

| Hook | Event | 役割 |
|---|---|---|
| `flow propose` | `SessionStart`（startup/resume/clear） | このセッションに開いている仕事（compass の次の一手・open な backlog・未完の condukt run）があれば、`/flow` を1つの `AskUserQuestion` で能動的に提案する **propose-then-confirm** ディレクティブを注入する。タスク数の再計算はせず（compass `nudge` / backlog `session-start` / condukt `restore` が各自の状態を注入する）、それらを束ねるディレクティブを足すだけ。 |

つまり flow は、開いている仕事があるセッションでは自動で `/flow` を提案し、承認後に起動する。手動でも `/flow` で起動できる。

### サブコマンド

バイナリは意図的に薄い。公開サブコマンドは1つだけ。

| サブコマンド | 用途 |
|---|---|
| `flow propose` | SessionStart hook：propose-then-confirm ディレクティブを注入する |

### 導入

Claude Code プラグインとして入れるのが推奨。hook・`/flow` skill・プリビルドバイナリを同梱し、**subscription で完結**する（API キー不要）。

```text
/plugin marketplace add yukineko/claude-harnesses
/plugin install flow@yukineko
```

hook は `${CLAUDE_PLUGIN_ROOT}/bin/flow propose` を呼ぶ。`bin/flow` はプラットフォーム別バイナリ（`bin/flow-<os>-<arch>`）を選ぶ POSIX ランチャで、一致するバイナリが無いホストでは exit 0 で黙って抜ける。

> flow は source/executor（`compass` / `backlog` / `condukt`、任意で `fugu-router`）がインストールされていることを前提とする。単体で動くものではなく、それらを束ねる driver である。

### ソースからビルド

```sh
scripts/build-plugin-bin.sh flow                       # ホストプラットフォーム
scripts/build-plugin-bin.sh flow x86_64-apple-darwin   # Intel Mac 向けにクロスビルド
git add bin/ && git update-index --chmod=+x bin/flow bin/flow-*
```

## プラットフォーム対応

| ホスト | ファイル | 状態 |
|---|---|---|
| Linux x86_64 | `bin/flow-linux-x86_64` | 同梱 |
| macOS Apple Silicon | `bin/flow-darwin-arm64` | 同梱 |
| macOS Intel | `bin/flow-darwin-x86_64` | macOS runner の CI でビルド |

## プラグイン構成

```
.claude-plugin/plugin.json     # プラグインマニフェスト（version 0.1.6）
hooks/hooks.json               # SessionStart=propose → ${CLAUDE_PLUGIN_ROOT}/bin/flow
skills/flow/SKILL.md           # /flow skill（source→executor ループを駆動）
bin/flow                       # POSIX ランチャ → flow-<os>-<arch>
bin/flow-<os>-<arch>           # プリビルドバイナリ
src/main.rs … Cargo.toml       # Rust クレート
```

## 開発

```sh
cargo build -p flow
```

## ライセンス

MIT
