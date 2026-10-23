---
name: daily-report
description: その日の作業を git のコミットと session record から1枚の日報にまとめたいときに使うこと。
argument-hint: "[YYYY-MM-DD | today | yesterday | --since <git-expr>] [--repo <path> ...] [--stdout]  省略時は今日"
allowed-tools: Bash(git:*), Bash(ls:*), Bash(cat:*), Bash(mkdir:*), Bash(date:*), Read, Grep, Glob, Write
---

## ハードルール（禁止事項を最優先で守ること）

- session-insights の record ノートは絶対に書き換えないこと。
- 数値を捏造しないこと。
- 全 diff 本文は読まないこと。
- `<vault>/daily/<date>.md` 以外には書かないこと（`records/` には決して書かないこと）。
- turns / tokens / cost は record の機械充填値、変更規模は git の実値だけを使うこと。
- config 欠如・非 git リポジトリ・records 0 件でも止まらず、拾えた材料で日報を出すこと。
- プロジェクトを跨ぐ全体像は record から、当該リポジトリの変更詳細は git から取り、両者を突き合わせて合成すること。

その日の git コミットと Obsidian の session record を材料に、スキャンで読める日報（Markdown）を合成すること。
新しい状態を作らず、材料を読んで合成するだけにすること。

- その日一日の作業を振り返り、日報 / standup メモを1枚にまとめたいときに使うこと。
- 複数プロジェクトを跨いで作業した日の成果を、記録済みの record から横断集約したいときに使うこと。

### Step 0 — 引数から対象日と設定を決めること

- 引数に日付 `YYYY-MM-DD` があれば、それを対象日にすること。
- 引数が `today` または省略なら今日、`yesterday` なら前日を対象日にすること。
- 日付の解決には、今日は `date +%F`、前日は `date -v-1d +%F`（BSD/macOS の date）か `date -d yesterday +%F`（GNU date）を使うこと。
- `--since <git-expr>` が渡されたら期間モードにし、`<date>` をその期間の終端日にすること。
- `--repo <path>` が渡されたら、そのリポジトリも追加で走査すること。省略時はカレントリポジトリのみを走査すること。
- `--stdout` が渡されたら、vault へ書き込まずターミナル出力だけにすること。

### Step 1 — Obsidian vault を解決すること

1. カレントディレクトリに `session-insights.toml` があればそれを、無ければ `~/.session-insights/config.toml` を読み（2 つはマージせず、先に見つかった 1 つだけを使う）、`obsidian_vault` と `record_dir` を採ること。
2. `obsidian_vault` は `~` を展開すること。
3. config が無ければ既定にフォールバックし、vault は `~/Documents/vault/yukineko`、record_dir は `records` を使うこと。
4. 解決した records ディレクトリが存在しなければ、その旨を報告し、records 無しで git だけから日報を作ること。records が 0 件でも同様に続行すること。

### Step 2 — git コミットを収集すること

- 全 diff 本文は読まないこと。
- 対象リポジトリ（カレントと追加指定分）ごとに、対象日のコミットを作者で絞らずに集めること。
- 単日なら次の git log を実行すること。

```bash
git log --since="<date> 00:00" --until="<date> 23:59:59" --no-merges \
        --pretty=format:'%h%x09%an%x09%ad%x09%s' --date=format:'%H:%M'
```

- 期間モードなら次の git log を実行すること。

```bash
git log --since="<git-expr>" --no-merges --pretty=format:'%h%x09%an%x09%ad%x09%s' --date=short
```

- 変更規模は `git log --shortstat` か、代表コミットに対する `git show --stat <sha>` で調べること。
- コミットが 0 件なら「このリポジトリでのコミットは無し」と控え、records 側の成果に依存すること。
- git が失敗した（非リポジトリ等の）場合は、そのリポジトリを飛ばして続行すること。

### Step 3 — 対象日の Obsidian record を読むこと

- record の数値を創作しないこと。

対象日の record ノート（ファイル名は `<date>-<project>-<session8>.md`）を列挙して読むこと。

```bash
ls "<vault>/<record_dir>/" | grep "^<date>-"
```

各 record から次の散文セクションを抽出すること（見出しは session-insights の record フォーマットに従うこと）。

- `## 完了サマリ` から、その session で完了したことを成果の主材料として抽出すること。
- `## つまずき / 学び` から、学び・気づきを抽出すること。
- `## 振り返り / 確立した方針` から、確立した方針を抽出すること。
- `## 注意点 / 落とし穴`、`## 残課題`、`## 要追跡 / あとで確認` から、残課題・フォローアップを抽出すること。
- `## 数値サマリ` と `## コスト` から、機械が自動充填した数値（turns / tool events / files / tokens / cost）をそのまま引用すること。

> record が 0 件の日は、git コミットだけから成果を書き、散文は git message から要約すること。
> record 本文が薄い（プレースホルダのみ）session は、散文はスキップしてよいが、数値は拾うこと。

### Step 4 — 日報を合成すること

- 無い数値を捏造しないこと。
- 全 diff や長い引用は貼らないこと。

材料（git コミット群と record の散文・数値）を統合し、重複を排除して1枚に合成すること。
同じ作業が複数 session に跨る場合は 1 項目にまとめること。
次の構造で日本語・エンジニア向けに出力すること。

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

- 数値は record / git から取った実値のみを使い、無い数値は「—」やスキップにすること。
- 分量はスキャンで読める程度に抑えること。

### Step 5 — 出力すること

- 既存が手書きの別内容なら上書きしないこと。
- `--stdout` 指定時は、合成結果をターミナルに出力するだけで終了すること。
- 通常時は `mkdir -p` で `<vault>/daily/` を用意し、`<vault>/daily/<date>.md` に Write すること。
- 既に同名ファイルがあれば Read して内容を確認し、追記か更新かを判断すること。
- 既存が手書きの別内容なら、別セクションとして追記すること。
- 書き込み後、保存先パスと本文（または要約）をターミナルにも表示して完了報告すること。
