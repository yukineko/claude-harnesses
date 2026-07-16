# 敵対的検証パネル (adversarial verification panel) — 設計メモ / 試作

`docs/loop-engineering-evaluation.ja.md` の「気になったギャップ」で指摘された論点への
**試作 (prototype)** の設計メモ。

## 背景 — 埋めるべきギャップ

loop-eng 評価はこう指摘した:

> verify系(propguard/reviewgate/specguard等)は基本的に**単一パスの自己検証 or 単一subprocess
> チェッカー**で、独立した複数の懐疑者が投票する「adversarial verify」パターン(N人の独立
> skeptic に refute させて多数決を取る)は見当たらない。propguard の subprocess モードも
> 1チェッカーのみ。重要な完了判定(GATEクレート自体の変更など)に限り、複数の独立
> チェッカーによる多数決を挟む価値はあるかもしれない。

失敗モードは **共有盲点 (shared blind spot)**: 単一の verifier が worker と同じ盲点を
持つと、「もっともらしいが誤り」の変更を素通りさせる。重要な完了判定ほどこの取りこぼしの
コストが高い。

## 既存資産との関係 — `consensus` とは直交・相補

condukt には既に `consensus.rs`(multi-sample self-consistency)がある。**両者は別の軸を
fan-out する**ので、置き換えではなく相補:

| | `consensus`（既存） | `adversarial`（本試作） |
|---|---|---|
| fan-out するもの | **生成** (generation) | **検証** (verification) |
| 何が N 個か | 同一タスクの独立実装候補 N | 同一 1 成果物を見る独立 skeptic N |
| 対抗する盲点 | 生成側(verifier が見たのは worker が書いた唯一候補) | 検証側(単一 verifier が worker と盲点を共有) |
| 投票の意味 | どの候補が勝つか(多数決) | この 1 変更を refute できるか(反証多数決) |
| 極性 | 勝者選択 + 低合意で opus へ escalate | **fail-closed** ゲート(refute 多数で block) |
| 出力 | winner / agreement_rate / escalate | block / escalate / pass |
| 発火 | opt-in(config/env)または high-risk flag | opt-in(env)または **GATE crate 変更** |

要するに consensus は「N 通り作って多数決」、adversarial は「1 つを N 人で叩いて通過可否」。

## 決定論コア(`crates/condukt/src/adversarial.rs`)

非決定な部分(N 体の独立 skeptic subagent を、できれば**別モデル**で起動して反証させる)は
`/condukt` SKILL 側のオーケストレーションに置き、**このモジュールは決定論の判定核のみ**を持つ
(consensus と同じ source↔executor 分離)。

### 型

- `enum Ballot { Refute, Pass, Abstain }` — 各 skeptic の1票。`Refute`=具体的欠陥を発見、
  `Pass`=反証を試みたが崩せなかった、`Abstain`=判断不能(票を投じず effective を下げる)。
- `struct Vote { skeptic, ballot, reason }` — 1 skeptic の verdict(`reason` は Refute の根拠)。
- `struct Policy { min_voters, block_ratio, escalate_on_dissent }` — 判定の締め方。
  既定 `min_voters=2` / `block_ratio=0.5` / `escalate_on_dissent=true`。
- `struct Panel { n, refutes, passes, abstains, effective, refute_ratio, block, escalate,
  outcome, ... }` — 判定結果。`outcome ∈ {"block","escalate","pass"}`。

### 判定規則 `adjudicate(votes, policy) -> Panel`(決定論・順序非依存・fail-closed)

1. **effective < min_voters** → **block**。反証検証が成立しない完了に benefit-of-the-doubt を
   与えない(1 人パネルは adversarial ではない)。
2. **refutes/effective ≥ block_ratio**(inclusive、なので偶数分割=tie も block)→ **block**。
3. **少数反証**(refute があるが block 閾未満)→ `escalate_on_dissent` なら **escalate**
   (高リスクゲートでの少数意見は人間/上位レビューへ)。
4. それ以外(反証ゼロ・十分な投票数)→ **pass**。

`RATIO_EPSILON` で 2/4=0.5 のような厳密多数を inclusive-block 側に載せる
(mutategate の `KILL_RATE_EPSILON` と同型)。

### 発火判定 `plan` / `touches_gate_crate`

- `touches_gate_crate(files) -> bool` — 変更パスが `crates/<gate>/` 配下(GATE_CRATES:
  blastguard/propguard/specguard/stuckguard/mutategate)に触れるか。`docs/specs/blastguard.md`
  のような単なる言及は**発火しない**(セグメント一致)。
- `plan(global_enabled, size, policy, high_stakes) -> PanelPlan` — global switch か high_stakes の
  いずれかで engage。engage 時 `size` を `[2, MAX_PANEL=5]` にクランプ(consensus::plan と同型、
  `state autonomy-check` の exit-code 契約と揃える)。

## CLI(試作の配線 — dead-code を避け、実際に叩ける)

```sh
# パネルを張るべきか(exit 0=張る / 1=通常の単一 verifier)。GATE crate 変更で強制発火。
condukt adversarial plan --touched crates/blastguard/src/detect.rs
CONDUKT_ADVERSARIAL=1 condukt adversarial plan            # global switch でも発火

# N 体の skeptic 票(JSON: bare 配列 or {"votes":[...],"min_voters":2,"block_ratio":0.5})を判定
echo '[{"skeptic":"s1","ballot":"refute","reason":"detect.rs:477 fail-open"},
       {"skeptic":"s2","ballot":"refute"},{"skeptic":"s3","ballot":"pass"}]' \
  | condukt adversarial adjudicate            # exit 1 + {"outcome":"block",...}
```

exit code 契約(consensus と同型): `adjudicate` は **pass=0 / block・escalate=1**。
`outcome` フィールドが block と escalate を区別するので、skill は hard error と取り違えない。

## LLM オーケストレーションの継ぎ目(SKILL 側・未配線)

`/condukt` の Phase 6(verifier)で `plan` を叩き、engage なら単一 verifier の代わりに
**N 体の独立 skeptic subagent を(できれば別モデルで)起動**して「既定 REFUTED。コード上の
根拠で反証できたら Refute、崩せなければ Pass」を返させ、その票を `adjudicate` に流す。
これは `/overwatch:continuous-audit` の finder→refute-verifier→多数決と同じ思想の、
**完了判定への内製化**版。

## 試作の境界(意図的に未実装)

- **live verify は一切変えていない**。既定 off の opt-in で、決定論核 + CLI + テストのみ。
  稼働中のゲート挙動は不変(高リスク変更を試作で不安定化させない)。
- **config.rs 統合済み(2026-07-13)**。`[adversarial]` config セクション
  (`enabled`/`size`/`min_voters`/`block_ratio`) と env `CONDUKT_ADVERSARIAL` を `Config::load`
  で解決する(consensus と同型: env が config.toml を上書き)。`condukt adversarial plan/adjudicate`
  は config 由来の switch と policy を使う(CLI flag > JSON > config > 既定)。既定は enabled=false。
- **SKILL 配線は実施済み**(`crates/condukt/skills/condukt/SKILL.md` Phase 6 冒頭)。
  `condukt adversarial plan` の exit code でパネル起否を判定し、engage 時は
  `condukt state skeptic-model --worker <model> --index <k>`(新規追加。TIERS のうち worker
  と同じ tier を除外し、残り tier に index で round-robin して割り当てる。verifier-model と
  同型の決定論)で N 体の独立 skeptic をモデル多様性つきで並列 Task 起動し、
  `condukt adversarial adjudicate` の outcome で verified/failed/escalate に分岐する。
  非 engage 時は既存の単一 verifier 手順を無変更で実行する。
- 汎用化の余地: 現状 condukt-local。propguard/reviewgate からも使うなら `harness-core` へ
  昇格(全 plugin バイナリに焼き込まれる共有層)を検討。

## テスト

`adversarial.rs` の `#[cfg(test)]`:
- example: 全員 pass→pass / 多数 refute→block / tie→block(fail-closed) / min_voters 未満→block /
  全員 abstain→block / 少数反証→escalate / escalate 無効時は pass / abstain が分母を下げる /
  厳しい block_ratio で単一反証→block / `touches_gate_crate` の検出と substring 安全性 /
  `plan` の既定 off・high_stakes 強制・global 発火・クランプ。
- proptest: `adjudicate` の順序非依存・outcome の排他網羅・件数保存・no-panic(任意票×任意 policy)。
