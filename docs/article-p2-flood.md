---
title: "GAN は悪くない — P2 の洪水と「テストで再現できたものだけを課題にする」"
emoji: "🌊"
type: "tech"
topics: ["llm", "claudecode", "テスト", "設計", "コードレビュー"]
published: false
---

<!-- record: 2026-10-15 -->

## この記事は何か

自作の Claude Code ハーネス群（Rust モノレポ）では、敵対的レビュー（finder と verifier を分けた GAN 的な監査）や
セッション中の監査が、見つけた問題を backlog に積んでいく。
その backlog が、**P2（低優先度）の課題で溢れた**。

P2 は間違っていたわけではない。ただ**証拠がなく、小さかった**。
それでも数が多いと、token を使う優先順位が狂い、本来の問題が後回しになる。

この記事は、その backlog 1,643 件を実測し、原因と対策を検証した記録である。
数字はすべて、git 管理下の backlog store（`.backlog/tasks.toml` / `.backlog/tasks.done.toml`）を
測定点 `9c7a54d5`、測定日 2026-10-15 に集計したもので、付録のスクリプトで再現できる。

---

## 1. 症状: P2 が一番多く、一番実を結ばない

| 優先度 | 件数 | 修正を観測（F→P） | 証拠なしで破棄 |
|---|---|---|---|
| P0 | 136 | 0 | 0 |
| P1 | 645 | 70（10.9%） | 85（13%） |
| **P2** | **698** | **8（1.1%）** | **310（44%）** |
| P3 | 65 | 0 | 49（75%） |

「修正を観測（F→P）」は、修正前の rev でテストが落ち、HEAD で通ることを backlog が記録した件である。
この仕組みは 10 月に入ったので、P0 の 0 件はその時期の影響を受けている。

P2 は全体の 42% を占め、P1 より多い。修正が観測で確かめられたのは 8 件だけだった。

### 10 月に P2 と P1 が逆転した

| 作成月 | P1 | P2 | P2/P1 |
|---|---|---|---|
| 7月 | 146 | 118 | 0.81 |
| 8月 | 131 | 75 | 0.57 |
| 9月 | 243 | 201 | 0.83 |
| 10月 | 93 | 265 | 2.85 |

### 優先順位が逆転していた

done になった P2/P3 は 287 件。そのうち **241 件（84%）は、より古い P0/P1 が未解決のまま閉じられていた**。
P2 は見つかるだけでなく、直す作業にも token を使っていた。

なお、ある 12 日間だけを見ると、閉じられたのは P1 が 67 件、P2 が 7 件で、P1 優先で回っていた。
ただしそれは人間が flow に「P1 をやれ」と指示したからで、仕組みが自分でそう選んだのではない。
**自動で回すと、見つけたものを追いかけ続けて、問題を見失う。**

---

## 2. 最初の仮説: 「見つけたい欲」

最初の仮説は、GAN や監査の「発見したい欲」が強すぎる、というものだった。

半分は当たっていた。finder は偽陽性を出しても何も払わない。
「問題を探せ」というプロンプトには「問題がある」という前提が入っていて、LLM は空の答えを返しにくい。

ただ、データを見ると、効いているのは欲の強さではなく構造だった。

### finding を出すのにコストがかからない

GAN では discriminator が間違えると罰を受ける。finder にはそれが無い。
**出すのはタダで、消すのは高い。**

### 積み上がる一方で、減らなかった

| 月 | 作成 | done | cancel | 月末の未解決 |
|---|---|---|---|---|
| 6月 | 81 | 59 | 0 | 22 |
| 7月 | 352 | 238 | 6 | 130 |
| 8月 | 276 | 40 | 1 | 365 |
| 9月 | 528 | 428 | 11 | 454 |
| 10月 | 406 | 169 | 622 | 69 |

未解決は 9 月まで毎月増えた。**修正で減った月は一度もない。**
減ったのは、10-03（149 件）と 10-15（461 件）の 2 回の一括破棄だけである。

### 検証が新しい finding を生む

backlog の 1,643 件のうち **620 件（38%）が、以前の backlog item を notes か title で参照**していた。
参照をたどった連鎖は、深さ 2 以上が 181 件ある。一番深い連鎖はこうだった。

```
625aa170 P2  並列作業の runtime conflict 検知（機能追加）
 └ 91b4a26b P2  625aa170 follow-up
   └ e117d351 P2  91b4a26b (v2)
     └ 7a1beb86 P2  merge-hold liveness
       └ 617b3112 P0  merge gate の fail-open
         └ 7fb00031 P1  merge() が conflict 記録の失敗をログだけで流す
```

各ステップは正しい対応である。ただ、6 段たどったあとで「もともと何を解きたかったのか」を問う手順はどこにも無かった。
検証の途中で別の小さな finding が見つかって登録される「派生 finding」も、P2 の 10% を占めた（P0 では 4%）。

---

## 3. 「間違い」ではない — 証拠がなく、小さい

P2 を「間違い」と「些少」に分けようとして、人間に訂正された。
**P2 は間違ってはいない。ただ証拠がなく、問題が小さい。そこにこだわると、本来の問題を解決できず、ROI が下がる。**

データもそれを支持した。10-04 に「unproven」として閉じた 12 件は、テストを書くと HEAD で通ったか、
テストで測れないものだった。主張が間違っていたのではなく、すでに直っていたか、測る手段が無かった。

今朝（10-15）の一括破棄はこうだった。

- 破棄 461 件。破棄理由は「no RED evidence recorded; user ruling 2026-10-15」
- 再現テストを試した記録があるのは **1 件だけ**
- notes の合計は **41.7 万字**（調査に使った token は別にかかっており、測れていない）
- 内訳は P2 298 件（65%）、P1 85 件、P3 49 件、優先度なし 29 件

**証拠を出せないものを課題に上げるのは、それ自体が迷惑である。** 記録・triage・ruling・一括破棄と、
触られるたびに token を食う。

---

## 4. 反論: 検証は要らないのか

主張を「GAN の結果は検証しないとゴミが積み上がる」に絞り、反論を当てた。

| 反論 | 判定 | 根拠 |
|---|---|---|
| 積み上がっても優先順位さえ守れば無害 | 退けられる | 上の 241/287 件。backlog は SessionStart hook で毎セッション context に注入されるので、量だけで token を食う |
| P0 を逃すコストに比べれば錯誤は安い | 重要度による | P0 級なら正しい。P2 の修正観測率は 1.1% で、取りこぼしの損失は小さい |
| 検証するより直す方が安い | 一部成り立つ | doc の行番号ずれなど。ただし錯誤を直すのは不要な変更を入れること |
| 検証も間違える | 成り立つ（最も強い） | 根拠なしの REFUTED を禁じた結果、反証できない UNVERIFIED がキューに残り続ける |
| 錯誤にも学習価値がある | 部分的 | 価値は finder ごとの精度という統計に出る。1 件ずつタスクにしなくても得られる |

主張の核は崩れない。どの反論も「検証が要らない」までは届かない。
ただし、検証は誤るので、**検証を挟むだけでは積み上がりは止まらない**。反証できなかった UNVERIFIED を
流し出す出口まで決めて、初めて閉じる。

**GAN そのものは悪ではない。** 修正を観測できた 78 件のうち 75 件は、監査（audit）由来だった。
本物を見つける主な手段は GAN である。悪いのは、**検証せずにタスクにすること**である。

---

## 5. 設計: テストで再現できたものだけを課題にする

答えは「欲を抑える」ではなかった。**finding を出す側に、証拠のコストを払わせる**ことだった。

1. GAN の結果は、すべて記録する（捨てない）
2. 利害関係のない別 context の別 agent が、再現テストを書く
3. そのテストが HEAD で**挙動として失敗した**ときだけ、backlog のタスクにする
4. 再現しなかったものはタスクにせず、unproven の記録に残す
5. 検証者は判定だけを返し、新しいタスクを生まない

backlog はこれを `backlog add --repro-test` として実装している（commit `fd7ad41f`、2026-10-04）。
`--repro-test` なしの add は exit 2 でタスクを作らず、`.backlog/unproven.toml` に追記する
[test: crates/backlog/tests/unproven_record.rs::add_without_repro_test_appends_unproven_record_and_files_no_task]。

結果、今の pending 69 件は **69 件すべてが再現済み**で、そのうち P2 は 36 件ある。
**P2 は全部が要らないわけではない。** 再現できた P2 は、ちゃんとキューに残る。

錯誤はテストで再現しないので、タスクにならない。backlog の中で繰り返しコストを生むこともない。
一括破棄で本物を捨ててしまっても、再現テストを付ければ再提出できる。本物なら RED が出て戻ってくるし、
錯誤なら戻ってこない。**選別をテストに任せたので、捨て方が粗くても回復できる。**

---

## 6. 設計そのものを検証したら、GAN 本体の経路が未移行だった

設計が本当に動いているかを確かめた。`backlog add` を呼ぶ箇所を調べると、flow・condukt・scout の skill は
`--repro-test` を渡していた。ところが、continuous-audit の CONFIRMED finding を backlog に送る
overwatch の bridge（`crates/overwatch/src/bridge.rs`）は、渡していなかった。

コードを読んだだけでは予測にすぎないので、利害のない別モデルの agent に再現テストを書かせ、RED を観測した。

| 仮説 | 結果 |
|---|---|
| H1: bridge は、再現テストが見つかって実際に失敗していても、`--repro-test` を渡さないのでタスクを作れない | RED |
| H2: add が全部拒否されても、bridge は exit 0 で、成功と区別できない | RED |
| H3: continuous-audit のスクリプトは、転送の失敗時にも PASS を出す | 予測どおりの経路では GREEN（PARTIAL を出していた）。代わりに、bridge が exit 3 で終わる別経路で PASS を出していた |
| H4: `--repro-test` なしの add は unproven に記録される | GREEN（記録の仕組みは動いている） |

H3 は、コードを読んだ段階の予測が外れた例である。予測で直していたら、存在しない欠陥を直し、
実在する別の欠陥を見逃していた。

修正は commit `2bd3f28c` で入れた。

- bridge は、見つけた回帰テストを `--repro-test` として渡す
  [test: crates/overwatch/tests/bridge_repro_evidence.rs::confirmed_finding_with_failing_ignored_repro_test_is_filed_with_repro_test]
- 拒否された add と失敗した add を、JSON summary の `unproven` と `failed` に数える
  [test: crates/overwatch/tests/bridge_repro_evidence.rs::refused_backlog_add_is_distinguishable_from_success]
- スクリプトは bridge の終了コードも読み、exit 3 を PASS にしない
  [test: crates/overwatch/tests/continuous_audit_to_backlog_fail.rs::undetermined_bridge_ledger_does_not_print_pass]
- 転送が 1 件でも失敗した run は、FAIL を出して exit 1 で終わる
  [test: crates/overwatch/tests/continuous_audit_to_backlog_fail.rs::refused_backlog_add_does_not_exit_zero]

最後の点は、最初は「PARTIAL（一部失敗）を出して exit 0」だった。人間の指摘は短かった。
**「そもそも PARTIAL なんてものが存在してはいけない。成功じゃないものは失敗」。**
第三の状態は、成功と失敗の区別を下流から消す。PARTIAL を廃止して exit 1 にしたところ、既存のテストが
1 本落ちた。そのテストは、転送が実際に失敗していた run を「exit 0 であるべき」と assert していた。
**テストの方が、失敗を成功として固定していた。** テストの環境を、転送を受理する偽の backlog に直した。

テストは偽の backlog を使うので、それとは別に、本物の backlog バイナリでも確かめた。
使い捨ての linked worktree で、bridge と同じ形式の `--repro-test` を渡すと「filed as pending」になった。
同じ確認を primary の作業ツリーで行うと、backlog は add 自体を拒否した。

皮肉なことに、今朝の一括破棄には `dea7ed1d`（bridge は add の拒否を warning で読み飛ばす）と
`df3d6243`（continuous-audit のスクリプトが転送の失敗時にも PASS を出す）が含まれていた。
**証拠が無いという理由で捨てられた指摘の近くに、本物の欠陥があった。** そして、テストを書いたら RED が出て、修正までたどり着いた。
第 5 節の「捨て方が粗くても回復できる」は、この件で実際に起きた。

---

## 7. 形式の監査と、場当たりの監査

監査由来の finding は 2 種類あった。

| 種類 | backlog の件数 | 修正を観測 | 破棄 | 10-15 時点で未解決だった件 |
|---|---|---|---|---|
| continuous-audit（finder≠verifier、CONFIRMED のみ送る） | 60 | 2 | 2 | 2（2 件とも破棄） |
| セッション中の場当たりの監査 | 523 | 73 | 202 | 204（201 件を破棄） |

破棄率だけを比べると、形式の監査の方がはるかに良く見える。だがこれは比較として成り立たない。
形式の監査の item は 7 月に作られ、一括破棄の前にほとんど閉じられていたからである。
破棄は、その時点で未解決だった item にしか当たらない。

形式の監査の ledger（overwatch の `audit_rounds.jsonl`）では、2026-07-10〜07-22 の 4 ラウンドで
新規 finding 28 件、CONFIRMED 23 件、回帰テスト追加 18 件だった。最後のラウンドは 07-22 で、
それ以降は回っていない。backlog の P2 の洪水を起こしたのは、場当たりの監査の方だった。

---

## 8. 残る穴

- **テストを誰が書いたかは、機械では強制していない。** 原則（別 context の別 agent）は triage skill の指示にあり、
  `backlog add` と `confirm` が見るのは「テストが挙動として落ちるか」だけである
- **token の実額は測れていない。** 1 finding あたりのコストは、notes の文字数という代用指標しかない
- **破棄した 461 件に本物がどれだけ含まれていたかは、判定できない。** 人間の裁定で閉じたものだからである

---

## まとめ

- 修正を観測できた欠陥のほとんど（78 件中 75 件）は、GAN（監査）が見つけた。しかし出力の多くは、証拠がなく小さい
- それを検証せずにタスクにすると、修正より登録の方が速くなり、backlog はまとめて捨てない限り減らない
- 自動ループは新しい finding を深さ優先で追い、ゴールを見失う
- 対策は欲を抑えることではない。**finding を出す側に証拠のコストを払わせる**ことである
- GAN の結果はすべて記録する。そのうえで、別 agent が書いた再現テストで RED が観測できたものだけを課題にする
- そして、その設計自体もテストで検証する。この記事を書く途中で、GAN 本体の経路が未移行だったことが見つかった

---

## 付録: 測定の再現

repo root で実行する。store は git 管理下なので、測定点 `9c7a54d5` を checkout すれば同じ数字が出る。

```python
import tomllib, collections as C, datetime as D

rows = []
for f in [".backlog/tasks.toml", ".backlog/tasks.done.toml"]:
    rows += tomllib.load(open(f, "rb")).get("task", [])

def pri(t):
    return next((p for p in ["p0", "p1", "p2", "p3"] if p in t["tags"]), "none")

def closure(t):
    return (t.get("closure") or {}).get("reason")

def f2p(t):
    return bool((t.get("closure") or {}).get("red"))

def month(ts):
    return D.datetime.fromtimestamp(ts).strftime("%Y-%m")

for p in ["p0", "p1", "p2", "p3"]:
    rs = [t for t in rows if pri(t) == p]
    print(p, len(rs), sum(map(f2p, rs)), sum(closure(t) == "discard" for t in rs))

o = 0
cr = C.Counter(month(t["created_at"]) for t in rows)
dn = C.Counter(month(t["updated_at"]) for t in rows if t["status"] == "done")
ca = C.Counter(month(t["updated_at"]) for t in rows if t["status"] == "cancelled")
for m in sorted(cr):
    o += cr[m] - dn[m] - ca[m]
    print(m, cr[m], dn[m], ca[m], o)
```

closure の分類は `backlog audit-closures` でも確認できる。

<!-- /record -->
