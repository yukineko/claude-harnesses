# daily-report — git ＋ Obsidian から日報を合成する

`/daily-report` は、その日の **git コミットログ** と **Obsidian の session record ノート**
（session-insights が `<vault>/records/` に書いたもの）を材料に、1 枚の **日報 (Markdown)** を合成し、
`<vault>/daily/<date>.md` へ書き戻すコマンドです。

```
材料（決定論的に収集）                合成（LLM）              出力
  git log（当該リポジトリの変更）  ─┐
  Obsidian records（横断サマリ・   ├─▶  日報ナラティブ  ─▶  <vault>/daily/<date>.md
    学び・残課題・コスト/トークン） ─┘                        （＋任意で stdout）
```

- **git** … 当該リポジトリで「何をどう変えたか」（コミット・変更規模）の一次情報。
- **Obsidian records** … 複数プロジェクトを跨ぐ「完了サマリ・学び・残課題」と、機械が自動充填した
  turns/tokens/cost の数値。
- **このコマンド自身は新しい状態を持たない**。materials を読んで合成するだけで、record は書き換えません。

## 使い方

```
/daily-report                    # 今日ぶんを合成 → <vault>/daily/<今日>.md へ
/daily-report yesterday          # 前日ぶん
/daily-report 2026-07-01         # 指定日
/daily-report --since "3 days ago"   # 期間モード
/daily-report --repo ../other-repo   # 追加リポジトリも走査（複数可）
/daily-report --stdout           # vault に書かずターミナル出力だけ
```

## vault の解決

1. `~/.session-insights/config.toml` があれば `obsidian_vault` / `record_dir` を採用（`~` 展開）。
2. 無ければ既定 `~/Documents/vault/yukineko` ＋ `records`（session-insights と同一既定）。

## 設計上の約束（fail-soft）

- config 欠如・非 git ディレクトリ・records 0 件でも止まらず、拾えた材料だけで日報を出す。
- 数値（turns/tokens/cost・変更規模）は record と git の**実値のみ**。無い数値は捏造しない。
- 出力は `<vault>/daily/<date>.md` のみ。`records/` には決して書き込まない。

## 関連

- `session-insights` (`/record`) … 日報の材料となる session record を `<vault>/records/` に書く。
- `difflog` … 単一セッションの git 差分ナラティブ（本コマンドは日単位で横断集約する上位）。

Subscription-native（skill のみ・バイナリ無し・API キー不要）。
