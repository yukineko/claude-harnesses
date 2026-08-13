# schemaguard 監査 — schema.rs / registry.rs の全 verdict 経路 (read-only)

測定点: このリポジトリの `crates/schemaguard/` は本監査コミット時点で HEAD の状態のまま
（本監査は `crates/schemaguard/` を1行も変更していない）。行番号はすべて実測（`Read` で確認済み）。

対象:
- `crates/schemaguard/src/schema.rs` の `validate()`（全84〜183行）
- `crates/schemaguard/src/registry.rs` の `get()` / `names()`（全217〜255行）
- `validate()` の戻り値 `Vec<Violation>` を消費する production 経路の**全体**（§4 に全集合を列挙）:
  - `crates/schemaguard/src/main.rs` の `cmd_check`（105〜146行。ゲート自身の CLI verdict 出力点）
  - `crates/condukt/src/main.rs` の `schema_precheck` / `schema_precheck_each` /
    `render_schema_violations`（4848〜4898行）

目的: CLAUDE.md 第3節（「判定不能は clean ではない。必ず block か ask に解決する」）に照らして、
各分岐が **restrictive**（violation を積む＝block 相当）・**permissive**（意図的・理由付きで
許可する）・**silent-skip**（判定不能なのに何も積まず下流からは "clean" と区別できない）の
いずれかを機械的に分類する。

---

## 1. `crates/schemaguard/src/schema.rs` — `validate()` の全分岐

| # | 行番号 | 逐語引用 | 分類 |
|---|--------|----------|------|
| 1 | schema.rs:86-95 | ```rust\nlet obj = match value.as_object() {\n    Some(o) => o,\n    None => {\n        violations.push(Violation {\n            path: path.to_string(),\n            problem: format!("expected object, got {}", json_type_name(value)),\n        });\n        return violations;\n    }\n};\n``` | **restrictive**（失敗アーム: `value` がオブジェクトでない → violation を積んで即 return）。成功アームは restrictive でも permissive でもない（分岐の分かれ目。後続の全チェックの前提を成立させるだけ）。 |
| 2 | schema.rs:107-114 (val.is_none() かつ required=true) | ```rust\nif val.is_none() {\n    if field.required {\n        violations.push(Violation {\n            path: field_path,\n            problem: "required field missing".to_string(),\n        });\n    }\n    continue;\n}\n``` | **restrictive**（`field.required == true` の内側アーム: violation を積む） |
| 3 | schema.rs:107-114 (val.is_none() かつ required=false) | 同上（`if field.required` の条件が false のときは中の `violations.push` を通らず、外側の `continue;` だけ実行される） | **意図的 permissive**（schema 設計上「このフィールドは無くてもよい」という宣言。`Field.required: bool` という明示フィールドで宣言された意図であり、未対応で検査できなかった silent-skip とは区別される） |
| 4 | schema.rs:120-127 | ```rust\nlet type_ok = match &field.ty {\n    Ty::String => val.is_string(),\n    Ty::Number => val.is_number(),\n    Ty::Bool => val.is_boolean(),\n    Ty::Array => val.is_array(),\n    Ty::Object => val.is_object(),\n    Ty::Any => true,\n};\n``` | `Ty::String`/`Number`/`Bool`/`Array`/`Object` の5本は **restrictive の判定式**（型不一致なら `type_ok = false` になり後段の129行で violation が積まれる）。`Ty::Any => true` は **意図的 permissive**（どんな JSON 型でも常に `type_ok = true`。`Ty` 列挙子の doc comment（schema.rs:19-21）が「Accept any JSON value without a type check.」と明記しており、宣言された意図） |
| 5 | schema.rs:129-136 | ```rust\nif !type_ok {\n    violations.push(Violation {\n        path: field_path.clone(),\n        problem: format!("expected {}, got {}", field.ty, json_type_name(val)),\n    });\n    // No point doing further checks on a mistyped value.\n    continue;\n}\n``` | **restrictive**（型不一致 → violation を積む。ただし `continue` で enum/array チェックをスキップする副作用がある。詳細は下記「continue 2本」） |
| 6 | schema.rs:143-168 (enum_values 非空・値が文字列・許可リストに含まれる) | `Some(s) => { if !field.enum_values.contains(&s) { ... } }` の `contains(&s) == true` の場合 — violation を積まず素通り | 分岐そのものは判定式（restrictive の否定形）。値が許可リストに含まれる場合は正当な pass であり permissive でも silent-skip でもない（enum 制約を実際に適用した結果の合格） |
| 7 | schema.rs:150-157 (Some(s) かつ enum に含まれない) | ```rust\nSome(s) => {\n    if !field.enum_values.contains(&s) {\n        violations.push(Violation {\n            path: field_path.clone(),\n            problem: format!("'{}' not in [{}]", s, allowed),\n        });\n    }\n}\n``` | **restrictive** |
| 8 | schema.rs:159-166 (val.as_str() が None＝値が非文字列) | ```rust\nNone => violations.push(Violation {\n    path: field_path.clone(),\n    problem: format!(\n        "enum constraint [{}] cannot be applied: value is not a string, got {}",\n        allowed,\n        json_type_name(val)\n    ),\n}),\n``` | **restrictive**（重要: この経路は現状すでに restrictive に解決済み。schema.rs:138-142 のコメント「a value the constraint cannot be evaluated against ... is a violation — not a silent skip. "Could not check" is not "passed".」が明言する通り、かつてはここが `if let Some(s) = val.as_str()` で else 節を持たず silent-skip だったことがテスト `enum_on_non_string_value_is_a_violation_not_a_silent_pass`（schema.rs:384-404）のコメントから読み取れる。**現状は既に是正済みであり、本監査時点で残存する silent-skip ではない**） |
| 9 | schema.rs:171-179 (array 要素への再帰が起きる条件) | ```rust\nif field.ty == Ty::Array && !field.items.is_empty() {\n    if let Some(arr) = val.as_array() {\n        for (i, elem) in arr.iter().enumerate() {\n            let elem_path = format!("{}[{}]", field_path, i);\n            let mut sub = validate(elem, field.items, &elem_path);\n            violations.append(&mut sub);\n        }\n    }\n}\n``` | 再帰が**起きる**条件（`field.ty == Ty::Array && !field.items.is_empty()` かつ `val.as_array()` が `Some`）は判定式そのもの（restrictive の入口。中の `validate` 呼び出し結果を素通りで積む） |
| 10 | schema.rs:171-179 (array 要素への再帰が起きない条件・その1: `field.items.is_empty()`) | 同上コードの `!field.items.is_empty()` が false のとき（`items` が空スライス） | **意図的 permissive**（`items` を指定しないフィールドは「配列の中身までは検査しない」という schema 記述者の明示的な選択。`Field.items` の doc comment（schema.rs:47-49）「When `ty == Array` and this slice is non-empty, each element ... is recursively validated」が意図を明記） |
| 11 | schema.rs:171-179 (array 要素への再帰が起きない条件・その2: `val.as_array()` が `None`) | `if let Some(arr) = val.as_array()` の `None` アーム — 何もせず if ブロックを抜ける | **silent-skip（未解決の空集合ではなく到達不能に近いが、コード上は明示的な no-op）**。この分岐に実際に到達するには `field.ty == Ty::Array` かつ 129行の型チェックで `val.is_array()` が既に `type_ok` として要求されているため、通常は `val` が非配列ならこの手前の135行 `continue` で弾かれ、そもそも170行まで到達しない。**したがって通常経路では到達不能**であり、「未検査のまま合格」を生む実害は現状無いが、`type_ok` の判定と `val.as_array()` の判定が別々の式として二重に存在する構造そのものが、将来どちらかが変更されたときに乖離しうる脆さを持つ。本監査では「到達可能性が低い暗黙の前提に依存した no-op」として記録する。 |
| 12 | **schema.rs:114** `continue;`（required チェック後） | ```rust\n            continue;\n```（107-115行のブロック末尾。107行 `if val.is_none() {` に対応） | 何を検査せず次へ進むか: **type_ok（型チェック）・enum_values（enum チェック）・array 要素再帰の3つすべてをスキップする**。`val` が存在しない（フィールド欠落）ケースなので型チェックのしようがなく、この省略自体は自然（値が無いものの型は判定不能であり、判定すべきは「欠落してよいか」だけ）。ただし下流から見ると、**このフィールドについて violation が0件のとき、それが「値が存在し全チェックを通過した」のか「値が存在せず required=false だったので何も検査されなかった」のかは `Vec<Violation>` という出力形状からは区別できない**。violation の不在イコール "全項目検査済みで合格" と読む消費者（`render_schema_violations` はまさにこう読む: `if !violations.is_empty() { bail!(...) } Ok(())`）にとっては、この2つの意味論的に異なる状態が同じ「空集合」に潰れる。 |
| 13 | **schema.rs:135** `continue;`（型チェック失敗後） | ```rust\n            continue;\n```（129-136行のブロック末尾。129行 `if !type_ok {` に対応、コメント「// No point doing further checks on a mistyped value.」） | 何を検査せず次へ進むか: **enum_values チェックと array 要素再帰の2つをスキップする**。型が既に不一致（violation を1件積んだ直後）なので、この省略は「他人（値そのもの）が壊れている以上、その中身の enum/array 再帰チェックをしても得られる情報が増えない」という設計判断であり、コメントで明示されている。**violation は既に1件積まれているため、この `continue` は下流から見た "clean" (空集合) を作らない** — #12 の `continue` との決定的な違いはここ。#12 は violation 0件のまま次へ進みうる（required=false のとき）が、#13 は必ず violation 1件を伴ってから次へ進む。したがって #13 自体は silent-skip ではなく、「既に restrictive に倒した後の枝刈り」に分類される。 |

### `continue` 2本の下流への読まれ方（明示的なまとめ）

- **schema.rs:114（required チェック後の continue）**: `required=false` かつ値が欠落しているケースでは
  violation を1件も積まずに次のフィールドへ進む。これは意図的 permissive（分類 #3）であり、
  `render_schema_violations`（condukt/src/main.rs:4882-4898）はこの結果、violation リストが空なら
  「全フィールドが検査され合格した」というメッセージ性（`Ok(())` を返し何も表示しない）を下流に返す。
  しかし実際には「このフィールドは検査対象にすらならなかった（値が無いから）」というだけであり、
  「値があって、正しい型で、enum を満たし、配列要素も正しかった」から合格したのとは意味が異なる。
  **この違いを `Vec<Violation>` という戻り値の形状は表現できない** — 「合格」と「対象外」が同じ空集合に潰れる。
  これは3値化（`Determination<T>` 的な設計）の候補になりうる箇所として記録する（次タスクの検討対象）。
- **schema.rs:135（型チェック失敗後の continue）**: 既に1件 violation を積んだ後の枝刈りなので、
  下流の「空集合＝合格」という読みには影響しない。silent-skip ではない。

---

## 2. `crates/schemaguard/src/registry.rs` — `get()` / `names()` の全分岐

| # | 行番号 | 逐語引用 | 分類 |
|---|--------|----------|------|
| 1 | registry.rs:220-228 `names()` | ```rust\npub fn names() -> Vec<&'static str> {\n    vec![\n        "decomposition",\n        "episode",\n        "playbook",\n        "scout-measure",\n        "verdict",\n    ]\n}\n``` | 判定を持たない純粋な列挙（restrictive/permissive/silent-skip のいずれにも該当しない — 固定リストを返すだけで、入力に対する判定を行わない）。空を返すことはあり得ない（コンパイル時に固定された5要素のリテラル） |
| 2 | registry.rs:233-236 `"decomposition" => Some(...)` | ```rust\n        "decomposition" => Some(Schema {\n            name: "decomposition".to_string(),\n            fields: DECOMPOSITION_FIELDS.to_vec(),\n        }),\n``` | 既知スキーマ（decomposition）: `Some` を返し、呼び出し元に検証を委譲する。判定なし（ルックアップの成功） |
| 3 | registry.rs:237-240 `"episode" => Some(...)` | ```rust\n        "episode" => Some(Schema {\n            name: "episode".to_string(),\n            fields: EPISODE_FIELDS.to_vec(),\n        }),\n``` | 既知スキーマ（episode）: 同上 |
| 4 | registry.rs:241-244 `"playbook" => Some(...)` | ```rust\n        "playbook" => Some(Schema {\n            name: "playbook".to_string(),\n            fields: PLAYBOOK_FIELDS.to_vec(),\n        }),\n``` | 既知スキーマ（playbook）: 同上 |
| 5 | registry.rs:245-248 `"scout-measure" => Some(...)` | ```rust\n        "scout-measure" => Some(Schema {\n            name: "scout-measure".to_string(),\n            fields: SCOUT_MEASURE_FIELDS.to_vec(),\n        }),\n``` | 既知スキーマ（scout-measure）: 同上 |
| 6 | registry.rs:249-252 `"verdict" => Some(...)` | ```rust\n        "verdict" => Some(Schema {\n            name: "verdict".to_string(),\n            fields: VERDICT_FIELDS.to_vec(),\n        }),\n``` | 既知スキーマ（verdict）: 同上 |
| 7 | registry.rs:253 `_ => None` | ```rust\n        _ => None,\n``` | **unknown スキーマ名は `None` を返す**。`get()` 自体はこれを明示的な三値（`Some`/`None`）で表現しており、`get()` 単体は判定不能を潰していない。**ただし呼び出し元（condukt/src/main.rs の `schema_precheck`/`schema_precheck_each`）はこの `None` を「未知スキーマだから追加のバリデーションを行わなくてよい」と読み、`return Ok(())` する** — これは registry.rs 自身の問題ではなく、呼び出し側（4節参照）の permissive な消費のされ方であることに注意。registry.rs の `get()` それ自体は restrictive でも permissive でもない中立なルックアップである。 |

### registry.rs のフィールド定義（`DECOMPOSITION_TASK_FIELDS` 等）に現れる意図的 permissive

`get()`/`names()` の分岐そのものではないが、返される `Schema` の `required` 値もこの監査の射程に
含まれる（`validate()` の分類#3「required=false」が実際にどの経路で踏まれるかを特定するため）:

- registry.rs:19-62 `DECOMPOSITION_TASK_FIELDS`: `title`/`class`/`done_criteria`/`suggested_model`/
  `confidence` はすべて `required: false`。registry.rs:10-18 のコメントが「`required` here is
  deliberately kept in lockstep with condukt's `model::Task` struct」と明記しており、**意図的
  permissive**（condukt の deserializer 側が `#[serde(default)]` を持つことに合わせた設計判断）。
- registry.rs:73-79 `DECOMPOSITION_FIELDS` の `tasks` フィールド: `Ty::Array` かつ
  `items: DECOMPOSITION_TASK_FIELDS`（非空）なので、schema.rs:171 の再帰が実際に起きる唯一の
  registry.rs 上の使用箇所。

---

## 3. 意図的 permissive と silent-skip の明示的な区別

**この2つを混同してはならない。** 本監査では以下を明確に別カテゴリとして扱う:

### 意図的 permissive（宣言された意図・理由あり）
「この入力パターンは正しい／許容範囲内であると schema 設計者が明示的に選んだ」結果、violation を
積まない経路。**理由がコード上（doc comment・変数名・型設計）に残っている**。

- `Ty::Any`（schema.rs:20-21, 126）— doc comment で「Accept any JSON value without a type check.」と明言。
  テスト `any_type_accepts_all_values`（schema.rs:368-374）がこの挙動を固定する。
- `field.required == false`（schema.rs:44, 108, 114 の `continue`）— `Field.required: bool` という
  明示フィールドで宣言された設計（「Whether absence of this key is a violation.」schema.rs:43-44）。
  registry.rs 側でも `DECOMPOSITION_TASK_FIELDS` のコメント（registry.rs:10-18）が condukt の
  `model::Task` の serde 属性との整合性という**具体的な理由**を書いている。
- 未知の追加フィールド（`obj.get(field.name)` で `fields` に無いキーは一切走査されない。
  schema.rs:80 の doc comment「Unknown extra fields are silently allowed.」で明言）—
  テスト `unknown_extra_fields_are_allowed`（schema.rs:345-350）がこの挙動を固定する。
- `field.items.is_empty()` のとき配列要素を再帰検証しない（schema.rs:47-49 の doc comment で
  「When `ty == Array` and this slice is non-empty, ...」と明記）。

これらは **「宣言された意図的 permissive」** であり、次タスクの三値化がこれらの挙動を壊してはならない
（壊すと `unknown_extra_fields_are_allowed` / `any_type_accepts_all_values` / 各 `*_task_with_only_id_is_valid`
系のテストが red になる）。

### silent-skip（未解決の空集合・本監査で識別されたもの）
「判定すべきかどうか自体が構造的に曖昧」で、violation の不在が「合格」なのか「そもそも検査されて
いない」なのか `Vec<Violation>` という戻り値の形状からは区別できない経路。

- **schema.rs:114 の `continue`（required=false の場合、分類 #3 と同一事象を別角度から見たもの）**:
  「値が存在しないので検査対象外」という状態と「値が存在し全チェックを通過した」という状態が、
  どちらも呼び出し元からは「このフィールドについて violation が0件」としか見えない。これは
  **意図的 permissive でありながら、同時に「検査した」と「検査対象外だった」を戻り値の形状が
  区別できていない** という二重の性質を持つ唯一の箇所である。理由（`required: false`）は明示されて
  いるが、結果の表現力（`Vec<Violation>` という二値的な形状）が「対象外」と「合格」を潰している点が
  次タスクの三値化（`validate()` の戻り値を `Determination` 的な型にする、あるいは各フィールドの
  検証結果を「Checked(pass/fail)」「NotApplicable」で分けて返す）の動機になる。
- **schema.rs:171-179 の `val.as_array()` が `None` になるアーム（分類 #11）**: 通常経路では
  `type_ok` チェック（129行）で先に弾かれるため到達不能に近いが、コード上は「配列だと分かっている
  はずの値が `as_array()` で取れなかった」場合に何も violation を積まず no-op で抜ける。到達可能性は
  低いが、`field.ty == Ty::Array` という条件と `val.is_array()` という実行時チェックが分離された
  二重表現である以上、**将来どちらかの判定式だけが変更されると乖離しうる構造的な脆さ**として記録する。

**この2つのカテゴリの決定的な違い**: 意図的 permissive は「なぜ許すか」の理由が **恒久的に成立する
設計原則**（型設計・serde 整合性）に基づく。silent-skip は「なぜ何も積まないか」の理由が **戻り値の
表現力不足**（Vec<Violation> が三値を表現できない）か、**判定式の二重化による偶発的到達不能性**に
基づく。前者はテストで固定してよい仕様であり、後者は次タスクが RED を書いて塞ぐべき対象である。

---

## 4. `validate()` の呼び出し元の全集合と、各呼び出し側での消費

**呼び出し元の全集合**（実測: 本監査で `grep -rn "schema::validate" crates/ --include "*.rs"` を
再実行し、`#[cfg(test)] mod tests`（schema.rs:185 / registry.rs:259 以降）に属するヒットを除いた
production 経路）:

| # | 呼び出し箇所 | 逐語引用（当該行） | 消費者 |
|---|--------------|--------------------|--------|
<!-- doc-claim-exempt: historical quote — this audit is a snapshot of the pre-fix tree; the recursion was rewritten to `report.absorb(validate_report(elem, field.items, &elem_path))` by the three-valued fix (schemaguard 0.1.9) this audit motivated -->
| A | `crates/schemaguard/src/schema.rs:175` | 「let mut sub = validate(elem, field.items, &elem_path);」 | `validate()` 自身からの再帰呼び出し（配列要素。§1 分類 #9）。戻り値は次行の append で親の violation リストへ合流する |
<!-- doc-claim-exempt: historical quote — this audit is a snapshot of the pre-fix tree; `cmd_check` now calls `schema::validate_report` and resolves it through `check_verdict` (schemaguard 0.1.9), which is exactly the change this row's finding asked for -->
| B | `crates/schemaguard/src/main.rs:121` | 「let violations = schema::validate(&value, &schema.fields, "");」 | **schemaguard 自身の CLI `cmd_check`**。§4.2 |
| C | `crates/condukt/src/main.rs:5100` | 「let violations = schemaguard::schema::validate(&value, &schema.fields, "");」 | condukt `schema_precheck`。§4.1 |
| D | `crates/condukt/src/main.rs:5114` | 「let mut sub = schemaguard::schema::validate(v, &schema.fields, &format!("[{i}]"));」 | condukt `schema_precheck_each`。§4.1 |

`schemaguard` を依存に持つ crate は condukt のみである（実測: `grep -rn "schemaguard" crates/*/Cargo.toml`
→ 自クレートの `[package]`/`[lib]`/`[[bin]]` 名を除くと `crates/condukt/Cargo.toml:18`
`schemaguard = { path = "../schemaguard" }` の1件のみ）。したがって上表 A〜D が
**`Vec<Violation>` を消費する production 経路の全体**であり、5番目は存在しない。

> **訂正（本監査の前版の誤り）**: 前版はここで「`validate()` の唯一の呼び出し元は condukt の以下3関数」
> と書いていた。これは事実に反する。**監査対象クレート自身の CLI（`crates/schemaguard/src/main.rs:121`）が
> 4番目の呼び出し元**であり、しかもそこは §1/§3 で識別した空集合の曖昧さが外部から観測可能な verdict
> （`"valid": true` / exit 0 / reject カウンタのスキップ）へ変換される、まさにその地点である。
> 全 verdict 経路を洗い出すことを目的とする監査が、持っていない網羅性（「唯一の」）を主張し、
> ゲート自身の CLI verdict 経路を未検査のまま残していた。本版で §4.2 として検査・追記する。

### 4.1 condukt 側の消費 — `schema_precheck` 系3関数（4848〜4898行、実測済み）

```rust
/// Validate raw LLM JSON against a named schemaguard schema BEFORE deserialize.
/// On violations, return a deterministic, structured error enumerating them
/// (so the caller re-asks rather than blindly executing / cryptically failing).
/// Unknown schema name or unparseable JSON is left to the existing serde path
/// (fail-soft: we only ADD a clearer error, never suppress a real one).
fn schema_precheck(raw: &str, schema_name: &str) -> Result<()> {
    let Some(schema) = schemaguard::registry::get(schema_name) else {
        return Ok(());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(());
    };
    let violations = schemaguard::schema::validate(&value, &schema.fields, "");
    render_schema_violations(schema_name, violations)
}
```
（condukt/src/main.rs:4848-4862）

```rust
/// Validate each element of a slice of already-parsed JSON values against a
/// named schema, aggregating violations across elements (path-prefixed with
/// the element index) before bailing. Used for array-shaped LLM output (e.g.
/// consensus verdicts) where `schema_precheck` operates on a single object.
fn schema_precheck_each(values: &[serde_json::Value], schema_name: &str) -> Result<()> {
    let Some(schema) = schemaguard::registry::get(schema_name) else {
        return Ok(());
    };
    let mut violations = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let mut sub = schemaguard::schema::validate(v, &schema.fields, &format!("[{i}]"));
        violations.append(&mut sub);
    }
    render_schema_violations(schema_name, violations)
}
```
（condukt/src/main.rs:4864-4878）

```rust
/// Shared bail-with-structured-violations rendering for the two precheck
/// helpers above.
fn render_schema_violations(
    schema_name: &str,
    violations: Vec<schemaguard::schema::Violation>,
) -> Result<()> {
    if !violations.is_empty() {
        let lines: Vec<String> = violations
            .iter()
            .map(|v| format!("  - {}: {}", v.path, v.problem))
            .collect();
        bail!(
            "invalid {schema_name} JSON ({} schema violation(s)) — re-ask required:\n{}",
            violations.len(),
            lines.join("\n")
        );
    }
    Ok(())
}
```
（condukt/src/main.rs:4880-4898）

呼び出し箇所（実測、いずれも `?` で `Result` を即座に伝播しており in-process 直接消費）:

- condukt/src/main.rs:3145 `schema_precheck_each(items, "verdict")?;`（consensus verdict 配列入力）
- condukt/src/main.rs:3149 `schema_precheck_each(items, "verdict")?;`（`{"verdicts": [...]}` ラッパー形状）
- condukt/src/main.rs:3423 `schema_precheck(&raw, "decomposition")?;`（`run_state` の
  `StateAction::Init` で decomposition JSON を deserialize する直前）

**事実として記録する消費の形状**:

1. `schemaguard::schema::validate` は `Vec<Violation>` を返す関数のまま、`schema_precheck` /
   `schema_precheck_each` が **同一プロセス内（別 crate だが同一バイナリ）で直接呼び出している**。
   HTTP/CLI 越しの間接呼び出しではない。
2. `registry::get()` が `None`（unknown schema）を返す経路、および `serde_json::from_str` が失敗する
   経路は、どちらも `schema_precheck` 内で **`return Ok(())`**（violation 0件で早期成功）に潰される。
   これは schema.rs 側の分類ではなく main.rs 側の意図的 permissive であり、doc comment
   （4851-4852行: 「Unknown schema name or unparseable JSON is left to the existing serde path
   (fail-soft: we only ADD a clearer error, never suppress a real one).」）が理由を明示している —
   後続の `serde_json::from_str::<model::Decomposition>` 等、既存の deserialize 経路がこの後に必ず
   控えているため、`schema_precheck` が黙って `Ok(())` を返しても最終的な検証が失われるわけではない
   （二重チェックの前段が失敗しただけで後段の serde が本来のエラーを出す設計）。ただし、この
   「後段が拾うはずだ」という前提そのものは本監査では検証していない（後段の serde 側の挙動は
   スコープ外）。
3. `violations.is_empty()` の判定（render_schema_violations 内、4886行）が **condukt 側の唯一の
   block/pass 分岐点**であり（schemaguard 自身の CLI にも別の分岐点がある。§4.2）、`Vec<Violation>` という形状をそのまま bool へ潰している。CLAUDE.md 第3節が
   要求する「`Result`/`Option` を bool へ潰すとき、既定値は必ず制限側」という観点では、ここは
   `!violations.is_empty()` という比較そのものが判定なので "既定値" の概念は無いが、**空集合が
   「検査した上で問題なし」なのか「検査対象外だった」なのかを区別できない**という schema.rs 側の
   構造的な問題（1節・3節参照）が、この bool 化を経由してそのまま condukt の CLI 終了コード・
   エラーメッセージへ伝播する。

### 4.2 schemaguard 自身の CLI `cmd_check`（`crates/schemaguard/src/main.rs:105-146`）— 2つ目の in-process な `Vec<Violation>` 消費者

`Vec<Violation>` を消費する production 経路は condukt だけではない。**監査対象クレート自身の CLI が
2つ目の in-process 消費者**であり、ここは condukt のような別 crate 越しの利用ではなく
**ゲート自身が外部へ verdict を出す出力点**（stdout の JSON・プロセス終了コード・reject カウンタ）である。

逐語引用（schemaguard/src/main.rs:105-145。146行は `cmd_check` の閉じ括弧 `}`）:

```rust
    // Parse JSON
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            // Parse failure counts as a reject
            metrics::record_reject(&schema.name, 1);
            let out = json!({
                "valid": false,
                "error": format!("invalid JSON: {}", e)
            });
            println!("{}", serde_json::to_string(&out).unwrap());
            return 2;
        }
    };

    // Validate
    let violations = schema::validate(&value, &schema.fields, "");

    if violations.is_empty() {
        let out = json!({
            "valid": true,
            "schema": schema.name,
            "errors": []
        });
        println!("{}", serde_json::to_string(&out).unwrap());
        0
    } else {
        let error_count = violations.len();
        metrics::record_reject(&schema.name, error_count);
        let errors: Vec<_> = violations
            .iter()
            .map(|v| json!({"path": v.path, "problem": v.problem}))
            .collect();
        let out = json!({
            "valid": false,
            "schema": schema.name,
            "errors": errors
        });
        println!("{}", serde_json::to_string(&out).unwrap());
        1
    }
```

**事実として記録する消費の形状**:

1. `violations.is_empty()`（main.rs:123）が **CLI 側の唯一の block/pass 分岐点**であり、空集合が
   3つの外部観測可能な出力へ同時に写される:
   - stdout の `{"valid": true, "schema": ..., "errors": []}`（main.rs:124-129）
   - プロセス終了コード **0**（main.rs:130。非空側は **1**、main.rs:144）
   - `metrics::record_reject` を **呼ばない**（非空側だけが main.rs:133 で
     `metrics::record_reject(&schema.name, error_count);` を呼ぶ）。すなわち「検査対象外だったので
     violation 0件」だったケースは reject カウンタにも一切現れない。
2. したがって §1/§3 で識別した「空の violation 集合は『検査して合格』と『適用外』を区別できない」
   という性質は、condukt の in-process 経路（§4.1）だけでなく、**`schemaguard check` の
   `"valid": true` と exit 0 という形で外部境界も越える**。この CLI 出力を消費する側
   （shell スクリプト・他フック）は `"valid": true` を「全フィールドを検査した上で合格」と読むが、
   実際には「`required: false` のフィールドは一度も検査されていない」場合を含む（§5-8 に入力例）。
3. **対照的に、同じ CLI の「判定不能」経路は既に restrictive に解決している**（本監査で確認した事実。
   したがって §4.2 の指摘は「判定不能が permissive」型ではなく「検査対象外が合格として出力される」型に
   限定される）: unknown schema 名は exit **2**（main.rs:69-79。condukt 側の `schema_precheck` が
   同じ状況を `return Ok(())` へ潰す（§5-5）のとは逆の選択）、ファイル/stdin の読み取り失敗も exit **2**
   （main.rs:83-103）、JSON パース失敗は exit **2** かつ `metrics::record_reject(&schema.name, 1)`
   （main.rs:106-118）。crate doc（main.rs:9-13）も3値の終了コードを明記している:

   ```rust
   //! Exit codes:
   //!   0  — JSON parsed and schema valid (or `metrics`/`list` succeeded)
   //!   1  — JSON parsed but schema violations found
   //!   2  — could not determine: JSON failed to parse, an unknown schema was
   //!        requested, or (`metrics`) the reject store exists but is unreadable
   ```

**次タスク（`validate()` の三値化）への制約として明記する**:

`Vec<Violation>` という形状に **構造的に依存**している消費者は、**2つの crate にまたがる4関数**である
（§4 の呼び出し元全集合 B〜D に対応。いずれも `violations.is_empty()` で分岐し、`violations.len()` /
`v.path` / `v.problem` を出力へ埋め込む）:

- condukt: `schema_precheck` / `schema_precheck_each` / `render_schema_violations`
  （condukt/src/main.rs:4848-4898）
- **schemaguard 自身: `cmd_check`（schemaguard/src/main.rs:105-146）** — 前版はこれを見落としていた
  （§4 冒頭の訂正を参照）

次タスクが `validate()` の戻り値を三値型（例: `Determination<Vec<Violation>>` や、フィールド単位で
`Checked`/`NotApplicable` を区別する型）へ変更する場合、**この4関数のシグネチャ・消費ロジックを
壊さない設計を優先すべき** ——具体的には、三値型から既存の `Vec<Violation>` 相当（「検査した上での
違反リスト」）を取り出す変換経路を用意し、呼び出し側（condukt/src/main.rs:3145/3149/3423 の3箇所と
schemaguard/src/main.rs:121）は無改修で通せることが望ましい。

壊す場合に lockstep で追随が必要な対象は**2系統**ある:

1. **schemaguard 自身の `src/main.rs`** — 同一 crate 内なので `touched_files` は
   `crates/schemaguard/` で閉じるが、`crates/schemaguard/` を触った時点で version bump の
   3ファイル lockstep（`Cargo.toml` / `.claude-plugin/plugin.json` /
   `.claude-plugin/marketplace.json`。本監査時点でいずれも `0.1.8`）の対象になる。
   また `cmd_check` を変更する以上、三値化の効果は **exit code / `"valid"` フィールドという
   外部契約**（main.rs:9-13 の doc に明記された 0/1/2）に現れるため、その契約を変えるか
   維持するかを設計時に決める必要がある。
2. **condukt 側** — こちらは `crates/schemaguard/` に閉じないため、同一バッチ・同一 PR での
   追随が必要になる。

---

## 5. permissive へ潰れている経路の既知集合（次タスクが RED を書く対象）

以下は本監査で識別した、**violation が積まれない（＝「問題なし」と読める）経路**の一覧。
「意図的 permissive」と明記したものは仕様として維持すべき対象、それ以外は次タスクの三値化・
RED 対象の候補として列挙する。

1. **`Ty::Any` フィールドはどんな値でも合格する**（schema.rs:126, 意図的 permissive）。
   入力例: スキーマ定義が `Field { name: "metadata", ty: Ty::Any, required: false, ... }` のとき、
   `{"metadata": 42}` も `{"metadata": {"nested": true}}` も `{"metadata": null}` もすべて violation 0件。
   現状 registry.rs の5スキーマはどれも `Ty::Any` フィールドを持たないため、この経路は
   `validate`/`Field` を直接使う外部呼び出し（テストの `ANY_AND_OBJECT_FIELDS`,
   `ANY_WITH_ENUM_FIELDS` 経由）でのみ踏まれる。**condukt 本番経路では未到達（現状は無害）**。

2. **`required: false` のフィールドが欠落していても合格する**（schema.rs:107-115, 意図的 permissive
   だが3節で述べた「検査対象外」と「合格」の区別不能という表現力不足を伴う）。
   入力例: `decomposition` スキーマで `{"goal": "x", "tasks": [{"id": "t1"}]}` — `title`/`class`/
   `done_criteria`/`suggested_model`/`confidence` を一切含まなくても violation 0件
   （テスト `decomposition_task_with_only_id_is_valid`, registry.rs:315-327 が固定）。

3. **未知の追加フィールドは一切検査されない**（schema.rs:80, 97-102 のループが `fields` を
   起点に走査するため。意図的 permissive）。
   入力例: `{"name": "Dan", "role": "user", "extra_unknown_field": true, "__proto__": "anything"}` —
   `SIMPLE_FIELDS` に定義の無いキーはすべて素通り（テスト `unknown_extra_fields_are_allowed`,
   schema.rs:345-350 が固定）。**この経路は本監査の done_criteria が名指しする「宣言された意図的
   permissive」の代表例であり、silent-skip と混同してはならない対象そのもの。**

4. **`items` が空スライスの `Ty::Array` フィールドは要素の中身を一切検証しない**（schema.rs:171,
   `!field.items.is_empty()` が false、意図的 permissive）。
   入力例: 仮にスキーマが `Field { name: "tags", ty: Ty::Array, items: &[], ... }` のように定義
   された場合、`{"tags": [1, "two", {"three": true}, null]}` のように型が混在した配列でも
   トップレベルが配列であること以外は一切検証されない。**現状 registry.rs の5スキーマのうち
   `Ty::Array` を使うのは `decomposition.tasks` の1箇所のみで、これは `items:
   DECOMPOSITION_TASK_FIELDS`（非空）を指定しているため、この経路（items 空）は condukt 本番の
   5スキーマでは踏まれない。**

5. **`schema_precheck`/`schema_precheck_each` は unknown スキーマ名を無条件で `Ok(())` にする**
   （condukt/src/main.rs:4854-4856, 4869-4871。schema.rs ではなく main.rs 側の permissive）。
   入力例: `schema_precheck(raw, "no-such-schema")` は `raw` の中身を一切見ずに `Ok(())` を返す
   （テスト `unknown_schema_name_skips_precheck` 相当、main.rs:5188 `assert!(schema_precheck(r#"{"anything": true}"#, "no-such-schema").is_ok());` で固定されている——このテストは本監査時点で
   condukt/src/main.rs に既存）。schemaguard 側の `registry::get()` 自体は `None` を正しく返しており
   （3値のうちの1つとして表現できている）、permissive なのは呼び出し側の「`None` なら
   バリデーションを行わず deserialize 任せにする」という選択である。

6. **schema.rs:114 の `continue` が生む「合格」と「対象外」の区別不能**（3節で詳述、silent-skip
   寄りの構造的問題。次タスクの主要な三値化対象）。
   入力例: `decomposition` スキーマにおいて `class` フィールド（`required: false`,
   `enum_values: &["parallel", "serial", "gated"]`）を持つタスク `{"id": "t1"}`（`class` 自体が
   欠落）は violation 0件。一方 `{"id": "t1", "class": "parallel"}`（enum を実際に満たす）も
   violation 0件。**この2つの入力は `validate()` の戻り値だけからは区別できない** —
   前者は enum 制約が一度も評価されておらず、後者は評価されて合格した。

7. **schema.rs:171-179 の `val.as_array()` が `None` になるアーム**（分類 #11・3節。到達可能性が
   低い構造的脆さとして記録、現状の実害は無し）。
   入力例（理論上）: もし将来 `Ty::Array` の判定条件（129行の `Ty::Array => val.is_array()`）と
   170行の `val.as_array()` 呼び出しの間に乖離が生じる変更が入った場合、`type_ok` は `true` なのに
   `val.as_array()` が `None` を返す状況が起こりうる。現状のコードでは両者は同じ `val` に対する
   同じ判定（`is_array()`）を実質的に二重評価しているため到達不能だが、**判定式が2箇所に分離
   していること自体がリスク**。

8. **`schemaguard check` は violation 0件を無条件に `"valid": true` / exit 0 / reject カウント無しへ
   写す**（schemaguard/src/main.rs:121-133。**ゲート自身の verdict 出力点**であり、上記 6・7 の
   silent-skip が外部から観測可能な verdict になる出口。§4.2 で詳述）。
   入力例: `decomposition` スキーマの `class` フィールドの定義は以下（逐語、registry.rs:34-40）。

   ```rust
       Field {
           name: "class",
           ty: Ty::String,
           required: false,
           enum_values: &["parallel", "serial", "gated"],
           items: &[],
       },
   ```

   したがって
   `echo '{"goal":"x","tasks":[{"id":"t1"}]}' | schemaguard check --schema decomposition` は
   enum 制約を一度も評価しないまま `{"valid":true,"schema":"decomposition","errors":[]}` を
   stdout に出して exit 0 で終わり、`metrics::record_reject` も呼ばれない。
   `{"goal":"x","tasks":[{"id":"t1","class":"parallel"}]}`（enum を実際に評価して合格）と
   **外部からは完全に同一の観測結果**になる。「検査して合格」と「検査対象外」の区別不能が、
   in-process の戻り値だけでなく **CLI の外部境界を越えて伝播する**のがこの項目である。
   なお同じ CLI の**判定不能**経路（unknown schema / 読み取り失敗 / パース失敗）は既に exit 2 へ
   restrictive に解決済みであり（§4.2-3）、この項目は「判定不能が permissive」型ではない。

---

## 付記: cargo test 確認

`cargo test -p schemaguard` は本監査（`crates/schemaguard/` 無変更）の状態で green であることを
確認済み（監査完了後に実行、詳細は commit ログ参照）。
