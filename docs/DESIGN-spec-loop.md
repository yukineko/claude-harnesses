# DESIGN — spec→backlog→flow の閉ループと、その停止基準

対象 backlog: `2dd5180f` (p1) / `09148819` (p1) / `d7330aef` (p2) / `197c9fa0` (p2)。
`e664320a` はこの設計により決着済み（wire up ではなく退役を選んだ）。
仕様本体は **`specs/spec-loop.toml`**（specforge Spec IR）。本文書はその散文側で、
**なぜこの形なのか**と**測ったこと**だけを書く。要求そのものは重複させない。

## 1. いま何が繋がっていないか（測定値・2026-07-31・測定点 `0ad0d055`）

- specforge (`crates/specguard/Cargo.toml:16-18`, `src/forge/main.rs`) は
  gather → draft → ratify → impl-prompt → implement → evidence → agree → merge の
  **完結したパイプライン**を持つ。
- `src/forge/` 配下で `overwatch` / `condukt` / `backlog` を grep すると **ヒットは1件**、
  それも `crates/specguard/src/forge/implement.rs:331` の**コメント**。
  つまり批准された spec が queue に入る経路は**無い**。
- repo 内で唯一存在する spec→backlog の受け渡しは **散文**である:
  `crates/specguard/commands/spec-audit.md:167-171` と
  `crates/specguard/commands/drift-map.md:121` が
  「`backlog add` せよ」と LLM に指示している。CLAUDE.md §6 は
  「規範の遵守を実装者の自己申告に依存させない」と書いており、markdown の指示はまさにそれ。
- 付随して観測: **`specforge.toml` がリポジトリに存在しない**。本文書の測定では
  scratchpad の一時 config を `--config` で渡した。すなわち specforge は
  **この repo で一度も走っていない**。R1/R4 を実装するなら config の同梱が前提条件になる。

## 2. cycle の形

```
        ┌────────────── spec-gap を検出 ──────────────┐
        │  (1) interpreter が open_questions を出す    │
        │  (2) task が confidence:low                 │
        │  (3) specguard brief の verdict=not-covered │
        ▼                                             │
   specforge  ──draft──▶  ratify ──【契約 floor】──▶ backlog ──▶ /flow ──▶ condukt
        ▲                    │                                              │
        │                    └─ floor を通らない ──▶ 停止・人間へ            │
        └──────────────────────────────────────────────────────────────────┘
```

これは**閉路**であり、閉路には停止規則が要る。

## 3. 停止基準 — 「condukt 可能か」を機械判定に落とす

当初 backlog `09148819` には「同じ task が2回 divert されたら人間へ escalate」と書いた。
**これは撤回する。** 回数は代理指標であって、知りたいこと（この spec で実装に進めるのか）を
測っていない。1回目で十分鋭い spec が書けたなら止める理由は無いし、5回目でも書けないなら
2回目で止めても意味は同じである。

正しい基準は **「その spec が condukt 可能か」** であり、これは既に機械判定として存在する:

```
crates/specguard/src/forge/ir.rs:162  Spec::contract_violations()
  requirement が1つ以上ある / id・statement が非空 /
  acceptance が非空 (G4 反証可能性) / canon が非空 (G1 接地) / falsifiable=true
```

`ratify` (`src/forge/main.rs:355-362`) はこれが非空なら**批准を拒否**する。
そして「condukt 可能」とは、Spec IR が condukt のタスクへ 1:1 で写るということに他ならない:

| Spec IR | condukt |
|---|---|
| `requirement.statement` | `task.title` |
| `requirement.acceptance` | `task.done_criteria`（逐語。下流で再導出しない） |
| `requirement.canon` | 参照ファイル |
| `requirement.falsifiable` | done_criteria が観測可能であるという主張 |

`acceptance` が空の requirement は `done_criteria` の無いタスクであり、**condukt に渡せない**。
`canon` が空なら接地が無く、実装者は仕様ではなく推測を実装する。したがって:

> **cycle は「floor を通ったか」で抜ける。通れば実行へ、通らなければ人間へ。回数は数えない。**

これは CLAUDE.md §3 とも整合する。floor を通らない draft を「まあ実装できるだろう」と
通すのは**判定不能を clean 側へ倒す**ことであり、§2 が禁じる「判断で ok を出す」そのもの。

### 3.1 測ったこと（判断ではなく観測）

測定点 `0ad0d055`、`cargo build -p specguard --bin specforge`（debug）、
config は scratchpad の一時ファイル（§1 のとおり repo に `specforge.toml` が無いため）。

**GREEN** — 本 spec（`specs/spec-loop.toml`, requirement 6件）に対し `ratify`:

```
specforge: spec を批准した (draft -> ratified) -> .../specs/spec-loop.toml
  canon commit に pin (canon_commit: 0ad0d0555168d0ca709a4002cbbdc0f602121b9a)
exit=0
```

**RED（anti-vacuity 対照）** — floor が実際に何かを見ていることを確かめるため、
`acceptance=[]` / `canon=[]` / `falsifiable=false` の requirement 1件だけを持つ
対照 spec を作って同じコマンドを流した:

```
specforge: error: spec が rigor 契約に違反 — 批准を拒否:
  - requirement 'X1-ungrounded-and-unfalsifiable': acceptance が空 (G4 反証可能性が無い)
  - requirement 'X1-ungrounded-and-unfalsifiable': canon が空 (G1 接地が無い)
  - requirement 'X1-ungrounded-and-unfalsifiable': falsifiable=false (G4 未充足)
exit=2
```

RED を先に見ずに GREEN だけを見ていたら、「floor を通った」は「floor が何も見ていない」と
区別できなかった（CLAUDE.md §2(b)）。対照 spec は測定後に削除した。
本 spec の `status` も `draft` へ戻してある — **批准は人間の儀式**であり、
この測定でハーネスが自分で押してよいものではない。

### 3.2 それでも残る停止の穴（塞いでいない・意図的に明示する）

floor は **spec が書けたかどうか**を判定するが、**同じ task が何度も spec-gap 判定される**こと
自体は止めない。draft が毎回 floor を通り、しかし出てくる requirement が元の task を
解決しない、という発散は理論上ありうる。これを「回数で止める」に戻すのは 3. の理由で誤りなので、
**測ってから決める**: R4 を実装したら divert の回数と、divert 由来 requirement の
done 率を観測し、発散が実在するか確かめる。実在しないものへ先回りで止め木を打たない。

## 4. 依存順（これを崩すと壊れる）

```
R3 (d7330aef, 独立)          R1 (2dd5180f, 独立・本体)
        └──── trigger 3 ────┐        │
                            ▼        ▼
                      R4 (09148819) ─┴─▶ R5 (197c9fa0)
```

- **R1 より先に R5 を landing させない。** 出口が無い状態で実装器を消すと、
  批准された spec に対して**何も実行できない**状態が生まれる。
- R4 の trigger 1・2 は R3 無しで出せる。trigger 3 だけが R3 を待つ。

## 5. 未決の設計判断（判断で埋めない・人間に返す）

**`impl-prompt`（`crates/specguard/src/forge/main.rs` の `ImplPrompt` サブコマンドと
`crates/specguard/src/forge/impl_prompt.rs`）を残すか消すか。**

- 残す論: requirement ごとの実装プロンプトは、実装器が消えた後も
  **condukt interpreter への context provider** として価値がありうる。
- 消す論: R2 で `acceptance` が逐語で backlog へ渡るなら、interpreter は
  spec から直接読める。二重の経路は片方が腐る。

どちらが正しいかは**測っていない**ので、ここでは決めない。R5 の acceptance は
「決めた結果が本文書に明記され、コードがそれと一致すること」だけを要求する
（CLAUDE.md §2: テストで決着できない設計判断は ask に回す）。

## 6. この設計に**含まれない**もの

- `457b44f1` — specguard の findings が overwatch review-queue に載らない件。
  spec の**生成側**（本設計）ではなく**監査側**の配管であり、独立に進めてよい。
- specguard `audit` / `drift-map` の散文 `backlog add` 指示の撤去。R1 が Rust の
  出口を作った後に、同じ理由（§6 自己申告依存）で別途片付ける。

## 7. 批准のしかた

```bash
specforge --config specforge.toml ratify --id spec-loop -m "<なぜこの仕様を受け入れるか>"
```

`specforge.toml` は §1 のとおり**まだ無い**。同梱が R1 の前提作業になる。
批准すると `[spec.ratification]` に canon commit と fingerprint が焼き込まれ、
以後 requirement を1文字でも編集すると fingerprint が変わって**再批准が要る**
（`crates/specguard/src/forge/ir.rs:140` の `fingerprint()`）。
