# mutategate

このワークスペース向けの **ミューテーションテスト kill-rate ゲート**。内部ツールであり、
配布される Claude Code プラグインではない（`plugin.json` を持たない）。

## なぜ

ゴールデン/リグレッションテストは、コードが *以前と同じ振る舞いを続けている* ことは証明する。
だが、もし欠陥が混入したときにテストがそれを **捕捉できるか** については何も語らない。
ミューテーションテストはその隙間を埋める。小さな欠陥（「mutant」）を注入し、既存テストが
失敗するか（mutant が **caught/killed**）、それでもパスしてしまうか（mutant が
**missed/survived**）を確かめる。生存可能な mutant のうち kill された割合が **kill-rate**
（mutation score）である。スコアが低ければ、どれだけグリーンに見えてもテストスイートは弱い。

背景: Meta の Automated Compliance Hardening (ACH) と PRIMG
(arXiv:2505.05584)。

## 何であり、何でないか

- ミューテーションエンジンを **実装しない**。標準の Rust ツール
  [`cargo-mutants`](https://mutants.rs) の上に立つ。
- **ゲートそのものである**: `cargo-mutants` の `outcomes.json` をパース → kill-rate を計算 →
  閾値を下回ったら非ゼロで exit する。この parse→score→exit のロジックは純粋で、固定の
  サンプル JSON に対してユニットテスト済み（`cargo test -p mutategate`）なので、pass/fail の
  判定は決定論的であり、（遅い）エンジンを起動せずに走る。

ここで用いる kill-rate の定義:

```
viable   = caught + missed + timeout      (unviable な mutant は除外 — シグナルなし)
killed   = caught + timeout               (timeout はテストが露出させた誤動作)
kill_rate = killed / viable               (未定義 -> ゲートは失敗)
```

## 使い方

```sh
# 既存の outcomes.json に対する決定論的ゲート:
cargo run -p mutategate -- --outcomes mutants.out/outcomes.json --min-kill-rate 0.80

# エンドツーエンド（パイロットクレートでエンジンを走らせてからゲート）:
scripts/mutation-gate.sh
PILOT=difflog MIN_KILL_RATE=0.70 scripts/mutation-gate.sh
PILOT=specguard scripts/mutation-gate.sh   # GATE クレート: polarity ゲート (similarity.rs)
```

Exit コード: `0` pass、`1` kill-rate が閾値未満（または生存可能な mutant が無い）、`2`
usage/IO/parse エラー。

## スコープ（意図的に狭い）

ワークスペース全体に対して `cargo-mutants` を走らせるのはゲートにするには遅すぎるため、
パイロットは **少数のクレート** に絞る:

- **パイロット1: `harness-core`**（既定）— 共有のビルド時ロジック。`hash`/`pricing`/`spans`
  は純粋でミューテーションに向く。
- **パイロット2: `specguard`** — GATE クレート（防御ゲート本体）で、
  かつテスト品質検証（mutation testing）が最も必要な対象。`similarity.rs` の
  polarity ゲート（`polarity_preserved` / `polarity_signature` / `triage` 等）は
  過去のレビューで複数回バグが見つかった箇所であり、kill-rate 計測の価値が高い。
  `specguard` 自体は大きいクレート（`main.rs` だけで 2300 行超）なので、
  `scripts/mutation-gate.sh` は `PILOT=specguard` のとき既定で
  `--file crates/specguard/src/similarity.rs` に絞り込んで実行時間を現実的に保つ
  （66 mutant、実測 約3分）。あわせて `ack_blocks_when_no_new_commits_since_raised`
  という1テストを `--skip` している。このテストは `repo_root()` 経由で実際の git
  HEAD を読むが、`cargo-mutants` はソースを `.git` を含まないスクラッチコピー先で
  ビルド・テストするため `scope::current_head` が解決できずベースラインの時点で
  false-flake になる（mutation 由来の欠陥ではなく、cargo-mutants の実行モデルに
  起因するテスト環境依存）。
- `PILOT=<crate>` で任意のクレートに切り替えられる。`MUTANTS_EXTRA="--file <path>"`
  でさらに絞れば高速な実走ができる（`<path>` はこのスクリプトを実行する repo root
  からの相対パス。例: `crates/harness-core/src/hash.rs`）。
  `MUTANTS_EXTRA` を明示指定すると、上記のパイロット別既定値は完全に上書きされる。

**閾値: 0.80。** これは確立されたミューテーションツール（例: PIT）や Meta ACH の系譜が示す
実務上の堅牢性のバーを反映している。これを下回るスイートは、検出可能な欠陥を明らかに
取りこぼしている。パイロットではゲートがフレークでなくシグナルであるよう保守的に保つ。

## 今後の拡張

- クレートは 1 つずつ、それぞれが既に閾値をクリアしてから追加する。そうすれば新しい
  クレートがゲートを黙って引き下げることはない（`specguard` の追加はこの手順に従った:
  `similarity.rs` に絞った状態でまず閾値超過を確認してから `case "$PILOT"` に追加した）。
- スイートが硬くなるにつれ `MIN_KILL_RATE` を引き上げる。生存した mutant は
  `target/mutants-<pilot>/mutants.out/missed.txt` で確認する。
- `specguard` は現時点で `similarity.rs` のみに絞っている。他のファイル
  （`ratify.rs`/`scope.rs`/`specmap.rs` 等）へのパイロット拡大は、それぞれの実行時間と
  ベースラインの安定性（他の git-HEAD 依存テストが無いか）を確認してから行う。
- CI: `.github/workflows/mutation.yml` が手動ディスパッチ・週次スケジュール・ゲート機構に
  触れる PR でパイロットを走らせる — パイロット限定、ジョブ上限 30 分。
