---
name: daily-report
description: その日の作業を「git のコミットログ」＋「Obsidian の session record ノート」から集約し、1枚の日報（何をやったか / なぜか / 学び / 残課題 / 数値）に合成して Obsidian vault へ書き戻すコマンド。git は当該リポジトリの変更を、Obsidian records は横断プロジェクトのサマリ・学び・残課題・コスト/トークン数を供給する。集約と合成（判断）は LLM、材料は git と session-insights が書いた既存 record が供給。生成のみで record は書き換えない。
argument-hint: "[YYYY-MM-DD | today | yesterday | --since <git-expr>] [--repo <path> ...] [--stdout]  省略時は今日"
allowed-tools: Bash(git:*), Bash(ls:*), Bash(cat:*), Bash(mkdir:*), Bash(date:*), Read, Grep, Glob, Write
---

# /daily-report — git log ＋ Obsidian から日報を合成

`/daily-report` は **その日の git コミット** と **Obsidian の session record** を材料に、
スキャンで読める **日報（Markdown）** を合成し、`<vault>/daily/<date>.md` に書き戻す。

```
材料（決定論的に収集）                合成（LLM）              出力
  git log（当該リポジトリの変更）  ─┐
  Obsidian records（横断サマリ・   ├─▶  日報ナラティブ  ─▶  <vault>/daily/<date>.md
    学び・残課題・コスト/トークン） ─┘                        （＋任意で stdout）
```

**役割分担**: 材料集めは git と（session-insights が既に書いた）record が供給。
このコマンドは **新しい状態を作らず**、材料を読んで **合成するだけ**。record ノートは決して書き換えない。

## いつ使うか

- その日一日の作業を振り返り、日報 / standup メモを1枚にまとめたいとき。
- 複数プロジェクトを跨いで作業した日の成果を、記録済みの record から横断集約したいとき。

## 手順

### Step 0 — 引数から対象日と設定を決める

- 引数に日付 `YYYY-MM-DD` があればそれを対象日 `<date>` にする。`today`/省略＝今日、`yesterday`＝前日。
  日付の解決は `date +%F`（今日）や `date -d yesterday +%F` を使ってよい。
- `--since <git-expr>`（例: `--since "3 days ago"`）が渡されたら期間モード。`<date>` はその期間の終端日にする。
- `--repo <path>` は追加で走査する git リポジトリ（複数可）。省略時は **カレントリポジトリのみ**。
- `--stdout` があれば vault への書き込みを省き、ターミナル出力だけにする。

### Step 1 — Obsidian vault を解決（fail-soft）

1. `~/.session-insights/config.toml` があれば読み、`obsidian_vault` と `record_dir` を採る
   （`obsidian_vault` は `~` 展開する）。
2. 無ければ **既定**にフォールバック: vault=`~/Documents/vault/yukineko` / `record_dir=records`
   （session-insights の既定と一致）。
3. 解決した `<vault>/<record_dir>/` が存在しなければ、その旨を報告し **records 無しで** git だけから日報を作る
   （records が今日 0 件でも同様に続行）。

### Step 2 — git コミットを収集（当該リポジトリ＝変更の一次情報）

対象リポジトリ（カレント＋`--repo` 指定分）ごとに、対象日の**自分のコミット**を集める:

```bash
# 単日
git log --since="<date> 00:00" --until="<date> 23:59:59" --no-merges \
        --pretty=format:'%h%x09%an%x09%ad%x09%s' --date=format:'%H:%M'
# 期間モード（--since 指定時）
git log --since="<git-expr>" --no-merges --pretty=format:'%h%x09%an%x09%ad%x09%s' --date=short
```

- 変更規模は `git log ... --shortstat` か、代表コミットに `git show --stat <sha>` を使う（全 diff 本文は読まない＝コスト節約）。
- コミットが 0 件なら「このリポジトリでのコミットは無し」と控え、records 側の成果に依存する。
- **git の失敗（非リポジトリ等）は fail-soft**: そのリポジトリを飛ばして続行する。

### Step 3 — 対象日の Obsidian record を読む（横断サマリの一次情報）

record ノート名は `<date>-<project>-<session8>.md`。対象日ぶんを列挙して読む:

```bash
ls "<vault>/<record_dir>/" | grep "^<date>-"     # 例: 2026-07-03-*.md
```

各 record から **散文セクション**を抽出する（見出しは session-insights の record フォーマット）:

- `## 完了サマリ` — その session で完了したこと（成果の主材料）
- `## つまずき / 学び` — 学び・気づき
- `## 振り返り / 確立した方針` — 確立した方針
- `## 注意点 / 落とし穴` / `## 残課題` / `## 要追跡 / あとで確認` — 残課題・フォローアップ
- `## 数値サマリ` / `## コスト` — **機械が自動充填した数値**（turns / tool events / files / tokens / cost）。
  数値は**そのまま引用**し、創作しない。

> record が 0 件の日は、git コミットだけから成果を書く（散文は git message から要約）。
> record 本文が薄い（プレースホルダのみ）の session は数値だけ拾って散文はスキップしてよい。

### Step 4 — 日報を合成（LLM の判断）

材料（git コミット群 ＋ record の散文・数値）を統合し、**重複を排除**して1枚に合成する。
同じ作業が複数 session に跨る場合は 1 項目にまとめる。以下の構造で **日本語・エンジニア向け**に出力する:

```markdown
# 日報 — <date>

## 概要
<その日全体を 2〜4 文で。何に取り組み、どこまで進んだか>

## 主な成果
- **<プロジェクト/テーマ>**: <何を実装・修正・決定したか。なぜか>
  - `<sha>` <commit subject>（必要な代表コミットだけ）
- …

## 学び / 気づき
- <record の「つまずき/学び」から抽出。無ければ省略>

## 残課題 / フォローアップ
- <record の残課題・要追跡から。次にやること / ブロッカー>

## 数値
- コミット: <N> 件（<repo 別内訳があれば>）／ 変更ファイル: <shortstat から>
- session: <M> 本 ／ tokens: <record の合計> ／ コスト: <record の合計>
  （record が無い項目は「—」）
```

- **数値は record / git から取った実値のみ**。無い数値は捏造せず「—」やスキップにする。
- 分量はスキャンで読める程度に抑える（全 diff や長い引用は貼らない）。

### Step 5 — 出力（vault へ書き戻す）

- `--stdout` 指定時 → 合成結果を**ターミナルに出力するだけ**で終了。
- 通常時 → `<vault>/daily/` を用意（`mkdir -p`）し、`<vault>/daily/<date>.md` に **Write** する。
  - 既に同名ファイルがあれば **Read して内容を確認**し、追記/更新かを判断する
    （既存が手書きの別内容なら上書きせず、`## 自動生成日報 (<時刻>)` セクションとして追記する）。
  - 書き込み後、**保存先パス**と本文（または要約）をターミナルにも表示して完了報告する。

## ハードルール

- **生成のみ**: session-insights の record ノートは**絶対に書き換えない**（読み取り専用の材料）。
- **数値を捏造しない**: turns / tokens / cost は record の機械充填値、変更規模は git の実値だけを使う。
- **fail-soft**: config 欠如・非 git リポジトリ・records 0 件でも止まらず、拾えた材料で日報を出す。
- **横断は records、変更詳細は git**: プロジェクトを跨ぐ全体像は record が、当該リポジトリの「何をどう変えたか」は git が供給する。両者を突き合わせて合成する。
- **daily/ 以外に書かない**: 出力先は `<vault>/daily/<date>.md` のみ（records/ には書かない）。
