---
name: fde
description: Forward Deployed Engineer ワークフロー。外部の target リポジトリを読んで仕様を推定し(証拠は逐語引用)、人間の批准を AskUserQuestion で得てから、要件ごとに RED→fix→GREEN を証跡付きで回し、未解決・未計測を逐語で報告する。批准前の target 変更は fde gate(PreToolUse)が拒否する。判定は `fde evidence verify`(全要件 RED→GREEN で 0。PASS のみは未完了)と `fde report`(批准済みかつ全要件 RED→GREEN のときだけ 0)の終了コード。
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
対象プラットフォームは Unix 系(WSL 含む)。Windows は対象外。

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
4. **完了の判定は終了コードで行う。** `fde evidence verify`(0 = 全要件が RED→GREEN。unresolved・unmeasured・
   PASS のみは 1。2026-11-04 のユーザー裁定: PASS のみの要件は完了ではない)と `fde report`(0 = 批准済みかつ全要件が
   RED→GREEN。未批准・unresolved・unmeasured・PASS のみは 1)。両者の違いは未批准の扱いだけ(verify は 2、report は 1)。
   「全要件を RED→GREEN で検証した」と言えるのはこれらが exit 0 のときだけ。
5. テストを書く agent と実装する agent は別にする。
6. **target の HEAD は批准時のコミットから前進させるだけ。** 批准は target の HEAD コミット(と dirty かどうか)を記録し、
   各証跡も CMD 実行直前の HEAD / dirty を記録する。批准後に target root で `git reset`・ブランチ切り替え・履歴の
   書き換えをすると、記録は「批准した履歴の上」に無くなり、`fde evidence red|green|pass|verify` と `fde report` は
   exit 2 になる。そうなったら止めて人間に聞く。人間が現在の HEAD で再批准(`fde ratify`)すると新しい証跡 epoch に
   なり、以前の記録は残るが無視される(`fde report` の `ignored:` 行に件数が出る)ので、全要件の RED / GREEN / PASS を
   記録し直す。前進したコミットで再批准した場合も以前の証跡は判定から外れる(2026-11-04 のユーザー裁定で現状維持)。
   同じ HEAD での再批准なら記録は数えられ続ける。旧版の fde が書いた `ratified_head` の無い記録が残っている場合は
   再批准しても 2 のままなので、人間に聞く。
   修正はコミットとして積む(merge / fast-forward / 新規コミット)。ただしコミットは **CMD の外**で行う:
   `fde evidence red|green|pass` は CMD の前後で HEAD を観測し、CMD の実行中に HEAD が変わる(後退・前進〈コミット〉・
   観測不能)と終了ステータスに関わらず exit 2 で何も記録しない(2026-11-04 のユーザー裁定。前進も拒否するのは、
   記録の HEAD が結果の属する tree を表さなくなり、CMD が自分で修正をコミットして来歴規則を満たせてしまうため)。

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
   gate が有効なとき、未批准の間は cwd が target 内の Bash も、target の root パスを文字列で含む Bash も、
   語を解決すると target(またはその worktree)に入る Bash(`../proj/x`、`rm -rf proj`、symlink 経由、
   `--out=proj/x`、`~/` で HOME が target など)も拒否される(target 内のテスト・ビルド・git コマンドも含む)。
   さらに未批准の target(または消えた target)がある間は、gate が解決できない語 — `$`・バッククォート・引用符の外のグロブ
   (`*` `?` `[`)・`{`・`~user`・閉じていない引用符 — を含む Bash は、target と無関係でも拒否される
   (判定不能は deny)。変数や置換を使わず、パスを字義どおりに書くこと。拒否を別構文で回避しない。
   target を読むのは Read / Grep / Glob で行う。
   **例外**: 未批准の間でも `fde status` / `fde report` / `fde begin` は Bash で実行してよい。ただし
   **ちょうど 1 回の単独呼び出し**に限る(`fde status [TARGET]` のように書く。`;` `&&` `||` `|` `&` 改行による連結、
   リダイレクト、`$(...)` 等の置換、引用符、環境変数の前置、`./fde` や絶対パス、`command` / `env` / `sh -c` 等の
   ラッパー、引数に `ratify` を含むものは拒否される)。**`fde ratify` は人間専用**で、未批准の間はモデルの実行を拒否する。
   **`fde forget` も人間専用**で、登録 target がある間は場所を問わずモデルの実行を拒否する(下の「復旧」)。
   target の状態が判定不能(status が 2)のときは、この例外も適用されず拒否される。

## (b) 批准 — 人間に確認する

1. 推定 spec の要件ごとに `statement`・`acceptance`・根拠の逐語引用(`file:line`)を並べ、
   `AskUserQuestion` で「この仕様で批准するか / どこを直すか」を聞く。**自分で答えない。**
2. 人間が修正を求めたら spec を直し、もう一度聞く。spec ファイルを 1 バイトでも変えると批准は外れる
   (fingerprint = 内容の SHA-256)。批准後に直した場合も同じく再確認が要る。
3. **`fde ratify` は自分で実行しない(人間専用)。** `AskUserQuestion` で人間が明示的に承認したら、
   人間自身に実行してもらう: 「Claude Code のプロンプトで `! fde ratify [TARGET]` と入力するか、
   自分の端末で `fde ratify [TARGET]` を実行してください」と同じコマンドを示して依頼する。
   モデルが実行しようとすると gate が拒否する。拒否を別経路で回避しない。
4. 先へ進む条件は `fde status [TARGET]` が **exit 0**。1(未批准・spec 変更あり)なら (b) に戻る。
   2 は止めて人間に聞く。
5. `fde ratify` は target に**コミットが 1 つ以上ある**ことを要する(HEAD を記録するため。コミットの無い repo では
   exit 2 で何も記録しない)。旧版の fde で批准した target(HEAD 記録なし)は `fde status` では批准済みのままだが、
   evidence / verify / report が exit 2 で再批准を求める。そのときは人間に再批准(`fde ratify`)を依頼する。

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
   RED を記録した要件に PASS は付けられない(exit 1)。**PASS だけでは要件は完了にならない**(2026-11-04 のユーザー
   裁定。verify も report も 1)。完了させるには、その要件を突いて失敗するテストで RED を取り、GREEN まで回す
   (PASS → RED → GREEN は完了)。

## (d) fix — 実装を委譲する

1. 実装は `condukt` / `condukt-loop` に委譲し、target の worktree で行う。**テスト agent とは別の agent** にする。
2. fde は `CMD` を **target root** で実行するので、修正を target root の作業ツリーに統合してから記録する。
   統合は**コミットしてから** GREEN を記録することを推奨する: 未コミットの変更がある(dirty な)tree で取った記録も
   数えられ、`fde report` に `dirty` と出るが、その状態はコミットからは再現できない。
3. `fde evidence green --req <REQ-ID> --cmd '<CMD>' [TARGET]` — RED と**同じ `CMD`**。同じ要件・同じ `CMD` の RED が無ければ exit 1。
4. tdd 側: `tdd green --task <spec-id>-<REQ-ID> --cmd '<CMD>' --author <impl-agent-id> --strict-separation`、
   続けて `tdd oracle --task <spec-id>-<REQ-ID>`(exit 0 = 有効な Fail→Pass。1 は不成立、2 は読めない=止めて聞く)。

## (e) 報告

1. **判定**: `fde evidence verify [TARGET]` — 0 = 全要件が RED→GREEN / 1 = 欠けあり(unresolved・unmeasured・
   PASS のみの要件を stderr に列挙)/ 2 = 判定不能。完了と言えるのは exit 0 のときだけ。
2. **一覧**: `fde report [TARGET]`(批准時 HEAD と clean/dirty、現在の HEAD、要件ごとの状態と各記録の HEAD・clean/dirty、
   unresolved / unmeasured の節)。
   一覧は未批准の target でも出る(`ratification: UNRATIFIED` / `evidence: not evaluated`)が、そのときの終了コードは 1。
   exit 0 は批准済みかつ全要件が RED→GREEN のときだけ。未批准(未登録・未批准・批准後の spec 変更)や、unresolved・
   unmeasured・PASS のみの要件があれば exit 1 で理由を stderr に出す(verify と同じく PASS のみは完了に数えない。
   違いは未批准の扱いだけ)。2 = 判定不能。
3. 最終報告には次を**逐語で**載せる:
   - unresolved(RED はあるが GREEN が無い)要件
   - unmeasured(証跡なし)要件
   - PASS のみ(RED→GREEN を経ていない)要件
   - `fde evidence verify` と `fde report` の終了コードと stderr
   - `dirty` と表示された記録があれば、その要件と HEAD(コミットから再現できない証跡であること)
   - やっていないこと・確かめていないこと(例: 実行しなかった検査、gate に止められて進めなかった手順)

## 終了コードの読み方(要約)

- `resolve`: 0 / 2。`begin` / `ratify` / `evidence red|green|pass`: 0 記録 / 1 拒否 / 2 判定不能
  (`ratify` は HEAD・dirty が判定できないとき、`evidence` は旧形式の批准、現在の HEAD が批准時 HEAD の子孫でないとき、
  CMD の実行中に HEAD が変わったときも 2)。
- `evidence verify` / `report` は、批准時 HEAD ≤ 各記録の HEAD ≤ 現在の HEAD(祖先または同一)が成り立たない・比較できないとき 2。
- `report`: 0 完了(批准済みかつ全要件 RED→GREEN)/ 1 未完了(未批准、または unresolved・unmeasured・PASS のみの要件あり)/ 2 判定不能。
- `status`: 0 批准済み / 1 未批准(target が消えた〈gone〉場合も 1)/ 2 判定不能。`evidence verify`: 0 完了(全要件 RED→GREEN)/ 1 欠け(PASS のみを含む)/ 2 判定不能。
- `forget`(**人間専用。モデルは実行しない**): 0 削除 / 1 未登録 / 2 判定不能。
- `gate`: 0 allow / 2 deny。引数エラー(clap)も 2。どれも 2 は pass ではない。

## 復旧: 登録済み target が消えた(gone)とき

- 登録済み target のディレクトリが削除されると、`fde status <root>` は exit 1 で stderr に `gone` と出し、
  gate はその元の場所(パス・cwd・root を含む Bash)を `gone` / `fde forget` を理由に拒否する。全語を解決できる
  無関係な作業は止めないが、gate が解決できない語(`$`・バッククォート・グロブ・`{`・`~user`・閉じていない引用符)
  を含む Bash は、元の場所を避けていると示せないので拒否し、理由に `! fde forget <root>` を出す(2026-11-04 の
  ユーザー裁定)。
- そのときは **`fde forget` を自分で実行しない**(人間専用。モデルが実行すると gate が拒否する。別経路で state dir を
  消すのも禁止)。人間に「Claude Code のプロンプトで `! fde forget <TARGET>` と入力するか、自分の端末で
  `fde forget <TARGET>` を実行してください」と、`fde status` が示した root を添えて依頼する。
- 壊れた登録で gate がすべてを拒否している場合も同じく、人間に `fde forget [TARGET]` を依頼する。
- 登録 root が git repo でなくなった(`.git` が消えた)場合も、人間に `fde forget <root>` を依頼すれば消せる
  (git が解決できないときは root の正規化パスで key を引く)。
