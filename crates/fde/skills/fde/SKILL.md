---
name: fde
description: Forward Deployed Engineer ワークフロー。外部の target リポジトリを読んで仕様を推定し(証拠は逐語引用)、人間の批准を AskUserQuestion で得てから、要件ごとに RED→fix→GREEN を証跡付きで回し、未解決・未計測を逐語で報告する。批准前の target 変更は fde gate(PreToolUse)が拒否する。判定は `fde evidence verify` の終了コードだけ。
argument-hint: '[target-dir]'
allowed-tools: Task, Bash, Read, Write, Edit, Grep, Glob, AskUserQuestion
---

# /fde — Forward Deployed Engineer

```
/fde [target-dir]
```

外部の target リポジトリに入り、**仕様を推定 → 人間が批准 → 要件ごとに RED → fix → GREEN → 報告**を回す。
判断(読解・仕様推定・テスト記述・実装)はこの skill と委譲先 agent、決定論(target 解決・批准の記録・
批准前変更の拒否・証跡の判定)は `fde` バイナリが担う。終了コードの全表は `crates/fde/README.ja.md` を参照。

## target の決め方

- `target-dir` は省略可。**ユーザーの引数をそのまま(省略ならそのまま省略して)現在の cwd から**
  各 `fde` サブコマンドの `[TARGET]` に渡す。自分でパスを組み立て直さない。
- 省略時の target は「呼び出し cwd を含む git top-level」(`fde resolve` が上へ辿って決める)。
  相対パスは呼び出し cwd 基準で解決される。git work tree の外・判定不能なら `fde resolve` は exit 2 で拒否する。
- 最初に `fde resolve [TARGET]` を実行し、出力 JSON の `root` と `state_key` を控える。exit 2 なら止めて人間に聞く。
- 状態と証跡は解決済み root で key 付けされる(同じ repo のどの下位ディレクトリから呼んでも同じ key)。
  置き場は state dir = 環境変数 `FDE_STATE_DIR`(未設定・空なら `~/.fde`)。target の中には書かない。

## 不変条件(外さない)

1. **批准前に target を変更しない。** `fde gate`(PreToolUse hook、matcher `Bash|Edit|Write|MultiEdit|NotebookEdit`)が、
   `fde begin` 済みで未批准の target への編集を拒否する。判定不能も拒否。拒否されたら**別経路で同じことをしない**
   (相対パスで target を名指す・別ディレクトリから回り込む等は禁止)。拒否理由を逐語で人間に返す。
2. **批准は人間だけが与える。** 自分で答えない(自律モード・`condukt policy answer` でも escalate 扱い)。
3. **exit 2 は常に「判定不能」**(gate では deny)。pass 扱いにしない。止めて人間に聞く。
4. **完了の判定は `fde evidence verify` の終了コードだけ。** `fde report` の終了コードは判定ではない。
5. テストを書く agent と実装する agent は別にする。

## (a) 把握 — 仕様を推定する

1. target のコード・README・テストを Read / Grep / Glob で読む(この段階では何も変更しない)。
2. 要件素材(markdown)を **state dir 側**に書く。例: `<state_dir>/specforge/<state_key>/req.md`。
   各要件の根拠には **target の `file:line` と逐語引用**を付ける。引用できない推測は推測と明記する。
3. 下書き spec を **`specforge draft` だけ**で作る(specforge の他のサブコマンドは使わない —
   **`specforge ratify` / `specforge queue` は使わない**。批准は fde が持つ)。
   - 同じ state dir 側のディレクトリに `specforge.toml` を置き、`[project] root = "."`(= そのディレクトリ)にする。
     これで spec_dir・sentinel・作業ファイルはすべて state dir 側に出る。`[agent]` は read-only の normalize agent にする
     (書式は `crates/specguard/specforge.example.toml`)。
   - 実行: `specforge --config <dir>/specforge.toml draft --id <spec-id> --req <dir>/req.md`
   - 出力: `<dir>/specs/<spec-id>.toml`(specforge Spec IR TOML: `[spec] id`、`[[requirement]]` の `id` / `statement` / `acceptance`)。
   - specforge が非 0 で終わった・spec ファイルが出ない・sentinel(`.specforge-pending`)が立った場合は、
     止めて人間に聞く(rigor 未達は人間が埋める)。
   - 実証の範囲: `crates/fde/tests/spike_specforge.rs` は stub agent での `specforge draft` が target を変えないことだけを示す。
     実 LLM agent の挙動は証明していない。
4. 登録: `fde begin [TARGET] --spec <dir>/specs/<spec-id>.toml`(spec は target の**外**であること。中なら exit 1)。
   これで target は「登録済み・未批准」になり、gate が target への変更を拒否し始める。
   **観測済みの挙動**: gate が有効なとき、未批准の間は cwd が target 内の Bash も、target の root パスを文字列で含む Bash も
   拒否される(`fde ratify` / `fde status` / `fde report` 自身も含む)。target を読むのは Read / Grep / Glob で行う。

## (b) 批准 — 人間に確認する

1. 推定 spec の要件ごとに `statement`・`acceptance`・根拠の逐語引用(`file:line`)を並べ、
   `AskUserQuestion` で「この仕様で批准するか / どこを直すか」を聞く。**自分で答えない。**
2. 人間が修正を求めたら spec を直し、もう一度聞く。spec ファイルを 1 バイトでも変えると批准は外れる
   (fingerprint = 内容の SHA-256)。批准後に直した場合も同じく再確認が要る。
3. **人間の明示的な承認があったときだけ** `fde ratify [TARGET]` を実行する。
   gate に拒否されたら(上の観測どおり、gate 有効時は拒否される)迂回せず、人間に「自分の端末で
   `fde ratify [TARGET]` を実行してください」と同じコマンドを示して依頼する。
4. 先へ進む条件は `fde status [TARGET]` が **exit 0**。1(未批准・spec 変更あり)なら (b) に戻る。
   2 は止めて人間に聞く。

## (c) RED — 要件ごとに失敗するテストを作る

各 `[[requirement]]` について:

1. **テスト agent**(Task で起動、実装はさせない)に、`acceptance` を突くテストを target 内に書かせる。
2. テストコマンド `CMD` を 1 つ決め、以後この要件では同じ文字列を使う。
3. `fde evidence red --req <REQ-ID> --cmd '<CMD>' [TARGET]` — fde が target root で `CMD` を実行し、
   **終了ステータスが非 0 のときだけ** RED を記録する(0 なら exit 1 で拒否)。
4. tdd の RED / F→P オラクルも同じ `CMD` で残す:
   `tdd red --task <spec-id>-<REQ-ID> --cmd '<CMD>' --author <test-agent-id>`
   (tdd の証跡は tdd の `proof_dir`、既定 `.tdd` に書かれる)。tdd は `CMD` を自分の cwd で実行し、
   fde は target root で実行する。両者で同じ結果になるよう、`CMD` は実行場所に依存しない形にする。
5. テストが最初から通る(=既に適合している)要件は RED にせず、
   `fde evidence pass --req <REQ-ID> --cmd '<CMD>' [TARGET]` で「適合済み」として記録する。
   RED を記録した要件に PASS は付けられない(exit 1)。

## (d) fix — 実装を委譲する

1. 実装は `condukt` / `condukt-loop` に委譲し、target の worktree で行う。**テスト agent とは別の agent** にする。
2. fde は `CMD` を **target root** で実行するので、修正を target root の作業ツリーに統合してから記録する。
3. `fde evidence green --req <REQ-ID> --cmd '<CMD>' [TARGET]` — RED と**同じ `CMD`**。同じ要件・同じ `CMD` の RED が無ければ exit 1。
4. tdd 側: `tdd green --task <spec-id>-<REQ-ID> --cmd '<CMD>' --author <impl-agent-id> --strict-separation`、
   続けて `tdd oracle --task <spec-id>-<REQ-ID>`(exit 0 = 有効な Fail→Pass。1 は不成立、2 は読めない=止めて聞く)。

## (e) 報告

1. **判定**: `fde evidence verify [TARGET]` — 0 = 全要件完了 / 1 = 欠けあり(欠けた要件は stderr)/ 2 = 判定不能。
   完了と言えるのは exit 0 のときだけ。
2. **一覧**: `fde report [TARGET]`(要件ごとの状態、unresolved / unmeasured の節)。
   **`fde report` は未批准の target でも exit 0** で `ratification: UNRATIFIED` / `evidence: not evaluated` を出す。
   したがって report の終了コードは判定ではなく、判定として使ってはならない。
3. 最終報告には次を**逐語で**載せる:
   - unresolved(RED はあるが GREEN が無い)要件
   - unmeasured(証跡なし)要件
   - `fde evidence verify` の終了コードと stderr
   - やっていないこと・確かめていないこと(例: 実行しなかった検査、gate に止められて進めなかった手順)

## 終了コードの読み方(要約)

- `resolve` / `report`: 0 / 2。`begin` / `ratify` / `evidence red|green|pass`: 0 記録 / 1 拒否 / 2 判定不能。
- `status`: 0 批准済み / 1 未批准 / 2 判定不能。`evidence verify`: 0 完了 / 1 欠け / 2 判定不能。
- `gate`: 0 allow / 2 deny。引数エラー(clap)も 2。どれも 2 は pass ではない。
