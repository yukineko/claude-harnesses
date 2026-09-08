# harness 補助プラグイン 実装仕様書

> **この文書は Claude Code に実装させるためのブリーフである。**
> 実装前に §1 と §9 を必ず読むこと。§9 は「実装してはいけないもの」で、これを無視すると
> 検討済みで却下された設計を再発明することになる。
> 全体を通して、決定事項には根拠を併記してある。根拠に反する変更を加える場合は先に確認を取ること。

対象環境：Claude Code / Rust monorepo プラグイン群（`ctxrot` / `playbook` / `toolguard` / `budgetguard` / `compass` / `beacon` / `condukt` / `deepwiki`）。
セッション記録は repository と Obsidian の両方に存在する前提。

---

## 1. 解決する問題と、確定した判断

### 1.1 現状の失敗（実測ではなく体感。M3 で実測に置き換える）

| ID | 内容 | 頻度 |
|---|---|---|
| F1 | 記載済みの制約を参照しないまま作業する | 一日数件 |
| F2 | 修正範囲が不足する | 一日数件 |
| F3 | 完了宣言したが未実施 | 一日数件 |
| F4 | 存在しない API・古いシグネチャを使う（誤認） | 過剰なほど |

### 1.2 最優先要求

**最低限、同じ問題を繰り返さない。** 汎用的な改善より再発防止が先。

### 1.3 確定した判断（根拠つき）

1. **再発防止には retrieval 問題が存在しない。**
   前回の失敗事例から発火条件（パス・シンボル）が確定しているため、検索も意味的照合も不要。
   → M2 を最優先で実装する。

2. **誤認（F4）は予防できないが、産物は diff に現れる。**
   誤認は確信を伴うため、モデルは自分が知っているつもりのことを調べない。事前注入では届かない。
   → 事後の diff 照合で捕まえる。

3. **「軽視（文脈にあったが従わなかった）」と「無視（到達していない）」は機械的に分離できる。**
   トランスクリプトに Read/Grep の記録があるため、失敗ターンより前に該当ファイルの読み込みがあったかを見れば判定できる。LLM 不要。
   → M3 の中核。**この数字が出るまで、注入機構の設計方針は決められない。**

4. **文書の蒸留（圧縮）はしない。**
   圧縮すると条件節と例外が最初に落ち、残るのは発火しない一般論になる。必要なのは圧縮ではなく
   `条件 → 禁止/必須` への変換と、常時ロードから条件付き注入への移行。

5. **全主張を一括で文脈に入れる方式は採らない。**
   文脈に載ることと注意が向くことは別。300件中1件が関連する状況は 299 個の distractor を置くのと同じ。
   → 照合は必ずシャード分割（20〜30件/シャード）して並列実行する。

6. **文書由来の指摘でコード修正を強制しない。**
   文書が誤っている（F2 系の原因）ため、「文書が正」に倒れると誤った文書に合わせてコードを直させる。
   → 陳腐化の疑いがある主張は警告の強度を下げる。§7.4。

---

## 2. スコープ

### やる

- 再発防止レジストリと、その発火・注入・検査（M2）
- 失敗の逆引き計測基盤（M1 / M3）
- 計測結果に基づくルールの条件付き化（M4、M3 の結果次第）

### やらない

- コード検索の高速化（別件。`rg` 化とシンボル索引で対応する）
- 型・call graph による防御（既存で対応済み）
- 全文検索エンジン／RAG（§9）

---

## 3. 成果物構成

新規プラグイン 1 本。既存 monorepo に追加する。

```
crates/harness-recur/
├── Cargo.toml
├── hooks/
│   └── hooks.json            # プラグインフック定義
├── src/
│   ├── main.rs               # CLI エントリ（サブコマンド分岐）
│   ├── db.rs                 # SQLite スキーマ・マイグレーション
│   ├── link.rs               # M1: session ↔ commit 紐付け
│   ├── registry.rs           # M2: 再発防止レジストリ
│   ├── resolve.rs            # M2: 発火解決
│   ├── inject.rs             # M2: hook 出力（additionalContext / exit 2）
│   ├── probe/
│   │   ├── mod.rs
│   │   ├── extract.rs        # M3: トランスクリプトからの特徴抽出
│   │   ├── prior.rs          # M3: 先行事例の収集（git log）
│   │   └── report.rs         # M3: レポート生成
│   └── judge.rs              # M2 拡張: シャード並列の diff 照合（M2 では未使用）
└── tests/
```

状態は `${CLAUDE_PLUGIN_DATA}/harness-recur/`。
プラグイン更新で消えてはいけないため `CLAUDE_PLUGIN_ROOT` ではなく `CLAUDE_PLUGIN_DATA` を使う
（公式ドキュメントで確認済み・2026-09-08、§12-5。`CLAUDE_PLUGIN_DATA` は Claude Code が
プラグイン hook に自動設定し、`CLAUDE_PLUGIN_ROOT` と違ってプラグイン更新を生き延びる）。

> **【要判断・未決】ストア形式を SQLite にするか。**
> このワークスペースに `rusqlite` / `sqlite` の依存は **0 件**（実測 §12-1）。既存 39 crate の
> 永続化はすべて追記 JSONL か JSON / TOML ファイルで、追記の原子性は
> `harness_core::append::append_line` に集約されている（issue #15 の修正で 6 sink を統一済み）。
> SQLite を入れると (a) ワークスペース初の C 依存、(b) 既存の append 不変条件の外側に
> もう一系統の並行性モデル、が同時に増える。一方 §5.8 の「1,000 件で 20ms」と §5.3 の
> 索引照合は SQLite の方が素直に書ける。**着手前に決めること。** 決めずに書き始めると後で移せない。
> 以降に出てくる `CREATE TABLE` は**データモデルの記述**であって、SQLite の採用を確定した
> ものではない。

### 3.1 再実装してはいけない既存部品（実測 §12-2）

このリポジトリは "independent reinvention"（同じ形の部品の再発明）を繰り返し検出してきた
（統合の前例は `harness_core::callgraph` 冒頭）。harness-recur が必要とする部品のうち、
**以下は既に存在する**。新規に書かず、これらを呼ぶこと。

| 用途 | 既存部品 | 備考 |
|---|---|---|
| glob 照合 | `globset`（8 crate が採用）または `ctxrot::glob::matches` | **後者は壊れたパターンに `false` を返す** ＝ そのトリガは永久に発火しない fail-open。トリガに転用するなら判定不能を `Undetermined` にすること |
| diff からの識別子抽出 | `harness_core::callgraph::changed_symbol_names(diff_text)` | §5.3-2 の入力そのもの |
| call graph 深さ1 | `harness_core::callgraph::{load_graph, callers_of, callees_of}` | `load_graph` は `Determination` を返す（判定不能を表現済み） |
| シンボル索引 | `harness_core::code_index::{extract_symbols, load_index}` | §8「索引外シンボル」の判定に使う |
| 注入予算 | `harness_core::inject::CharBudget` ＋ `harness_core::inject_metrics::record` | §5.7 参照。**char 単位**であり token 単位ではない |
| 注入済みフラグ | `context_governor::ledger::was_injected` | §5.5 の「ctxrot / context-governor が生存判定を持っている場合はそちらと接続」の接続先 |
| 再発シグネチャ | `overwatch::violation::{normalize_signature, detect_recurrence}` | §5.6 の昇格条件（3 回以上の発火・誤発火ゼロ）の計数に転用できる |
| transcript の streaming 読み | `harness_core::transcript` | usage トークンと直近 N ターンだけ。**M3 が要る「path 付き・順序付き・turn_index 付き」のイテレータは存在しない**（§6.3）ので、ここは harness-core への追加になる |
| 劣化の単調性の検査 | `harness_core::degrade::{is_monotone, explain_break}` | 「入力を劣化させても判定が permissive 側へ動かない」という性質を proptest で探索する部品（**stderr 通知のヘルパではない**）。§8 の「DB 破損時」の挙動はこの性質のテストで固定すること。stderr への通知そのものは共有ヘルパが無く、各プラグインが直接書いている |

---

## 4. M1：session ↔ commit 紐付け（最優先・即日）

**これが無いと M3 の逆引きが時刻近傍推定に劣化する。今日入れないと今日以降のデータも失われ続ける。**

> **この主張は実測で裏づけられた。ただし想定より深刻である（§12-3）。**
> 2026-09-08 時点で `difflog` は **137 セッション**分の記録を持つ（最古 2026-07-01）が、
> `~/.claude/projects/**/*.jsonl` に現存する transcript は全 7 プロジェクト合計 **51 本**、
> しかも**すべて 2026-09-07〜09-08 の 2 日分**である。過去セッションの transcript は既に無い。
>
> **帰結: 「§4.1 で `transcript_path` を保存し、§6.3 で後日そこから特徴を抽出する」という
> 設計は成立しない。** 保存したパスは数日で dangling pointer になる。M1 は「パスを記録する」
> ではなく「**セッションが終わる時点で §6.3 の特徴を抽出して永続化する**」でなければならない。
> §4.1・§6.3・§6.7 はこの前提で書き直してある。

### 4.1 記録するもの

`SessionStart` と `SessionEnd`、および各コミット時点で以下をサイドカーに追記する。

```sql
CREATE TABLE session_link (
  session_id      TEXT NOT NULL,
  commit_sha      TEXT,           -- コミット時のみ
  event           TEXT NOT NULL,  -- 'start' | 'end' | 'commit'
  ts              TEXT NOT NULL,  -- ISO8601
  cwd             TEXT NOT NULL,
  transcript_path TEXT,
  branch          TEXT,
  PRIMARY KEY (session_id, event, ts)
);
CREATE INDEX idx_link_commit ON session_link(commit_sha);
CREATE INDEX idx_link_ts ON session_link(ts);
```

`transcript_path` は**残す。ただしそれだけでは足りない**（上のとおり実体が数日で消える）。
`SessionEnd` の時点で §6.3 の特徴を抽出し、同時に永続化する。

```sql
CREATE TABLE session_feature (
  session_id   TEXT NOT NULL,
  turn_index   INTEGER NOT NULL,
  ts           TEXT NOT NULL,
  tool_name    TEXT,            -- 'Read' | 'Grep' | 'Edit' | ...
  target       TEXT,            -- file_path（Grep は pattern / path）
  token_pos    INTEGER,         -- セッション先頭からの累積トークン位置
  via_subagent INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, turn_index, tool_name, target)
);
CREATE INDEX idx_feature_target ON session_feature(target);
```

これで `read_before`（§6.3）は transcript ではなく `session_feature` への問い合わせになり、
**transcript が消えた後も答えが出る**。`SessionEnd` は 1.5 秒予算（§4.2）なので、抽出は
streaming の 1 パスで済ませ、重い集計は `probe report` 側に置く。

**既存ストアへの最小の追加**: `difflog` は既に
`SessionState { session_id, start_sha, project, started_at }` を
`~/.difflog/logs/sessions/<session_id>.json` に保存している（`crates/difflog/src/state.rs`）。
ここに `transcript_path` を **1 フィールド**足すだけで、harness-recur が無い期間のセッションも
後追いで紐付けられる（ただし前段のとおり、実体が残っている 2 日分に限られる）。

### 4.2 hook 配線

```json
{
  "description": "harness-recur: session/commit linkage",
  "hooks": {
    "SessionStart": [
      { "hooks": [ {
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/bin/harness-recur",
          "args": ["link", "--event", "start"],
          "async": true
      } ] }
    ],
    "SessionEnd": [
      { "hooks": [ {
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/bin/harness-recur",
          "args": ["link", "--event", "end"],
          "timeout": 1
      } ] }
    ],
    "PostToolUse": [
      { "matcher": "Bash",
        "hooks": [ {
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/bin/harness-recur",
          "args": ["link", "--event", "commit"],
          "if": "Bash(git commit *)",
          "async": true
      } ] }
    ]
  }
}
```

注意点。

- `SessionEnd` フックは全体で 1.5 秒の予算しかない。`timeout` を短く設定し、処理は最小に
- `if` はツールイベントでのみ評価される。`Bash(git commit *)` はサブコマンド単位で照合される
- コミット SHA は hook 内で `git rev-parse HEAD` を実行して取得する（`tool_output` の解析に依存しない）
- 可能なら commit trailer にも `Claude-Session-Id: <session_id>` を入れる（git 単体で追える冗長化）
- **この hook JSON の形（`args` / `if` / `async`、および SessionEnd の 1.5 秒予算）は
  2026-09-08 に公式ドキュメントで確認済み**（§12-5）。ただし**この repo の 30 個の
  `hooks/hooks.json` はどれも `args` / `if` / `async` を使っていない**（すべて単一の `command`
  文字列 ＋ `timeout`）。harness-recur が最初の利用者になるので、`/hooks` での登録確認（§11-3）を
  省略しないこと。特に `if` が効かなければ commit フックは**すべての Bash 呼び出しで発火する**

### 4.3 受け入れ基準

- 1セッション内で複数コミットしても、全て記録される
- `harness-recur link --lookup <sha>` で session_id と transcript_path が返る
- hook が失敗してもセッションは継続する（全て非ブロッキング）

---

## 5. M2：再発防止レジストリ（最優先）

### 5.1 データモデル

```sql
CREATE TABLE claim (
  id             TEXT PRIMARY KEY,      -- slug
  statement      TEXT NOT NULL,         -- 1〜3行。条件 → 禁止/必須 の形
  fix_hint       TEXT,                  -- 正しい書き方（任意、1〜2行）
  severity       TEXT NOT NULL,         -- 'info' | 'warn' | 'block'
  origin_sha     TEXT,                  -- 元になった失敗コミット
  origin_session TEXT,
  verified_sha   TEXT,                  -- この主張を確認した時点の SHA
  created_at     TEXT NOT NULL,
  disabled       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE claim_trigger (
  claim_id  TEXT NOT NULL REFERENCES claim(id),
  kind      TEXT NOT NULL,   -- 'path' | 'symbol' | 'command' | 'diff_regex'
  pattern   TEXT NOT NULL
);
CREATE INDEX idx_trigger_kind ON claim_trigger(kind, pattern);

CREATE TABLE fire_log (
  ts          TEXT NOT NULL,
  session_id  TEXT NOT NULL,
  claim_id    TEXT NOT NULL,
  event       TEXT NOT NULL,   -- 'inject' | 'block' | 'suppressed_budget'
  tool_name   TEXT,
  target      TEXT
);
```

**設計上の制約**

- `statement` は1〜3行。これを超えるものは主張として成立していないので分割する
- `severity` の初期値は必ず `warn`。`block` への昇格は §5.6 の条件を満たしたときのみ
- `diff_regex` トリガは、書けるものだけ書く。書けないものは `path` / `symbol` のみで運用する
- **【要判断】語の衝突**: このリポジトリで `claim` は既に「**backlog の並行セッション排他**」を
  指す（`backlog claim ledger` / `condukt state is-claimed` / 同名の pending backlog 複数件）。
  同じ語を「再発防止の主張」に再利用すると、ログとコードの両方で読み手が取り違える。
  `precept` / `rule` / `recur` などへの改名を推奨する。**仕様の語彙は著者の決定**なので
  ここでは変更していない

### 5.2 登録フロー

失敗が起きたその場で1件登録する。これが唯一の供給経路。

```
harness-recur add \
  --statement "retry 経路から呼ばれる verify_token では二重検証しない" \
  --path "crates/auth/**" \
  --symbol "verify_token" \
  --origin-sha HEAD
```

- `--origin-sha` を渡した場合、そのコミットが触ったパス・シンボルをトリガの初期値として自動補完する
- 対話モードも用意する（`harness-recur add -i`）。**登録が面倒だと運用が止まるので、ここの摩擦を最小化することを最優先で設計すること**

### 5.3 発火解決

```
入力: tool_name, tool_input（file_path / command / diff）
1. path トリガ    : file_path を glob 照合
2. symbol トリガ  : diff から識別子抽出 → 既存 call graph で深さ1の閉包 → 照合
3. command トリガ : command 文字列を照合
4. diff_regex     : diff 本文に正規表現照合
5. disabled を除外
6. 同一セッションで既に注入済みの claim を除外（§5.5 の例外あり）
7. 予算内に収める
```

**完全一致とグロブのみ。あいまい照合・意味的類似は使わない。**
発火件数の想定は1ターンあたり 2〜5 件。1,000件登録しても変わらない。

### 5.4 hook 配線

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Edit|Write",
        "hooks": [ {
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/bin/harness-recur",
          "args": ["resolve", "--phase", "pre"],
          "timeout": 5
      } ] }
    ],
    "PostToolUse": [
      { "matcher": "Edit|Write",
        "hooks": [ {
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/bin/harness-recur",
          "args": ["resolve", "--phase", "post"],
          "timeout": 10
      } ] }
    ],
    "Stop": [
      { "hooks": [ {
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/bin/harness-recur",
          "args": ["verify", "--phase", "stop"],
          "timeout": 30
      } ] }
    ],
    "PostCompact": [
      { "hooks": [ {
          "type": "command",
          "command": "${CLAUDE_PLUGIN_ROOT}/bin/harness-recur",
          "args": ["reinject"],
          "timeout": 5
      } ] }
    ]
  }
}
```

**各フックの役割と出力形式**

| フック | 役割 | 出力 |
|---|---|---|
| `PreToolUse` | `block` の主張に違反する編集を差し止め | `hookSpecificOutput.permissionDecision: "deny"` + `permissionDecisionReason` |
| `PostToolUse` | 発火した主張を注入 | `hookSpecificOutput.additionalContext`（ツール結果の隣に入る） |
| `Stop` | 未充足の主張が残っていれば停止させない | 継続させる場合は `decision: "block"` + `reason`。指摘のみなら `hookSpecificOutput.additionalContext` |
| `PostCompact` | compaction で消えた注入を再投入 | stderr はユーザにしか出ないため、実際の再注入は次の `PostToolUse` で行う（§5.5） |

**exit code の扱い（重要）**

- ポリシー強制には必ず `exit 2` を使う。`exit 1` は非ブロッキングエラー扱いで処理が続行される
- `PostToolUse` の `exit 2` は編集を取り消さない。stderr が Claude に見えるだけ
- `PreToolUse` の command hook はタイムアウトしても**ブロックしない**。ゲートとして当てにしない設計にする
- `additionalContext` は 10,000 文字上限。超えると別ファイルに退避されプレビューだけになる

### 5.5 compaction 対応

`PostCompact` はユーザにしか stderr を出せないため、そこで直接注入はできない。実装は以下。

1. `PostCompact` で「注入済みフラグ」をリセットする（DB の session state）
2. 次の `PostToolUse` で、発火中の主張が再度注入される

`ctxrot` / context-governor が生存判定を持っている場合はそちらと接続し、二重にリセットしない。

### 5.6 `block` への昇格

**主張単位で昇格させる。一括で上げない。**

昇格条件（全て満たすこと）：

- 登録から 2 週間以上経過
- `fire_log` に3回以上の発火があり、うち誤発火の報告がゼロ
- `harness-recur promote <claim_id>` を人が明示的に実行

降格：誤発火が1件でも報告されたら即座に `warn` へ戻す（`harness-recur demote <claim_id>`）。
**機構全体は止めない。** 粒度を主張単位に落とすことが、誤発火で仕組みごと無効化されるのを防ぐ唯一の方法。

### 5.7 注入予算

- 1ターンあたり上限 **1,500 トークン相当**。ただし実装は既存機構に合わせて **char 単位**で数える
  — `harness_core::inject::CharBudget` ＋ `harness_core::inject_metrics::record(plugin, session,
  prompt, chars)`。これを使うと playbook / runbook / ctxrot / fugu-router / context-governor が
  同じターンに注入した分と**合算**で予算判定できる（各プラグインが個別に上限を持つと、合計は
  誰にも見えない）
- **`budgetguard` の管理下には置けない**（実測 §12-4）。budgetguard が見ているのはセッション /
  日次の **USD コスト**であって注入トークンではない。注入予算の台帳は `inject_metrics` 側で、
  そちらは現状 **detect + warn のみで強制していない**（`inject_metrics.rs`:
  「the shipped enforcement is detection + warn only」）。harness-recur が強制する最初の
  消費者になるなら、その旨を明示すること
- 優先順位：`block` > `warn` > `info`、同順位内は登録が新しい順
- 予算で落とした件数は必ず明示する（「他 N 件を予算により省略」）。黙って落とさない
- 予算超過は `fire_log` に `suppressed_budget` として記録する

### 5.8 受け入れ基準

- 主張を1件登録し、該当パスを編集すると `PostToolUse` で注入される
- 該当しないパスの編集では発火しない（誤発火ゼロ）
- 1,000件登録した状態で解決処理が **20ms 以内**
- `block` の主張に違反する `Edit` が `PreToolUse` で拒否される
- `harness-recur stats` で発火回数・誤発火率・予算超過率が出る

---

## 6. M3：失敗の逆引き計測

**目的は1つ。「軽視」と「無視」の比率を出すこと。** これが出るまで M4 の方針は決められない。

### 6.1 失敗点の同定

以下を失敗候補として抽出する（決定的、LLM 不要）。

- revert コミット
- コミットメッセージが修正を示すパターン（`fix`, `修正`, `revert`, `hotfix` 等。プロジェクトの実際の慣習に合わせる）
- 同一ファイルへの N 時間以内の再編集
- テスト red → green のサイクル
- Obsidian daily note 内の問題記述

### 6.2 先行事例の収集

```
失敗コミットが触ったファイル集合 F
  → git log --follow -- F で過去コミット
  → session_link 経由で過去セッションを特定
  → 先行事例群（成功群）
```

**意味的類似は使わない。** ファイルとシンボルの一致のみ。

### 6.3 特徴抽出

各セッションのトランスクリプト（`transcript_path`、JSONL）から、失敗ターンについて以下を抽出する。

| 特徴 | 型 | 取り方 |
|---|---|---|
| `read_before` | bool | 失敗ターンより前に関連ファイルの Read/Grep があるか |
| `read_distance_tokens` | int | Read から編集までのトークン距離 |
| `turn_index` | int | セッション内のターン番号 |
| `token_position` | int | セッション内のトークン位置 |
| `compacted_before` | bool | 失敗ターン以前に compaction があったか。**transcript を解析するより `PreCompact` フックの既存記録を引く方が確実** — ctxrot が `<state_dir>/metrics.jsonl` に `rescue` イベントを書いている（行スキーマは `harness_core::metrics`） |
| `via_subagent` | bool | `agent_id` の有無で判定。**ただし `harness_core::hook::HookInput` に `agent_id` / `agent_type` フィールドが無い**ので、hook 側で使うなら先に HookInput を拡張する。transcript 側から取るなら `harness_core::usage` の `isSidechain` 判定 |
| `distractor_count` | int | 直前 N ターンで参照した他ファイル数 |
| `instructions_loaded` | list | ロードされていた CLAUDE.md / rules のハッシュ |

`agent_id` / `agent_type` は subagent 内で発火した hook の入力に含まれる。
`InstructionsLoaded` イベントを記録しておくと `instructions_loaded` が正確に取れる。
**どちらも公式に実在することを 2026-09-08 に確認済み**（§12-5）。ただしこの repo の 30 個の
`hooks.json` はどちらも使っておらず、`HookInput` にも該当フィールドが無いので、harness-recur が
最初の利用者になる。

### 6.4 出力

```
harness-recur probe report --since 2026-08-01
```

出すもの：

1. **軽視 / 無視の比率**（`read_before` の分布）— **最重要**
2. 失敗率のトークン位置相関（size が支配変数かどうかの判別）
3. 成功群 vs 失敗群での各特徴の差
4. 失敗クラス（F1〜F4）の件数分布

### 6.5 判断の分岐

| 結果 | 意味 | 次の手 |
|---|---|---|
| 無視が支配的 | 到達の問題 | 強制注入を強化。M4 は注入経路の拡張 |
| 軽視が支配的 | 希釈・競合の問題 | **注入を増やすと悪化する。** M4 は常時ロードの削減と条件付き化 |
| 失敗率がトークン位置に平坦 | size ではない | 文書の形式（条件付き命令化）が対象 |
| 特定位置から急増 | size が支配変数 | その位置が閾値。セッション分割かルール件数削減 |

### 6.6 限界（レポートに明記すること）

- **交絡**：n が小さいと偽の分離が出る。複数の失敗クラスで再現する特徴だけを採用する
- **生存者バイアス**：検出されなかった失敗は記録に無い。分布は「気づけた失敗」に偏る
- **先行事例が無い作業**：初回作業は対比が取れない

### 6.7 受け入れ基準

- ~~直近4週間について §6.4 の4項目が出る~~ → **遡及は不能**（実測 §12-3）。M1 が landed する
  前のセッションについては transcript が既に失われており、`read_before` は原理的に出せない。
  受け入れ基準を**前向き**に置き換える:
  - M1 landed 後に蓄積した `session_feature` に対して §6.4 の 4 項目が出る
  - 母数が足りないときは、数字ではなく **`Undetermined` と件数**を出す（空集合や 0% を
    「差が無い」と読ませない — CLAUDE.md 3.）
  - 失敗クラスごとに最低 10 件が貯まるまで M4 に進まない
- 手で確認した10件と、自動判定した `read_before` が一致する

---

## 7. M4：ルールの条件付き化（M3 の結果を見てから着手）

### 7.1 常時ロードの棚卸し

- CLAUDE.md / `.claude/rules/*.md` のルールを件数で数える
- 目安：常時ロードは **20件以下**。1ターンあたりの注入は **3〜7件**、10件を超えると選択が崩れる
- ただし**自分のデータから出た閾値（M3 §6.5）を優先する**。上の数字は一般論

### 7.2 変換（蒸留ではない）

| 変換 | 内容 |
|---|---|
| 散文 → 条件付き命令 | `<条件> のとき <禁止/必須>`。この形に落ちないものは主張ではない |
| 抽象 → 具体 | 実際の識別子・パスを含める。「適切に扱う」は削除 |
| 常時 → 条件付き | claim テーブルへ移し、トリガで発火させる |

**圧縮しないこと。** 条件節と例外を落とすと発火しない一般論になる。

### 7.3 階層化

```
claim.statement : 1〜3行の命令（これだけ注入）
claim.source    : 元文書へのパス（必要時に読ませる）
```

### 7.4 陳腐化の扱い

`claim.verified_sha` より後に、その主張のトリガパスが変更されていれば「陳腐化の疑い」として提示する。

- **ブロックしない。警告の強度を下げる**
- git だけで判定でき、自動更新できる
- これが §1.3-6（文書由来の指摘でコード修正を強制しない）の実装

---

## 8. 非機能要件

| 項目 | 要件 |
|---|---|
| `PreToolUse` レイテンシ | 20ms 以内（1,000 claim 時） |
| `Stop` レイテンシ | 3秒以内 |
| DB 破損時 | 注入経路は fail-open（注入せず継続）。ただし**沈黙は不可** — `harness_core::degrade` で劣化を明示する（CLAUDE.md 1.: 沈黙は「余裕あり」と読まれる fail-open）。`block` を持つ主張が 1 件でもある状態で DB が読めないなら、その主張は判定不能なので次行に従い fail-closed |
| `block` 判定不能時 | fail-closed（差し止め） |
| 索引外シンボル | 「存在しない」ではなく**「索引外」**として扱う。マクロ由来の偽陰性が誤ったブロックに化けるのを防ぐ |
| hook 失敗 | セッションを止めない。ポリシー強制以外は全て非ブロッキング |

---

## 9. 実装してはいけないもの

以下は検討の上で却下済み。実装しないこと。理由を添えてあるので、覆す場合は理由に反論してから。

| 却下したもの | 理由 |
|---|---|
| **全文検索エンジン / Meilisearch** | 再発防止に retrieval 問題は存在しない。必要になるのは claim が 1,000 件を超え、かつトリガで絞れない主張が有意に残った場合のみ。現時点で該当しない |
| **RAG / 埋め込み検索** | 同上。加えて、誤認は確信を伴うため事前注入経路では届かない |
| **文書へのアンカー一括付与（全文書移行）** | 効果が出るまで6週間かかる。失敗駆動の登録なら初日から効く。将来 claim が 1,000 件を超えたときのコスト最適化として再検討 |
| **全 claim を一括で文脈に入れる照合** | distractor による希釈。必ずシャード分割する |
| **文書の蒸留・圧縮** | 条件節と例外が最初に落ち、発火しない一般論が残る |
| **`severity: block` の一括適用** | 誤発火1件で機構全体が無効化される。主張単位で昇格させる |
| **文書由来の指摘でのコード修正強制** | 文書が誤っているため、誤った文書に合わせてコードを直させる |

---

## 10. マイルストーンと受け入れ基準

| | 内容 | 期間 | 完了条件 |
|---|---|---|---|
| M1 | session ↔ commit 紐付け | 即日 | §4.3 |
| M2 | 再発防止レジストリ | 3〜5日 | §5.8 |
| M3 | 失敗逆引き計測 | 3〜5日 | §6.7 |
| M4 | ルール条件付き化 | M3 次第 | M3 の分岐に従う |

M1 と M2 は M3 の結果を待たずに着手してよい。M4 は M3 の数字が出るまで着手しない。

---

## 11. Claude Code への作業指示

1. **M1 から始める。** 紐付けが無い間、失われ続けるデータがある
2. 各マイルストーンは受け入れ基準のテストを先に書く
3. hook を追加したら `/hooks` で登録を確認する。パスの打ち間違いは
   `Failed with non-blocking status code:` の通知として出るだけで、ゲートは黙って無効になる
4. `harness-recur add` の摩擦を最小化することを他の何より優先する。**登録されない主張は存在しないのと同じ**
5. §9 のいずれかを実装したくなったら、その理由を提示して確認を取ること

### 参照

- Claude Code hooks reference: https://code.claude.com/docs/en/hooks

---

## 12. 実測ログ — この仕様書を repo の実態と突き合わせた結果

**測定の作法**: このリポジトリの CLAUDE.md の要求により、数字には**測定コマンド・測定点（rev）・
測定日**を必ず併記する。測定点の無い数字は、次の著者が転記した瞬間に腐る。**継承せず、毎回
測り直すこと。**

測定点: `d51bd558` ／ 測定日: **2026-09-08** ／ 対象: `~/src/claude-harnesses`（39 crate）

### 12-1. ワークスペースに SQLite 依存は無い

```
grep -rn 'rusqlite\|sqlite' --include=Cargo.toml . | grep -v target | wc -l
```

→ **0**。既存 39 crate の永続化は追記 JSONL / JSON / TOML のみ。§3 の【要判断】の根拠。

### 12-2. 3 機構の重複調査（M1 / M2 / M3）

| 機構 | 既に存在するもの | 存在しないもの |
|---|---|---|
| **M1** | `difflog::SessionState { session_id, start_sha, project, started_at }`（`crates/difflog/src/state.rs`）。`overwatch::ActualChangeset { task_key, session_id, base_ref, head_sha, files }`（`crates/overwatch/src/changeset.rs`） | `transcript_path` を保存する永続ストア **0 件**。SHA→session の逆引き索引 **0 件**。commit ごとの追記 **0 件**。`ActualChangeset` は `task_key` キー・TTL prune・condukt run 限定なので恒久ストアに転用不可 |
| **M2** | 注入系 3 crate（playbook / runbook / context-governor）はすべて **UserPromptSubmit のプロンプト語彙マッチ**。playbook の `triggers` は path glob ではなくプロンプト語（`crates/playbook/src/retrieve.rs`）。PreToolUse で glob を見る 4 crate はすべて deny 専用の組み込み定数 | **「path / symbol トリガで PreToolUse・PostToolUse に条件付き注入」は 39 crate のどこにも無い。** `severity` の昇格 / 降格 **0 件**、`fire_log` 相当 **0 件**。`harness_core::lessons::Lesson` は 6 フィールド（`id / kind / task_summary / lesson_text / source_run / ts`）で trigger も severity も持たず、検索は lexical Jaccard（§5.3 の「完全一致とグロブのみ」と方式が逆）、かつ `~/.lessons/` は**プロジェクト非依存**という設計意図があり path glob と噛み合わない |
| **M3** | transcript パーサが **7 箇所に散在**。`harness_core::transcript`（usage トークン / 直近 N ターン）、`harness_core::usage`（model 別集計・`isSidechain`）、`trajectoryeval::extract_tools`、`blastguard::retro`、`autoflow`、`beacon`、`compass` | **`read_before` を出せる実装はゼロ。** `trajectoryeval` は順序を保つが `file_path` を捨て、`blastguard::retro` は path を持つが `BTreeMap` なので順序を失う。「path 付きで順序が残る」実装は存在しない。`turn_index` / `token_position` を返す関数も無い。`InstructionsLoaded` の文字列はリポジトリ全体で 0 件 |

**結論**: M1 / M2 / M3 の中核はどれも既存に無いため、**新規 crate `harness-recur` が妥当**。
ただし部品は §3.1 のとおり既存を呼ぶこと。併せて既存側に小さな追加を 2 つ:
(a) `difflog::SessionState` に `transcript_path`、(b) `harness_core::transcript` に
「path 付き・順序付き・turn_index 付きのツールイベント streaming イテレータ」。
(b) は `trajectoryeval` と `blastguard::retro` の重複も同時に解消する。

### 12-3. transcript は 2 日で消えている（この仕様書で最も重い実測）

```
ls ~/.difflog/logs/sessions | wc -l                                  # -> 137（最古 2026-07-01）
find ~/.claude/projects -name '*.jsonl' | wc -l                      # -> 51
find ~/.claude/projects -name '*.jsonl' -printf '%TY-%Tm-%Td\n' | sort | uniq -c
                                                                     # -> 43 件が 2026-09-07 / 8 件が 2026-09-08。それ以外は無い
grep -rl 'transcript_path' ~/.difflog ~/.gauge ~/.claude/sessions    # -> 0 件
```

→ difflog は 137 セッションを知っているのに、transcript は **51 本しか残っておらず全て直近 2 日分**。
`transcript_path` を保存している永続ストアも **0 件**。§4（設計変更）と §6.7（受け入れ基準の
前向き化）の根拠。

### 12-4. 注入予算の実体

- `harness_core::inject::CharBudget` — **char 単位**の running cap（`new` / `would_overflow` /
  `add` / `used` / `remaining`）。最初の 1 件は常に通す。
- `harness_core::inject_metrics::record(plugin, session, prompt, chars)` — 横断台帳。
  記録しているのは playbook / runbook / ctxrot / fugu-router / context-governor。
  `over_budget(turns, budget_chars)` / `remaining_for_turn` はあるが、コード内に
  「Provided for a future active-enforcement phase; the shipped enforcement is detection + warn only」
  と明記されている。
- `budgetguard` が扱うのは `session_cost`（**USD**）。**注入トークンの予算機構ではない。**

### 12-5. hook スキーマの確認（公式ドキュメント照会、2026-09-08）

| 仕様が使うもの | 実在 | 備考 |
|---|---|---|
| `"args": [...]`（exec form） | **する** | この repo の 30 個の hooks.json では 0 件使用 |
| `"if": "Bash(git commit *)"` | **する** | permission rule 構文。0 件使用 |
| `"async": true` | **する** | `asyncRewake` で exit 2 時に復帰。timeout 非強制。0 件使用 |
| SessionEnd の 1.5 秒予算 | **する** | 既定 1.5 秒。per-hook timeout を長くすれば最大 60 秒まで引き上げ（difflog は 30 秒を指定している） |
| `PostCompact` | **する** | matcher は `manual` / `auto`。この repo の hooks.json では 0 件使用（実際の再注入経路は `SessionStart` の `source == "compact"`。`crates/ctxrot/src/hooks/restore.rs`） |
| `CLAUDE_PLUGIN_DATA` | **する** | プラグイン更新を生き延びる永続データ置き場として公式推奨。この repo では 0 件使用 |
| `agent_id` / `agent_type` | **する** | subagent 内で発火した hook の入力にのみ含まれる。`harness_core::hook::HookInput` には未定義 |
| `InstructionsLoaded` | **する** | CLAUDE.md / `.claude/rules/*.md` のロード時に発火。matcher は `session_start` / `nested_traversal` / `path_glob_match` / `include` / `compact` |

→ **§4.2 と §5.4 の hook 配線は正しい。** この repo の既存 30 個が古い部分集合しか
使っていないだけである。

### 12-6. 未確認のまま残したもの（判断で埋めていない）

CLAUDE.md 2.「判断は予測にすぎない」に従い、確かめられなかったものは確かめられなかったと書く。

- **`additionalContext` の 10,000 文字上限**（§5.4）: 公式ドキュメントに数値の記載を見つけられ
  なかった。「超えると別ファイルに退避されプレビューだけになる」も未確認。当該 2 行は
  **未検証の主張**として残してある。実装前に実測すること。
- **`PreToolUse` の command hook がタイムアウト時にブロックしないか**（§5.4）: exit code の
  一般規則（exit 2 のみが単独で block する）は確認できたが、タイムアウト時の扱いは記載が無い。
  「ゲートとして当てにしない設計にする」という**結論は保守的な側なので維持**するが、
  その根拠は未確認である。
- **§1.1 の F1〜F4 の頻度**: 仕様自身が「実測ではなく体感」と書いているとおり未実測。
  M3 がこれを実測に置き換える。
