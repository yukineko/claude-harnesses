# parallelguard

1 セッションの同時実行数を、散文ではなく **binary が数えて強制する**ゲート。

WSL2 のフリーズは「同時に走るプロセスが多すぎる」だけで起きる。このハーネスの
fan-out（shard ごとに 1 体ずつ auditor を起動する skill、1 メッセージに並べた
`Bash` の束）はすべて同じ枠を食うが、これまでその上限は SKILL.md の
「3 体ずつの波に分けてください」という**お願い**しか無かった。parallelguard は
**実際に in-flight な数**を数え、上限を超える呼び出しを deny する。

## 何を上限にするか

独立した 2 つのプール。既定はそれぞれ **3**:

| プール | ツール | 上限 |
|---|---|---|
| shell | `Bash` | 同時 3 |
| subagent | `Task` / `Agent` | 同時 3 |

**共有プール 1 本にしないのは deadlock するから**: subagent は自分の生存期間ずっと
slot を持つので、3 体が live だと *その subagent 自身の* `Bash` が全部 deny される。
slot を持っている側が前に進めない＝永遠に返さない、という循環になる。

`HARNESS_MAX_PARALLEL` で**下げられる**が、上げられない。3 は既定値ではなく
**天井**であり、範囲外・パース不能な値は天井に解決する。

## 配線

```
PreToolUse(Bash|Task|Agent)          -> parallelguard acquire   slot を取る / deny する
PostToolUse(Bash|Task|Agent)         -> parallelguard release   slot を返す
SessionStart|UserPromptSubmit|Stop   -> parallelguard reset      台帳を消す
```

台帳はセッションごとに 1 ファイル（`$HOME/.parallelguard/state/sessions/`、
絶対パスの `PARALLELGUARD_STATE_DIR` で変更可）。並列 tool バッチの hook プロセスは
**同時に走る**ので、advisory lockfile で read-modify-write を直列化する
（この lock を外すと 8 並列が 8 件とも通ることを実測済み — `tests/concurrency.rs`）。

## 判定不能は deny

payload が壊れている / 台帳が読めない / lock が取れない / 書き込みが失敗する /
binary が panic する / そのプラットフォームのビルドが無い — どれも
**in-flight 数が不明**であり、不明は「空き」ではない。すべて deny に倒す
（CLAUDE.md 第3節）。**沈黙は degrade として使えない**: PreToolUse hook の
「出力なしで exit 0」は allow そのもので、「ゲートが壊れた」と「ゲートが空きを
見つけた」が下流から 1 バイトも区別できなくなる。

代わりに、**すべての deny が人手なしで回復可能**であることを設計で担保する:

* 台帳は毎ターン境界の `reset` で消える
* 殺されたプロセスが残した lockfile は 30 秒で steal される
* deny された呼び出しは「走らなかった」だけ。再発行のコストは 1 ラウンドで、作業は失われない

## 上限が効かない範囲（明示）

* `run_in_background: true` の `Bash` は即座に返るので、プロセスが走ったまま
  `PostToolUse` が発火して slot が早く返る。バックグラウンド shell は**数の外**。
* slot は**時間で失効しない**。長い呼び出しは終わるかターンが終わるまで slot を持つ
  （「古そうだから死んだことにする」は permissive な推測であり、長く掛かるだけで
  上限を超えられる抜け道になる）。

## 運用コマンド

```sh
parallelguard status   # 実効の上限・セッションごとの count・直近の deny
```

`status` は「何も走っていない」と「この hook は一度も動いていない」を**区別して**
表示する（空の台帳では両者が同じ見た目になるため）。
