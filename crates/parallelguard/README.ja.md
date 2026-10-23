Claude Code の tool 呼び出しの同時実行数を、散文ではなく **binary が数える**上限。

parallelguard は `acquire` を通過してまだ `release` されていない呼び出しを数え、上限を超える呼び出しを deny する。

独立した 2 つのプールで、上限はそれぞれ **3**:

| プール | ツール | 上限 |
|---|---|---|
| shell | `Bash` | 同時 3 |
| subagent | `Task` / `Agent` | 同時 3 |

2 つのプールを共有プール 1 本にまとめない: 2 プールなら shell が満杯でも subagent は止まらない。

上限は上げられず、3 は **天井**であり、`HARNESS_MAX_PARALLEL` の
大きすぎる値・パース不能な値は天井に解決する。
`HARNESS_MAX_PARALLEL` で**下げる**ことはできる。
`0` は天井ではなく下限の 1 に引き上げられる。

```
PreToolUse(Bash|Task|Agent)          -> parallelguard acquire   slot を取る / deny する
PostToolUse(Bash|Task|Agent)         -> parallelguard release   slot を返す
SessionStart|UserPromptSubmit|Stop   -> parallelguard reset      台帳を消す
```

台帳はセッションごとに 1 つの JSON ファイル（`$HOME/.parallelguard/state/sessions/`、
絶対パスの `PARALLELGUARD_STATE_DIR` で変更可）。
並列 tool バッチの hook プロセスは同時に走るので、advisory lockfile で
read-modify-write を直列化し、上限を超えて通すことはない。

**沈黙は degrade として使えない**: 下の経路はどれも出力なしの exit 0 で終わらない。
payload が壊れている / 台帳が読めない / lock が取れない / 書き込みが失敗する /
そのプラットフォームのビルドが無い — どれも **in-flight 数が不明**であり、不明は「空き」ではないので、
すべて deny に倒す（CLAUDE.md 第3節）。

* 台帳は毎ターン境界の `reset` で消える
* 殺された hook が lock を塞ぎ続けることはない: lock は永続 lockfile 上の kernel advisory lock（`flock`）で、保持者が死ねば kernel が解放し、鮮度タイムアウトも steal も無い

* slot は**時間で失効しない**。
* `run_in_background: true` の `Bash` は特別扱いされない: 他の `Bash` と同じく `acquire` で slot を取り `release` で返すので、`release` 後も走り続けるバックグラウンドプロセスは数の外。

```sh
parallelguard status       # 実効の上限・セッションごとの count・直近の deny
parallelguard --protects   # PROTECTS / AGAINST / GROUNDS の保護宣言を表示
```

`status` は、セッションの台帳がある間は（何も走っていなくても）プールごとの count を表示し、
台帳が無いときは「the gate is NOT running」の行を表示する。
ターン境界の `reset` は台帳を消すので、`reset` 直後の `status` は、ゲートが動いていても同じ「NOT running」の行を表示する。
