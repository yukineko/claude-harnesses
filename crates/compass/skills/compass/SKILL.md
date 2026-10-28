---
name: compass
description: ゴール（北極星）と完成定義を彫り直して鮮明にし、現状との gap を出し、焦点に合う右サイズの一手だけを condukt へ渡し、それ以外は保留へ流す再オリエンテーション層。「次に何をすればいいか分からなくなった」「一区切りついて次が無い」瞬間に使う。判定(ゴールを彫る/gap を読む/一手を選ぶ)は LLM、状態維持・size routing は compass バイナリ。subscription で完結(API キー不要)。
argument-hint: '[再接地のフォーカス（任意。例: "認証機能のゴール"）]'
allowed-tools: AskUserQuestion, Bash(compass:*), Bash(git:*), Read, Skill, Task
---

`/compass` で、**charter を彫る → gap を出す → 右サイズの一手を選ぶ → condukt へ渡す**を1サイクル回す。
目的は「候補を列挙する（症状治療）」ではなく **「ゴールを鋭く保ち、次の一手をそこからの差分として導く」**。

```
次の一手 = (鋭いゴール/完成定義) − (現状: git・deepwiki) の 最大かつ右サイズな差分
```

**役割分担（外さない）**: 判定（ゴールを彫る・gap を読む・一手を選ぶ・問い方）は **LLM(=この skill)**、
決定論（C1/C2 floor・状態維持・size routing）は `compass` バイナリ。バイナリは LLM も
AskUserQuestion も呼ばない。**矛盾は人間が裁く**（権威で自動解決しない）。**焦点保護**: 今コミットするのは ONE 右サイズの一手だけ。

## 前提確認

`compass --version`（または `${CLAUDE_PLUGIN_ROOT}/bin/compass`）が通るか確認する。無ければ README の導入手順を案内する。

## C ゲート（charter が「鮮明」か）

| ゲート | 判定 | 担当 |
|---|---|---|
| **C1 存在** | charter.md があり north_star/DoD が空でない | バイナリ（evaluate） |
| **C2 鮮度** | charter が直近のコミット/ファイルから乖離していない | バイナリ（evaluate） |
| **C3 観測可能** | DoD 各項目が観測可能な pass/fail か | **あなた（LLM）** |
| **C4 整合** | north_star/DoD が直近の実作業と矛盾しないか | **あなた（LLM）** |
| **C5 勾配可能** | gap（ゴール − 現状）を計算できる具体度を DoD が持つか | **あなた（LLM）** |

## 手順

### Step 1 — 決定論 floor を読む
```
compass evaluate
```
出力 JSON `{ open_questions, status, round }` を読む。`open_questions` は **C1/C2 の未達だけ**。
`status` が `resolved` でも、C3–C5 はあなたがこれから判定するので、まだ終わりではない。

### Step 2 — LLM ゲート C3–C5 を自分で判定
`compass charter` で現在の charter を読む（＋必要なら `git log --oneline -20` / `compass gap` の出力）。
- **C3**: DoD の各項目は観測可能な pass/fail になっているか。「速くする」「きれいにする」のような測れない項目は未達。
- **C4**: north_star / DoD が直近の実作業（git log）と矛盾していないか。矛盾していれば未達（＝糸が動いた疑い）。
- **C5**: gap が「次の一手を1つ引ける」具体度を持つか。抽象すぎて手が引けないなら未達。

未達ごとに OpenQuestion を1つ立てる（`gate=C3|C4|C5`, `reference`=どのフィールド/DoD項目, `gap`=何が足りないか）。
Step 1 の C1/C2 と合わせて「未解決の問い集合」を作る。

### Step 3 — 彫る（棄却ループ）
未解決の問いが残り、かつ `status != sentinel` の間、**1問ずつ** `AskUserQuestion` で人間に詰める。

**問い方規律（必須）**:
- 選択肢は **具体シナリオ ＋ オプトアウト**（「どれでもない/後で」）。抽象的な二択にしない。
- **最も権威の高い既定（evaluate の `default`、無ければ現 charter の値）を先頭の「推奨」選択肢**に置く。
- **動機の二択は禁止（F1-d）** — 「あなたの motivation は A か B か」のような問いは立てない。
- **矛盾（C4 型）は人間が裁く** — ツールは既定を提案するだけで、**自動で解決しない**。

1問答えるごとに、その回答を記録して C1/C2 を再評価する:
```
compass apply --answer '{"gate":"C3","reference":"definition_of_done[0]","value":"<人間の回答>","defer":false}'
```
- `defer:true` は「後にする」→ 残りを sentinel にして離席を許す。
- C3–C5 の回答も apply に渡す（High 権威の決定として記録される。C1/C2 だけが Rust 側で再判定される＝想定どおり）。
- apply 後の JSON を読み、C3–C5 は**あなたが再判定**する。問いが尽きるか `status` が `resolved`/`sentinel` になったら抜ける。

### Step 4 — 鋭くした charter を保存
合意した内容で charter を組み立て、**観測可能な DoD** にして保存する:
```
compass charter --write '{"north_star":"...","definition_of_done":["観測可能な条件A","条件B"],"measuring_stick":"擁護可能性 × ゴールへの接近距離 ÷ コスト","current_gap":"","next_action":"","parked":[]}'
```
（`current_gap` は Step 5 で埋める。`parked` は route が書かない — route は parked を印字するだけ。）

### Step 5 — gap を出す
```
compass gap                      # 決定論的に集めた入力(DoD/直近活動)を JSON で得る
```
これを読み、**ゴール − 現状** を推論して最大の差分を1〜2行にまとめ、書き戻す:
```
compass gap --write "<gap テキスト>"
```

### Step 6 — 課題化 → condukt で分解（size 付き）
合意した一手を **課題文**にし、文脈（north_star / current_gap / measuring_stick）を添えて condukt に分解させる。
condukt skill を起動する（または「`/condukt <課題>` を実行して」とユーザーに促す）。
**各タスクに `size`(xs|s|m|l|xl) を必ず付与**させる（ルーブリックは DESIGN §13:
xs=1ファイル自明 / s=1–2ファイル単一関心 / m=3–5ファイル1モジュール / l=複数モジュール横断 / xl=要再分解）。
得られた Decomposition JSON をファイル（例 `/tmp/compass-decomp.json`）に書く。

### Step 7 — route（size triage）
```
compass route --file /tmp/compass-decomp.json
```
出力 `{ to_condukt, parked, edge }` を honor する:
- **`to_condukt`** = 今コミットする一手 → そのまま condukt に実行させる（handoff 課題文が後続に印字される）。
- **`parked`** = 今回は着手しない一手。route の出力 JSON に印字されるだけで、どこにも保存されず、次回の gap 入力にも再浮上しない（2026-10-27 ユーザー裁定）。残したいものがあれば停止報告に書くこと。
- **`edge` = `GoalTooBig`** → Step 4 に戻り、ゴールを**より小さく彫り直す**（多くは validate 系の最小スライス）。
- **`edge` = `OnlyNoise`** → north_star 自体を問い直す（方向が尽きた合図、Step 3 へ）。

### Step 8 — outcome（計測ループを閉じる）
`to_condukt` の一手が**完了したら**、その成果を measuring_stick で判定して記録する。これをやらないと
charter に書いた measuring_stick は handoff に印字されるだけで**読み戻されず**、次の gap が「計測された進捗」を
反映できない（build しただけで validate していない状態）。
```bash
compass outcome --verdict <forward|unchanged|backward> --evidence "<観測した成果>"
```
- verdict は move の diff・テスト結果・gap への接近度から**あなた(LLM)が判定**する（前進 / 不変 / 後退）。
- `--evidence` は計測値必須（証拠なしは失敗＝ build ≠ validate）。記録は project の main worktree root の `.compass/outcomes.json`（全 linked worktree で共有。root 判定不能なら拒否）に蓄積され、
  次回 `compass gap` の `last_outcome` として再浮上する。
- `/flow` 経由で回している場合は sink（Step 3-3）が自動でこれを呼ぶので、手動の Step 8 は不要。

## 焦点保護（最重要 / B案）
condukt は並列実行できるが、**compass は「今コミットする一手」を1つに絞る＝糸を増やさない**。
右サイズが複数あっても、gap の主筋に最も効く1件（＋密結合な最小集合）だけを渡し、残りは保留する（route が印字するだけで保存はされない）。

## 失敗モード
- `compass` 不在 → プラグイン導入を案内。
- carve をやり直したい → `compass carve-reset` で状態をクリアしてから Step 1 へ。
- 人間が席を外した（sentinel）→ いま分かっている範囲で charter を保存し、残りは停止報告に書いて終える（保留ストアは無い）。
