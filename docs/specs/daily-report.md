> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# daily-report 仕様

## 概要

`daily-report` は、その日の作業を **git のコミットログ** と **Obsidian の session record ノート**
（`session-insights` が `<vault>/records/` に書いたもの）から集約し、1 枚の日報（Markdown）に合成して
`<vault>/daily/<date>.md` へ書き戻す **skill-only プラグイン**である。バイナリを持たず（`src/` は無く
Cargo.toml も無い）、`skills/daily-report/SKILL.md` の手順のみで動く subscription-native
（API キー不要）な SOURCE-of-truth digest として構成される。git は当該リポジトリの「何をどう変えたか」
（コミット・変更規模）の一次情報を、Obsidian records は横断プロジェクトの完了サマリ・学び・残課題と
機械が自動充填した turns/tokens/cost の数値を供給する。集約と合成（判断）は LLM が担い、材料集めは
git と（`session-insights` が既に書いた）record が決定論的に供給する。コマンド自身は新しい状態を持たず、
materials を読んで合成するだけで record ノートは書き換えない。

## 不変条件

- **生成のみ・record 非改変（ハード不変条件）** — `session-insights` が書いた `records/` 配下の record
  ノートは読み取り専用の材料であり、絶対に書き換えない（SKILL.md「ハードルール」）。
- **出力先は daily/ のみ** — 書き込みは `<vault>/daily/<date>.md` の 1 箇所に限定する。`records/` を含め
  他の場所には書かない。`allowed-tools` の `Write` はこの出力にのみ使う。
- **数値を捏造しない** — turns / tool events / files / tokens / cost は record が機械充填した実値、変更規模は
  git の実値のみを引用する。存在しない数値は創作せず「—」やスキップにする（Step 4 / ハードルール）。
- **fail-soft** — config 欠如・非 git ディレクトリ・records 0 件のいずれでも止まらず、拾えた材料だけで
  日報を出す。個別リポジトリの git 失敗（非リポジトリ等）はそのリポジトリを飛ばして続行する
  （Step 1〜3、SKILL.md「設計上の約束」）。
- **read-mostly なツール境界** — `allowed-tools` は `Bash(git:*)`/`Bash(ls:*)`/`Bash(cat:*)`/`Bash(mkdir:*)`/
  `Bash(date:*)` と `Read`/`Grep`/`Glob`/`Write` のみ。破壊的 git 操作は宣言されず、git は log/show 等の
  read-only 参照に用いる。
- **全 diff を読まない（コスト節約）** — 変更規模は `git log ... --shortstat` か代表コミットの
  `git show --stat <sha>` で拾い、全 diff 本文は読まない。日報の分量もスキャンで読める程度に抑える。
- **役割分担の固定** — 横断的な全体像は records が、当該リポジトリの「何をどう変えたか」は git が供給する。
  両者を突き合わせて合成し、同じ作業が複数 session に跨る場合は 1 項目に重複排除する。

## 振る舞い

`/daily-report [YYYY-MM-DD | today | yesterday | --since <git-expr>] [--repo <path> ...] [--stdout]`。
引数省略時は今日を対象日にする。SKILL.md の Step 0〜5 を順に実行する。

- **Step 0 — 対象日と設定の決定** — 引数の日付 `YYYY-MM-DD` を `<date>` に採る。`today`/省略＝今日、
  `yesterday`＝前日（`date +%F` / `date -d yesterday +%F` を使ってよい）。`--since <git-expr>`（例
  `"3 days ago"`）で期間モードに入り、`<date>` は期間の終端日にする。`--repo <path>` は追加走査する
  git リポジトリ（複数可、省略時はカレントリポジトリのみ）。`--stdout` は vault 書き込みを省略する。
- **Step 1 — vault 解決（fail-soft）** — `~/.session-insights/config.toml` があれば `obsidian_vault`
  （`~` 展開）と `record_dir` を採る。無ければ既定 vault=`~/Documents/vault/yukineko` /
  `record_dir=records`（session-insights の既定と一致）へフォールバック。`<vault>/<record_dir>/` が
  無ければ報告のうえ records 無しで git だけから日報を作る。
- **Step 2 — git コミット収集** — 対象リポジトリごとに対象日の自分のコミットを集める。単日は
  `git log --since="<date> 00:00" --until="<date> 23:59:59" --no-merges --pretty=...`、期間モードは
  `git log --since="<git-expr>" --no-merges ...`。変更規模は `--shortstat` / `git show --stat`。
  コミット 0 件なら「このリポジトリでのコミットは無し」と控え、records 側の成果に依存する。
- **Step 3 — Obsidian record の読み取り** — record 名は `<date>-<project>-<session8>.md`。
  `ls "<vault>/<record_dir>/" | grep "^<date>-"` で対象日ぶんを列挙し、各 record から散文セクション
  （`## 完了サマリ` / `## つまずき / 学び` / `## 振り返り / 確立した方針` /
  `## 注意点 / 落とし穴` / `## 残課題` / `## 要追跡 / あとで確認`）と数値セクション
  （`## 数値サマリ` / `## コスト`）を抽出する。数値はそのまま引用する。record 0 件の日は git コミット
  だけから成果を書く。本文が薄い（プレースホルダのみ）session は数値だけ拾い散文はスキップしてよい。
- **Step 4 — 日報の合成（LLM 判断）** — git コミット群と record の散文・数値を統合し、重複排除して
  1 枚に合成する。出力構造は `# 日報 — <date>` の下に `## 概要` / `## 主な成果`（プロジェクト別、
  代表コミットのみ `<sha> <subject>`）/ `## 学び / 気づき` / `## 残課題 / フォローアップ` / `## 数値`
  （コミット件数・変更ファイル・session 本数・tokens・コストの実値、無い項目は「—」）。日本語・
  エンジニア向け。
- **Step 5 — 出力** — `--stdout` 指定時はターミナル出力のみで終了。通常時は `mkdir -p <vault>/daily/`
  のうえ `<vault>/daily/<date>.md` へ Write する。既存の同名ファイルがあれば Read して内容を確認し、
  手書きの別内容なら上書きせず `## 自動生成日報 (<時刻>)` セクションとして追記する。書き込み後は
  保存先パスと本文（または要約）をターミナルにも表示して完了報告する。

### 構成

- **形態** — skill-only プラグイン。バイナリ・`src/`・Cargo.toml を持たず、正典は
  `skills/daily-report/SKILL.md`（frontmatter の `name`/`description`/`argument-hint`/`allowed-tools`
  ＋ Step 0〜5 手順＋ハードルール）。hooks・agents は同梱しない。version 整合は
  `.claude-plugin/plugin.json`（`version` 0.1.0）と `.claude-plugin/marketplace.json` の 2 正典で保つ
  （Cargo.toml が無いため lockstep 対象は 2 ファイル）。
- **上流の材料供給者** — `session-insights`（`/record`）が日報の材料となる per-session record を
  `<vault>/records/` に書く。数値（turns/tokens/cost）はその record 側が機械充填する。
- **関連コマンド** — `difflog` は単一セッションの git 差分ナラティブ。`daily-report` はそれを日単位で
  横断集約する上位に位置づけられる。
