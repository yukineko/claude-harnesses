# taintguard

**provenance（出所）に基づく最小権限ゲート** — このターンが信頼できない出所の
content（WebFetch/WebSearch の結果、プロジェクト外の `Read`）を取り込んだら、
書き込み系ツールをそのターンの間だけ read-only 相当に格下げする Claude Code
hook トリオ。

## 目的

プロンプトインジェクション対策の一つの層として、「このターンで外部由来の
content を読んだ直後は、書き込み系操作を無条件には許可しない」という
least-privilege を機械的に強制する。エージェントが騙されて悪意ある指示を
外部 content から取り込んでいても、それが即座に `Bash`/`Write`/`Edit` 等の
実行に直結しないようにする。

## 3 つの hook

| hook | event | matcher | 役割 |
|---|---|---|---|
| `taintguard mark`  | PostToolUse | `WebFetch\|WebSearch\|Read` | 出所を判定し taint marker を記録 |
| `taintguard gate`  | PreToolUse  | `Bash\|Write\|Edit\|MultiEdit\|NotebookEdit` | tainted なら ask/deny |
| `taintguard clear` | Stop        | (全体)                        | クリーンなターン終了で marker を解除 |

### mark（PostToolUse）

- `WebFetch` / `WebSearch` の結果は常に信頼できない出所として扱う（source: `web`）。
- `Read` はターゲットパスをプロジェクトルート（`cwd`）基準で分類する
  （`src/classify.rs`）:
  - ルート配下に解決される → 信頼できる（no-op）。
  - ルート外（`/tmp`、ホームディレクトリ、他プロジェクト、`..` での脱出）→
    信頼できない（source: `external-read`）。
  - パスが判定不能（file_path が空/欠落、シンボリックリンクの解決不能等）→
    **fail-closed**（信頼できないものとして mark）。
- 解析自体が panic した場合も fail-closed（source: `internal-error` で
  強制的に mark）。「解析に失敗したので何もしない」は「解析して問題なしと判断した」
  と区別がつかず、それは fail-open になるため。

### gate（PreToolUse）

- このセッション（`session_id` + `cwd`）が tainted なら、blastguard と同じ
  `hookSpecificOutput.permissionDecision` 形状で `ask`（対話セッション）または
  `deny`（非対話）を返す。
- 対話判定は blastguard の `interactive::ask_available` と同一ロジック
  （`CLAUDECODE=1` かつ `CLAUDE_CODE_ENTRYPOINT=cli` のときだけ `ask`、それ以外は
  `deny` — 人間が実際に応答できない `ask` は、一時停止ではなくエージェントが
  解除できない block になるため）。
- taint marker が読めない/壊れている場合も **fail-closed**（`ask`/`deny` 側）。
  「判定できなかった」を「問題なし（silent allow）」に潰さない。
- tainted でなければ何も出力しない（silent allow）。

### clear（Stop）

- ターンがクリーンに終わったら marker を削除し、次のターンを信頼できる状態に
  戻す。
- 削除に失敗した場合は stderr に出すだけで exit 0（**tainted のまま残る方が
  安全側**なので、消せなかったことを許可の失敗として扱わない）。

## 状態の保存先

`$TAINTGUARD_STATE_DIR/<project_key>/<session>/taint.json`
（`TAINTGUARD_STATE_DIR` 未設定時は `~/.taintguard/state`）。
`project_key`/セッションディレクトリの命名規則は
`harness_core::store::context_state_dir` と同じ慣習（cwd を canonicalize
してから project key を作る）。

## fail-closed の設計

- `state::check` は 3 値（`Clean` / `Tainted(sources)` / `Undetermined(why)`）
  で、marker が読めない・パースできないケースを `Clean` に潰さない。
  `is_tainted` はこの 3 値のうち `Clean` 以外を全部 `true` にする bool 便利関数。
- `mark`/`gate` はどちらも `catch_unwind` によるパニックバリアで包まれており、
  内部で panic しても「何もしなかった（＝silent allow）」にはならず、
  fail-closed な結果（強制 mark / 強制 ask-deny）に解決する。
