この crate の存在を coverage と読まないこと。

ホスト型 CI からこのゲートを起動しない。再結線は §7 に従い local（pre-push または明示スクリプト）で行うこと。

`.githooks/` 配下のどの git hook も `scripts/mutation-gate.sh` を参照せず、`.github/workflows/` ディレクトリも無いため、kill-rate ゲートは INERT であり、人間が手で叩いたときだけ動く。

内部ツールであり、配布される Claude Code プラグインではない（`plugin.json` を持たない）。

背景として Meta の Automated Compliance Hardening (ACH) と PRIMG (arXiv:2505.05584) を参照すること。

このゲートは `cargo-mutants` の `outcomes.json` をパースし、kill-rate を計算し、閾値を下回ったら非ゼロで exit する。

ここで用いる kill-rate の定義:

```
viable   = caught + missed + timeout + unknown   (unviable な mutant は除外 — シグナルなし)
killed   = caught + timeout                      (timeout はテストが露出させた誤動作)
kill_rate = killed / viable                      (未定義 -> ゲートは失敗)
```

`unknown` は、この crate が認識しない `summary` 状態の mutant、または `summary` を欠く record。

`scenario` が `"Baseline"` でも `Mutant` オブジェクトでもない record（null・欠落を含む）も `unknown` に数える。

`viable` には数えるが `killed` には数えないので、未知の状態は分母から消えて kill-rate を水増しするのではなく、生き残った mutant として扱われる。

`unknown` が 1 件でもあれば理由文に警告が付く。

```sh
# 既存の outcomes.json に対するゲート
cargo run -p mutategate -- --outcomes mutants.out/outcomes.json --min-kill-rate 0.80

# エンドツーエンド: パイロットクレートでエンジンを走らせてからゲート
scripts/mutation-gate.sh
PILOT=difflog MIN_KILL_RATE=0.70 scripts/mutation-gate.sh
PILOT=specguard scripts/mutation-gate.sh
PILOT=condukt scripts/mutation-gate.sh
PILOT=blastguard scripts/mutation-gate.sh

# このゲートが何を・何から・何を根拠に守るかを、判定なしで表示
cargo run -p mutategate -- --protects
```

既定値: `--outcomes mutants.out/outcomes.json`、`--min-kill-rate 0.80`。

`1e-9` 以下（0・負値）の `--min-kill-rate` は exit `2` で拒否される。

Exit コード: `0` pass、`1` kill-rate が閾値未満（または生存可能な mutant が無い）、`2` usage/IO/parse エラー。

FAIL 時は overwatch の fleet violation ledger に違反イベントも追記する。

この書き込みに失敗した場合は注記を出すが、FAIL の判定はそのまま維持される。

`scripts/mutation-pilots.sh` は変更パスの一覧（stdin）を、走らせるべき較正済みパイロットクレートに写像する。

どれにも当たらなければ `harness-core` に倒し、空の一覧は決して出さない。

- パイロット1: `harness-core`（既定）。
- パイロット2: `specguard` — 既定で `--file crates/specguard/src/similarity.rs` に絞り込み、`ack_blocks_when_no_new_commits_since_raised` という1テストを `--skip` する。
- パイロット3: `condukt` — 既定で `--file crates/condukt/src/circuit.rs` に絞り込む。
- パイロット4: `blastguard` — 既定で `--file crates/blastguard/src/classify.rs` に絞り込む。
- `PILOT=<crate>` で任意のクレートに切り替えられる。`MUTANTS_EXTRA`（例: `MUTANTS_EXTRA="--file crates/harness-core/src/hash.rs"`）は cargo-mutants にそのまま渡され、スクリプトは呼び出し元のディレクトリに関係なく repo root から cargo-mutants を走らせる。空でない `MUTANTS_EXTRA` を指定すると、パイロット別の既定値は完全に上書きされる。

 blastguard と同時に計測した GATE クレートのうち stuckguard / propguard はバーに届かず、overwatch はベースライン失敗で計測できなかったため、3 つともパイロットにしなかった。

クレートは 1 つずつ、それぞれが既に閾値をクリアしてから追加すること。

スイートが硬くなるにつれ `MIN_KILL_RATE` を引き上げる。

スクリプトは cargo-mutants に `--output target/mutants-<pilot>` を渡し、`target/mutants-<pilot>/mutants.out/outcomes.json` を採点する。

 この時点では `scripts/mutation-gate.sh` を実行する箇所は無く、kill-rate ゲートは人間が手で叩いたときだけ動いた。
