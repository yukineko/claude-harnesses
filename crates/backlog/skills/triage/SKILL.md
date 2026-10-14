---
name: triage
description: unconfirmed（再現テスト無しで起票された所見）を棚卸しする。証拠がある item は独立 agent に RED テストを書かせて `backlog confirm` で pending へ昇格し、証拠が無い・再現しない item は `backlog cancel` で捨てる。
argument-hint: [絞り込み: p1 / p2 / tag:<tag> / <id>...]
allowed-tools: Bash(backlog:*), Bash(git:*), Bash(cargo:*), Bash(python3:*), Read, Agent
---

# /backlog:triage — unconfirmed の棚卸し（証拠 → テスト → pending ／ 証拠なし → cancel）

`unconfirmed` は「再現テスト無しで起票された所見」であり、作業キュー（`next --claim`）には
乗らない。放置すると件数だけが積み上がり、**実在する欠陥と推測が同じ棚に並んだまま**になる。
この skill はそれを 2 方向に決着させる:

- **証拠がある** → テストを用意して **pending** へ昇格する（`backlog confirm --repro-test`）。
- **証拠が無い／再現しない** → **cancel** する（`backlog cancel --reason`。`discard` closure として理由が残る）。

**「証拠」とは観測である**（CLAUDE.md 第2節）。所見の notes に書かれた推測・疑い・「たぶん」は
証拠ではない。証拠が最終的に認められるのは **書いたテストが HEAD で behaviourally に落ちたとき**だけで、
notes の記述が具体的であっても、テストが通った（再現しない）なら cancel 側である。

## 判定（item ごとに 4 値。どれか 1 つ）

| verdict | 条件 | sink |
|---|---|---|
| **REPRODUCED** | notes に具体的な観測（逐語の出力・exit code・具体入力→誤出力を伴う file:line・rev 付きの失敗コマンド）があり、それを忠実に突くテストが **HEAD で behaviourally FAIL** した | テストを commit → `backlog confirm <id> --repro-test "<cmd>"`（`reproduced` なら pending） |
| **NOT-REPRODUCED** | 具体的な主張を忠実に突くテストを書いたが **HEAD で PASS** した（修正済み／主張が誤り） | `backlog cancel <id> --reason "not reproduced at <rev>: <何を走らせ何が出たか>"`。通るだけのテストは commit しない |
| **NO-EVIDENCE** | notes が推測だけ（suspect / probably / 具体入力なし）、または今は再現手段の残らない一過性の事象 | `backlog cancel <id> --reason "no evidence: <決め手になった notes の逐語>"` |
| **UNTESTABLE** | 具体的な主張はあるが、失敗するテストが書けない（環境で観測できない・期待挙動そのものが未決の設計判断 等） | `backlog cancel <id> --reason "untestable: <なぜ失敗するテストが書けないか>"`。**報告で列挙**し、人間が期待挙動を決めたら RED テスト付きで起票し直せるようにする |

> **unconfirmed は残さない**（2026-10-15 ユーザー裁定「失敗するテストがかけないなら、それはtaskじゃない」
> 「unconfirmedは基本的に許容できない。証拠がないで終わらずないならcancelしてなくす。再度発見するなら証拠をもってきてpendingにせよ」）。
> triage の終わりには、対象の全 item が pending か cancelled のどちらかになっている。
> **cancel は「やらないと決めた」記録であって「直った」の主張ではない。** NOT-REPRODUCED を
> `done` にしてはいけない（RED を観測していないので F→P 証拠にならない）。
> **UNTESTABLE を pending に丸めない**（テストの無い pending は task ではない）。cancel するが、
> 測れないという事実と理由は cancel reason と報告に必ず残す — 閉じることは隠すことではない。
> `ruling request --kind judgment` で unconfirmed のまま人間の承認待ちに置くのも同じ理由で使わない。

## 手順

1. **対象を列挙する**（read のみ。main からでもよい）:
   ```bash
   backlog list --all --json | python3 -c 'import json,sys; [print(t["id"],t["title"]) for t in json.load(sys.stdin) if t["status"]=="unconfirmed" and "p1" in t["tags"]]'
   ```
   既に `repro` 記録を持つ item（過去の confirm / add --repro-test が `undetermined` だったもの）は、
   その `detail`（例: runner allowlist 外）を読み、allowlist に乗るテストへ書き直す対象として扱う。

2. **テストは利害の無い agent が書く**（CLAUDE.md 第2節 (a)）。item を crate／スクリプト領域ごとに
   束ね（同じ crate の version bump が並行 agent 間で衝突しないように）、領域ごとに 1 体の
   general-purpose agent を起動する。各 agent への指示に必ず含めること:
   - 自分の worktree（`git worktree add …/triage-<group> -b triage-<group> main`）だけで作業し、merge / push しない。
   - item ごとに上表の 4 値で判定し、**逐語の根拠**を添えて報告する。
   - commit する RED テストは**通常スイートを赤くしない**形にする（この repo の既存規約）:
     - Rust: `crates/<c>/tests/backlog_<id>.rs` に `#[ignore = "backlog <id>: RED repro"]`。
       repro コマンドは `cargo test -p <c> --test backlog_<id> -- --ignored`。
     - Python: `scripts/test_backlog_<id>.py` に `@unittest.expectedFailure`。
       repro コマンドは `python3 -m pytest --runxfail scripts/test_backlog_<id>.py`。
     - repro コマンドが behaviourally 非 0、素のコマンドが 0 の**両方**を観測すること。
   - 既存スイートの失敗ノードが所見の主張そのものを突いているなら、それを repro にしてよい
     （新規ファイル不要）。ただし**現在の失敗原因が所見の主張と一致すること**を確かめる。
   - crate を触る commit は lockstep の micro bump（Cargo.toml / plugin.json / marketplace.json。
     harness-core の tests/ は免除）。`--no-verify` 禁止。ゲートに block されたら逐語で報告し迂回しない。
   - `backlog confirm/cancel/add/done/fail` は**打たない**（store への書き込みは driver が一括で行う）。

3. **統合する**（driver。cwd = セッションの linked worktree。CLAUDE.md 第8節）:
   各 `triage-<group>` branch をセッション worktree に merge する（REPRODUCED のテストを HEAD に載せる。
   `backlog confirm` は cwd の HEAD でテストを実行するため、merge 前に打っても再現しない）。

4. **sink を当てる**（同じ worktree で）:
   ```bash
   backlog confirm <id> --repro-test "<cmd>"        # REPRODUCED。非 0 終了＝reproduced にならなかった
   backlog cancel  <id> --reason "<理由>"            # NOT-REPRODUCED / NO-EVIDENCE / UNTESTABLE
   ```
   - `confirm` が `reproduced` を返さなかった（not-reproduced / undetermined）ら、その出力を逐語で記録し、
     agent の判定と食い違った事実として報告する（agent の自己申告より実測が優先）。原因が runner や
     テストの書き方（allowlist 外・未 commit 等）なら直して confirm し直す。直しても reproduced に
     ならなければ、実測の出力を reason に書いて cancel する（RED が観測できないものは task ではない）。

5. **store を commit し、main へ統合する**: `.backlog/tasks.toml` とテスト・bump を含む branch を
   main へ fast-forward して push し、crate を触っていれば `scripts/rollout-plugins.sh --canary`。
   使い終えた `triage-<group>` worktree / branch は merge 済みを確認してから削除する。

6. **報告**: verdict ごとの件数と id、confirm の実測結果、cancel 理由。UNTESTABLE で cancel した item は
   「期待挙動を決めれば RED テスト付きで再起票できる候補」として別に列挙する。

## やってはいけないこと

- **証拠の有無を notes の雰囲気で決めて cancel する**（「古そう」「たぶん直った」）。
  NOT-REPRODUCED は**テストを走らせて通った**ときだけ。走らせていないなら NO-EVIDENCE の条件
  （notes が推測だけ）を満たすかを逐語で示す。
- **confirm を通すためにテストを主張から逸らす**（proxy を落とす）。テストは notes の主張する性質を突く。
- **テストの無い item を pending にする／unconfirmed のまま残す。**
- **UNTESTABLE の cancel を報告から落とす**（理由を書かずに閉じると、測れないという事実が消える）。
