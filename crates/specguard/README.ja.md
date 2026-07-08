# specguard

> 🌐 [English](README.md) ・ **日本語**

**仕様↔実装 整合監査ハーネス (project-agnostic)**

## 目的

実装が「正典 (canonical spec)」からずれていないか、また正典 doc 自体に沈黙・矛盾・
重複がないかを、LLM エージェントに **read-only** で監査させる CLI である。判定の中核は
LLM が逐語引用つきで担い、specguard はその周りの **決定的なハーネス** —
スコープ決定 → プロンプト描画 → エージェント起動 → marker 解析 → レポート/sentinel —
を担う。プロジェクト固有の部分はすべて設定ファイル (TOML) に外出しされている。

```
specguard.toml ──┐
   git diff ──────┼──▶ scope (変更領域 ∪ 不変条件)
                  │         │
templates/ ───────┼──▶ プロンプト描画 ──▶ agent (read-only) ──▶ marker 解析 ──▶ report + sentinel
```

監査は 3 つの次元で行う (正典は `templates/audit-prompt.md` / `decisions-prompt.md`)。

specguard 自体の検証ゲート設計は **[DESIGN-VERIFY.md](DESIGN-VERIFY.md)** を参照。
**[DESIGN.md](DESIGN.md)**(+ [DESIGN-INTAKE.md](DESIGN-INTAKE.md))は同じ crate に同居する
生成側の兄弟ハーネス `specforge`(`src/forge/`、バイナリ名 `specforge`)の設計であり、
specguard を read-only の受け入れゲートとして呼び返す。`/specforge:*` のスラッシュコマンドは
まだ無く、以下で説明する本プラグインの対象範囲には含まれない。

- **D1 実装↔正典 drift**: 実装が正典からずれていないか (矛盾は誤読/コード違反/正典陳腐化に分類)。
- **D2 仕様品質**: 正典 doc 自体の沈黙・矛盾・重複。
- **D3 決定ログ鮮度・陳腐化**: 仕様変更の *理由* を canon commit に pin し、決定が今も成立するか照合 (`[decisions]` で有効化)。

## どうして必要か

仕様 (正典 doc) とコードは、開発が進むほど黙って乖離していく。コードを直したのに doc を
直し忘れる、doc に書いたつもりの不変条件がいつのまにかコードで破られている、あるいは
正典 doc 自体が互いに矛盾していたり同じことを二箇所で言っていたり、肝心な点について沈黙
している — こうしたドリフトはレビューでは見落とされやすく、放置すると「どちらが正しいのか
分からない」状態に陥る。

specguard はこのドリフトを **機械的・反復的に検出する前線**を与える。判定は LLM に任せつつ、
スコープ決定・プロンプト描画・marker 解析・レポート/sentinel 化といった「決定的に正しく
あるべき部分」をバイナリが担うので、判定ロジックを二重化せず再現性のある監査が回せる。

read-only であることが要点で、監査エージェントは書き込み・ネットワーク・任意 shell を
持たない。これは「お願い」ではなく **権限** で担保される (詳細は後述)。

- **standalone**: 既定エージェントは allowlist (Read/Grep/Glob + `git diff/log/show/status`)
  で起動し、書き込み・ネットワーク・任意 shell を deny する。`--print` モードでは allowlist
  外のツールは自動 deny されるため、監査対象リポジトリ由来の prompt injection でも破壊的
  コマンドは成功しない。
- **プラグイン**: subagent の保証は **ツール名レベル** (Edit/Write/NotebookEdit/WebFetch/
  WebSearch を剥奪 + 読み取り専用 git のプロンプト規律)。Claude Code の subagent 定義は
  Bash の *引数* allowlist (`Bash(git diff *)`) を持てないため、standalone より prompt-
  injection 耐性は弱い。強制の強さを最優先したい対象では standalone `specguard run` を選ぶ。

検出した未修正ドリフトは `needs_user=yes` のとき sentinel として残り、SessionStart hook が
それを検知して人間に促す (Human-on-the-loop)。さらに baseline (`.last-ref`) は **ack 連動で
前進**するため、未修正のドリフトが次回の diff から外れて検出漏れになることがない。

## どう使うか

実行方式は 2 つあり、**同じ `specguard` バイナリ**を共有する。

| | standalone バイナリ | Claude Code プラグイン |
|---|---|---|
| 監査エンジン | `claude --print` を shard ごとに spawn | session 内 read-only subagent (nested claude なし) |
| 課金 | claude CLI の login 依存 | **ホストセッションの subscription** |
| read-only 強制 | `claude --print` の **Bash 引数 allowlist** (強い) | subagent の **ツール名** 制限 (やや弱い) |
| 起動 | `specguard run` (cron 等) | `/specguard:run` (対話/HOTL) |

### 前提

- Rust toolchain (`cargo`)。無ければ https://rustup.rs から。
- 監査対象は **git リポジトリ**であること。
- いずれの方式でも対象リポジトリに `specguard.toml` が必要 (下記 scaffold)。

### 1. バイナリを入れる (両方式の共通前提)

```sh
./install.sh                                  # release ビルドして ~/.local/bin へ
SPECGUARD_BIN_DIR=/usr/local/bin ./install.sh # 配置先を変えるなら
```

手動なら `cargo build --release` で `target/release/specguard` が生成される。
`~/.local/bin` が PATH 上にあることを確認すること。詳細・トラブルシュートは
**[INSTALL.ja.md](INSTALL.ja.md)**。

### 2. 対象リポジトリに scaffold

```sh
cd /path/to/your/repo
specguard init        # specguard.toml と SessionStart hook を生成 (冪等)
```

`init` は既存の `specguard.toml` を `--force` 無しでは上書きせず、`.claude/settings.json`
の他設定を壊さず SessionStart hook (未処理ドリフトの提示) だけを足す。
**プラグイン方式では hook は同梱済み**なので、config だけ用意すれば足りる
(`cp specguard.example.toml specguard.toml` でも可)。

### 3a. standalone で使う

```sh
cd /path/to/your/repo
# specguard.toml の [[area]] / [[invariant]] / canon を対象に合わせて編集
specguard run                                 # 監査を実行
```

cron / タスクスケジューラから `specguard run` を回し、`needs_user=yes` のとき立つ
sentinel を SessionStart hook が検知して人間に促す Human-on-the-loop に組み込める。

### 3b. Claude Code プラグインとして使う (subscription-native)

このリポジトリ自体がプラグインである。`claude --print` を起動せず、各 shard を
session 内の read-only subagent (`specguard-auditor`) に委譲し、ホストセッションの
subscription で監査する (API キー不要)。決定的ハーネスは同じ `specguard` バイナリに
委譲する (判定ロジックの二重化なし)。

```sh
cd /path/to/your/repo
claude --plugin-dir /path/to/specguard        # このセッションだけ読み込む
# 変更後は /reload-plugins、確認は /plugin
```

または marketplace 経由で install (セッションを跨いで永続):

```text
/plugin marketplace add yukineko/specguard    # marketplace を登録 (GitHub)
/plugin install specguard@specguard           # プラグインを install
```

**standalone モード**では `specguard` バイナリが PATH 上に必要 (`./install.sh`)。
**プラグインモード**ではコンパイル済みバイナリが `bin/` に同梱されており、hook は
`${CLAUDE_PLUGIN_ROOT}/bin/specguard` を直接呼ぶため PATH 設定は不要。
**macOS / Linux / WSL2** に対応 (バイナリは Rust、hook/コマンドは bash)。

```
/specguard:run
  └─ specguard prompt --json    (ハーネス: scope 解決 + shard 描画)
  └─ Task(specguard-auditor) × shard   (判定: read-only subagent / subscription)
  └─ specguard ingest --from …  (ハーネス: parse → verify → report → sentinel/baseline)
```

### スラッシュコマンド (プラグイン)

| コマンド | 対応バイナリ | 説明 |
|---|---|---|
| `/specguard:run [--baseline <ref>]` | `prompt --json` + subagent + `ingest` | subscription-native 監査 |
| `/specguard:brief <task>` | `brief --prompt` + subagent | 着手前の read-only 仕様ブリーフィング (ドリフトを未然に防ぐ) |
| `/specguard:scope` | `scope` | 解決済みスコープを表示 (agent 呼ばない) |
| `/specguard:ack` | `ack` | 対応済み sentinel をクリア |
| `/specguard:accept-prompt <理由>` | `accept-prompt` | prompt(メタ正典)を批准して pin |
| `/specguard:decide <タイトル>` | `decide` | 決定ログ(ADR)を canon commit に pin して生成 |
| `/specguard:drift-map [--baseline <ref>]` | `map build`/`map sync` + subagent | **書き込み側**。spec↔実装マッピングを保守し、仕様が無い entry には生成、drift は是正 (確信度が低ければ HOTL で確認) |

### サブコマンド (バイナリ)

```sh
specguard run                      # 監査を実行 (claude --print を shard ごとに spawn)
specguard scope                    # 解決されたスコープだけ表示 (agent 呼ばない)
specguard prompt                   # 各 shard のプロンプトを表示 (agent 呼ばない)
specguard prompt --json            # shard を機械可読 JSON で出力 (プラグインが使う)
specguard ingest [--from <file>]   # 収集済み shard 出力(JSON/stdin)を食わせて
                                   #   parse→report→sentinel (agent を spawn しない)
specguard brief "<task>"           # 着手前の read-only 仕様ブリーフィング (agent 実行)
specguard brief "<task>" --prompt  # ブリーフィングプロンプトのみ描画 (プラグインが使う)
specguard pending                  # sentinel があれば fix-offer を出力 (SessionStart hook)
specguard ack                      # 対応済みの sentinel をクリア (sentinel 後に fix commit が必要)
specguard ack --force              # fix-commit チェックを飛ばして無条件にクリア
specguard testaudit                # 実装済みだが実行されていないテストを検出 (findings あれば exit 7)
specguard testaudit --json         # 同上を機械可読 JSON で出力
specguard decide "<title>"         # 決定ログ(ADR)を生成
specguard accept-prompt -m "理由"  # prompt(メタ正典)を批准
specguard map build                # spec-map ストアを作成(無ければ) + 全履歴窓で seed
specguard map sync                 # baseline 以降の git 差分だけ増分反映 (A/M/R/D)
specguard map list [--json]        # 現在の spec↔実装マッピングを表示
specguard --baseline HEAD~5 run    # baseline を上書き
specguard --config examples/aegis.toml run
```

`ack` の fix-commit ゲート: `specguard ack` は sentinel が立った時点の git HEAD を記録し、
新しい commit が少なくとも 1 つ積まれるまでクリアを拒否する (ドリフトを ack する前に修正が
実際に commit されていることを保証)。rebase / cherry-pick などで sentinel 発生前に修正が
入っていた場合は `specguard ack --force` でこのチェックを飛ばす。

`testaudit` は全 `.rs` ファイルを走査し、(a) `#[ignore]` 付きのテスト、(b) コンパイルされない
`#[cfg(…)]` ブロック内のテスト、(c) どの親からも `mod` 宣言されていない `#[test]` を含む
`.rs` ファイル、(d) workspace に取り込まれていない `tests/` 配下の統合テストファイルを報告する
(= 実装したのに `cargo test` で実行されないテスト)。clean なら exit 0、findings ありなら exit 7。

### spec-map ストア + `/specguard:drift-map` (書き込み側の補完)

`run` / `brief` が **read-only の監査**なのに対し、`map` サブコマンドと
`/specguard:drift-map` コマンドは **書き込み側**でマッピングと仕様を育てる:

- **`specguard map`** は、spec/feature ↔ 実装ファイル ↔ テスト ↔ API/URL を関連付ける
  **独立した再利用可能なマッピングストア** (`.specguard/spec-map.toml`) を保守する。
  `git log --name-status` の差分を決定的に反映する (Added→新規 entry、Modified→`changed`、
  Renamed→移動、Deleted→detach/`missing`) だけの skeleton レイヤで、drift ワークフローに
  依存しない。**将来の `spec-audit` 等の別機能からも共有される独立レイヤ**として設計されている
  (このコマンドはマップ実装を再実装せず、常に `specguard map` に委譲する)。各 entry は
  `kind` (Feature|Endpoint) / `spec_doc` / `impl_files` / `test_files` / `client_refs` /
  `api` ({method, route}) を持つ。
- **`/specguard:drift-map`** はそのストアを **消費する** LLM オーケストレーションで、(1) `map sync`
  でマッピングを保守し、(2) 各 entry の `docs/specs/` 仕様書を参照、(3) 仕様が無い entry は実装と
  テストを読んで仕様本文 (概要・不変条件・振る舞い) を生成 (`REVIEW-NEEDED` マーク付き。必要なら
  同 crate の生成系 `specforge` を活用)、(4) `changed` entry で仕様↔実装が **食い違えば誤っている側
  (仕様書 or 実装) を是正**する。**どちらを正とするか確信が持てない場合は黙って決め打ちせず
  `AskUserQuestion` で人間に確認する (Human-on-the-loop)。** 意味的な帰属付け (どのファイルがどの
  feature に属するか) は、テストコード・API/route 実装・クライアントの HTTP 呼び出し・仕様書記述の
  うち **entry ごとに最も安価な情報源**から読み、安価に解決できないものは Ask するか未解決のまま
  残す (探索コストを有界に保つ)。

### 出力

| パス | 内容 |
|---|---|
| `<report_dir>/<date>.md` | レポート本体 |
| `<report_dir>/.last-ref` | 最後に監査した HEAD (次回の change-triggered baseline) |
| `<sentinel>` | `needs_user=yes` のときだけ (date / report / summary) |

baseline は **ack 連動で前進**する。クリーンに監査できた回だけ `.last-ref` を HEAD へ
進め、指摘が残っている間は据え置く (未修正 drift が次回の diff から外れて検出漏れに
なるのを防ぐ)。

### 設定 (TOML)

`specguard.example.toml` に全項目のコメント付きサンプルがある。要点:

- `[project]` … `name`, `root` (リポジトリルート)
- `[agent]` … `command` + `args`。既定は `claude --print` (read-only allowlist 付き)。
  任意のエージェント CLI に差し替え可 (プロンプトを stdin、レポートを stdout)
- `[scope]` … `baseline_ref` / `fallback_ref` (両方解決不能なら全 tracked file を監査)
- `[output]` … `report_dir` / `sentinel`
- `[prompt]` … `template` (省略時は埋め込み既定) / `require_ratification` (批准ゲート)
- `[[area]]` (複数) … `name` / `globs` / `canon`。**globs にマッチする変更があれば in-scope**
- `[[invariant]]` (複数) … `name` / `description` / `canon`。**毎回チェック**
- `[verify]` … 検証ゲート (既定 OFF)。`enabled` = 反証 (偽陽性除去) / `completeness` =
  網羅性批評 (偽陰性発掘)。**両方併用を推奨**。詳細は [DESIGN-VERIFY.md](DESIGN-VERIFY.md)
- `[decisions]` … 決定ログ (ADR) の鮮度・陳腐化照合を有効化

AEGIS の元実装を再現する設定例は `examples/aegis.toml`。

### 終了コード

| code | 意味 |
|---|---|
| 0 | 正常終了 |
| 2 | 設定 / 使用法エラー |
| 3 | いずれかの shard の出力に marker が無い (レポートは保存。baseline 前進・sentinel はしない) |
| 4 | いずれかの shard のエージェントが非ゼロ終了 (真の終了コードは stderr) |
| 5 | prompt(メタ正典)が未批准/変更あり (`require_ratification` 有効時)。`accept-prompt` が必要 |
| 6 | `ack` が sentinel 発生後の修正コミットを見つけられない。`--force` で上書き可 |
| 7 | `testaudit` が実装済みだが実行されていないテストを検出 |

一次情報は `src/main.rs` の `EXIT_*` 定数。エージェント由来の終了コードはそのまま伝播せず、
常に `4` に集約し各 shard の実コードを stderr に出す。

### 注意

検証ゲート (`[verify]`) ON 時、`ingest` 内の反証/網羅性批評は従来どおりバイナリが agent を
spawn する (= プラグイン経由でも nested claude が走る)。完全 native 化は今後の課題。

### テスト

```sh
cargo test          # unit (parse/scope/prompt/report) + integration (fake agent)
```

統合テストは `bash -c` の擬似エージェントを使うので実 LLM は不要。

## ライセンス

MIT
