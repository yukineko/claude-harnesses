---
description: 今セッションを Obsidian の record ノートに記録する。数値（コスト/トークン/ターン）は session-insights が自動充填し、散文（完了サマリ/認知の変化/学び/振り返り/自己批判・確信度/残課題/関連）をあなたが埋める。成果物の複製ではなく認知の記録。AEGIS の /record 相当。
---

You are running the **session-insights `/record`** command: write a human-readable
session record note to the user's Obsidian vault. The deterministic numbers
(cost, tokens, turns, files, context) are produced by the bundled binary; YOUR
job is to author the Japanese prose sections based on THIS conversation.

## Steps

1. **(Re)generate the deterministic note and get its path.** Run:
   ```
   ${CLAUDE_PLUGIN_ROOT}/bin/session-insights record-now
   ```
   (If `CLAUDE_PLUGIN_ROOT` is unset, fall back to `bin/session-insights record-now`
   from the plugin dir.) It resolves the current session from
   `$CLAUDE_CODE_SESSION_ID`, refreshes the `## コスト` / `## 数値サマリ` blocks,
   and prints the **absolute path** of the note. Capture that path.
   - If it prints nothing and writes `no record note written (Obsidian vault not
     found: …)` to stderr, the vault directory does not exist. Tell the user the
     resolved `obsidian_vault` path and that they must create it (or set
     `obsidian_vault` / enable `record` in `session-insights.toml`), then stop.

2. **Read the note** at the printed path.

3. **Fill the prose.** Replace each `<!-- fill: <section> -->` placeholder with
   concise Japanese prose grounded in this session. Mirror the AEGIS record
   intent per section:
   - `## 完了サマリ` — 2–4 行で、何を達成したか（変更したファイル/コミット/動いた機能）。
     **ID の羅列は禁止**。個別 ID は backlog にあるので、ここではポインタに畳む
     （例:「正本 `422d256a`、個別 ID は backlog 参照」）。
   - `## 認知の変化 / 修正された理解` — **このセッションで自分の理解がどう変わったか**を
     順序どおりに書く。型は「初期モデル → 修正1 → 修正2 → …」で、**各修正に
     『その修正が塞いだ失敗モード』を必ず添える**（例:「修正1 →『止まるのはタスク完了時のみ』
     ← 無限ループ化を防ぐ境界」）。最終形だけを書かない — 最終形だけなら仕様であって
     認知の記録ではなく、次に同じ誤りを踏む人の役に立たない。理解が変わらなかった回は
     「変化なし」と正直に書く（無理に物語を作らない）。
   - `## つまずき / 学び` — 詰まった点と、その解決から得た学び。**道具の話で止めない**:
     つまずきの多くは道具ではなく認知の側にある（命題をリテラルに取りすぎた、前提を
     確認せず進めた等）。道具の学びは軽微として最後にまとめる。
   - `## 振り返り / 確立した方針` — 今後に効く方針・判断基準（**省略しない**）。
   - `## 自己批判 / 確信度` — **最も確信の低かった判断はどれか**、それを何で
     代用したか（勘・推測・未検証の仮定）、**次はどう測るか**。省略した検証
     （テストを書かなかった・別 agent に回さなかった等）があればその妥当性も。
     ここは「うまくいった話」を書く場所ではない。何も自己批判が無い回は、
     それ自体が確信度の較正ミスを疑う材料なので、そう書く。
   - `## 注意点 / 落とし穴` — 触ると壊れる箇所・前提・ハマりどころなど、次に同じ領域を触る人が**事前に知っておくべき注意**（「方針」ではなく「気をつけろ」の粒度）。
   - `## 残課題` — 未完了・先送り・次にやること。
   - `## 要追跡 / あとで確認` — 今は判断できず**後日見直すべき項目**（仮の選択・要観察の挙動・効果測定待ち・期限付きの再確認など）。可能なら確認の起点（条件/日付）も添える。
   - `## 関連` — 関連ノートへの `[[リンク]]`（あれば）。
   If a section genuinely has nothing, write a short honest line (e.g. `特になし`)
   rather than leaving the placeholder.

   **Distill via a fresh Sonnet subagent — never switch the main model.** Spawn a
   subagent with `model: "sonnet"` and have it return just the per-section bullet
   points. Pass the transcript as a **file path**, not pasted text, so the
   subagent's fresh context only loads what it reads:

   ```
   Agent(
     subagent_type: "general-purpose",   // or ctxrot:ctxrot-distiller
     model: "sonnet",
     prompt: "Read this session transcript and return ONLY the per-section
              bullet points for a record note: 完了サマリ / 認知の変化・修正された理解 /
              つまずき・学び / 振り返り・確立した方針 / 自己批判・確信度 /
              注意点・落とし穴 / 残課題 / 要追跡・あとで確認 / 関連.
              Do NOT duplicate what backlog/code/commits already hold: fold ids
              and path:line evidence into pointers, and spend the note on the
              reasoning, the corrected understanding, and the confidence
              calibration that exist nowhere else. Do not implement anything.
              Transcript: <abs path to $CLAUDE_CODE_SESSION_ID.jsonl under
              ~/.claude/projects/<slug>/>"
   )
   ```

   Resolve the transcript path from `$CLAUDE_CODE_SESSION_ID` (the `.jsonl` lives
   under `~/.claude/projects/<cwd-slug>/`). The subagent starts with an empty
   context, so even a long Opus session distills under Sonnet's window.

   **Do NOT distill by switching the live session to Sonnet** (e.g. `/model
   sonnet`). If this is an Opus 1M-context session whose live context already
   exceeds Sonnet's 200k window, switching mid-session overflows and errors
   ("prompt is too long"). Keep the main loop on Opus; only the subagent runs
   Sonnet. (Same spirit as the `/distill` skill.)

4. **Save** the note back to the same path.

5. **Reconcile the cross-session backlog.** The per-session `## 残課題` is a
   snapshot; the durable, cross-project queue is the **standalone `backlog`
   crate** (`~/.backlog/tasks.toml`, surfaced at SessionStart by its own hook).
   This is now the single canonical queue — session-insights no longer keeps an
   independent backlog. Use the project name from the record note heading as `<P>`.
   1. **See what's pending:** `backlog list --project <P> --status pending`
   2. **Close what this session finished.** For each pending item that is now
      done (compare against what was actually accomplished this session), mark it
      by id: `backlog done <id>`.
   3. **Add genuinely-open follow-ups** from this session's `## 残課題`:
      `backlog add --title "<one open item>" --project <P>` (one call per item).
      Do NOT add items you just closed, or transient notes — only durable
      "still needs doing" work.
   Keep backlog item titles short and action-oriented ("X が必要", "Y を直す") so the
   SessionStart injection reads as a clear to-do list.

## Hard rules

- **成果物を複製しない。** backlog / code / commit から読み直せるものは、この
  ノートの仕事ではない。record が書くのは**そこに書けないもの** — 推論の経路、
  修正された理解、確信度の較正。ID と evidence（`path:line`）は列挙せず
  ポインタに畳む。この規範を破った実例と、それを書き直した版が参照見本として
  残っている（下記）。
- **参照見本（before/after）**: ユーザーの Obsidian vault の
  `records/2026-07-24-harness-d52fbfce.md`（wikilink: `[[2026-07-24-harness-d52fbfce]]`）。
  旧版は完了サマリ・残課題・関連が 11 個の ID と evidence を並べ直しただけで
  backlog notes と重複していた。書き直した現行版は、そのセッション最大の価値
  （ユーザーによる停止契約の 3 手の認知修正弧）を「初期モデル → 修正1/2/3 →
  各修正が塞いだ失敗モード」として物語化し、つまずきを道具ではなく認知の側に置き、
  ID 列をポインタに畳んでいる。**新しい章立てに迷ったらこのノートを読む。**
  （このノートは vault 内にあり repo には含まれない。見つからない場合はユーザーに尋ねる。）
- **Never edit anything between** `<!-- si:numeric:start -->` … `<!-- si:numeric:end -->`
  **or** `<!-- si:cost:start -->` … `<!-- si:cost:end -->`. Those blocks are
  machine-owned and will be overwritten on the next `record-now` / SessionEnd.
  Only replace the `<!-- fill: … -->` lines and the prose around them.
- **Do not add YAML frontmatter** — this note format is intentionally
  frontmatter-free (matching the user's hand-written AEGIS session notes).
- Keep the existing section order and headings; don't restructure the note.
- If `$ARGUMENTS` is given, treat it as a focus hint for the prose (e.g. which
  thread of the session to emphasize), not as a new filename.
- **Never switch the main session model** to do the distillation. Distill only
  via a Sonnet subagent (step 3); the main loop stays on whatever model the
  session started with.
