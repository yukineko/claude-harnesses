#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box tests for `webqa weaken-check` (independent test author).
//!
//! Contract under test:
//!   exit 0 = no weakening found
//!   exit 1 = weakening found; each finding on stdout with file path + rule id
//!   exit 2 = cannot determine (unreadable diff file, non-diff garbage, git failure)
//! Only Playwright spec files (.spec.ts/.spec.js/.test.ts/.test.js/.spec.tsx/
//! .test.tsx/.spec.mjs) are inspected.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_path(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "webqa-weaken-check-{}-{}-{}-{}.diff",
        tag,
        std::process::id(),
        n,
        nanos
    ))
}

/// Writes `diff` to a unique temp file, runs `webqa weaken-check --diff-file <it>`,
/// removes the file, and returns the process output.
fn run_with_diff(tag: &str, diff: &str) -> Output {
    let path = unique_path(tag);
    std::fs::write(&path, diff).expect("write diff fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_webqa"))
        .arg("weaken-check")
        .arg("--diff-file")
        .arg(&path)
        .output()
        .expect("spawn webqa");
    let _ = std::fs::remove_file(&path);
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn describe(out: &Output) -> String {
    format!(
        "status={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_finding(out: &Output, file: &str, rule: &str) {
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 (weakening found)\n{}",
        describe(out)
    );
    let so = stdout(out);
    assert!(
        so.lines().any(|l| l.contains(file) && l.contains(rule)),
        "expected a stdout line containing both `{file}` and `{rule}`\n{}",
        describe(out)
    );
}

fn assert_exit(out: &Output, code: i32) {
    assert_eq!(out.status.code(), Some(code), "{}", describe(out));
}

// ---------------------------------------------------------------- rules

#[test]
fn skip_added_is_reported() {
    let diff = "\
diff --git a/tests/login.spec.ts b/tests/login.spec.ts
index 1111111..2222222 100644
--- a/tests/login.spec.ts
+++ b/tests/login.spec.ts
@@ -1,7 +1,7 @@
 import { test, expect } from '@playwright/test';

-test('user can log in', async ({ page }) => {
+test.skip('user can log in', async ({ page }) => {
   await page.goto('/login');
   await page.fill('#user', 'alice');
   await expect(page.locator('#welcome')).toBeVisible();
";
    let out = run_with_diff("skip", diff);
    assert_finding(&out, "tests/login.spec.ts", "skip-added");
}

#[test]
fn describe_skip_added_is_reported_as_skip_added() {
    let diff = "\
diff --git a/e2e/cart.test.js b/e2e/cart.test.js
index 1111111..2222222 100644
--- a/e2e/cart.test.js
+++ b/e2e/cart.test.js
@@ -1,4 +1,4 @@
 const { test, expect } = require('@playwright/test');

-test.describe('cart', () => {
+test.describe.skip('cart', () => {
   test('adds item', async ({ page }) => {
";
    let out = run_with_diff("describe-skip", diff);
    assert_finding(&out, "e2e/cart.test.js", "skip-added");
}

#[test]
fn only_added_is_reported() {
    let diff = "\
diff --git a/tests/search.spec.ts b/tests/search.spec.ts
index 1111111..2222222 100644
--- a/tests/search.spec.ts
+++ b/tests/search.spec.ts
@@ -3,5 +3,5 @@ import { test, expect } from '@playwright/test';

-test('search returns results', async ({ page }) => {
+test.only('search returns results', async ({ page }) => {
   await page.goto('/search?q=cats');
   await expect(page.locator('.result')).toHaveCount(10);
 });
";
    let out = run_with_diff("only", diff);
    assert_finding(&out, "tests/search.spec.ts", "only-added");
}

#[test]
fn expect_removed_is_reported() {
    let diff = "\
diff --git a/tests/profile.spec.ts b/tests/profile.spec.ts
index 1111111..2222222 100644
--- a/tests/profile.spec.ts
+++ b/tests/profile.spec.ts
@@ -5,8 +5,6 @@ import { test, expect } from '@playwright/test';
 test('profile shows name', async ({ page }) => {
   await page.goto('/profile');
-  await expect(page.locator('#name')).toHaveText('Alice');
-  await expect(page.locator('#email')).toHaveText('alice@example.com');
   await page.click('#edit');
 });
";
    let out = run_with_diff("expect-removed", diff);
    assert_finding(&out, "tests/profile.spec.ts", "expect-removed");
}

#[test]
fn wait_timeout_added_is_reported() {
    let diff = "\
diff --git a/tests/dashboard.spec.tsx b/tests/dashboard.spec.tsx
index 1111111..2222222 100644
--- a/tests/dashboard.spec.tsx
+++ b/tests/dashboard.spec.tsx
@@ -4,6 +4,7 @@ import { test, expect } from '@playwright/test';
 test('dashboard loads', async ({ page }) => {
   await page.goto('/dashboard');
+  await page.waitForTimeout(3000);
   await expect(page.locator('#chart')).toBeVisible();
 });
";
    let out = run_with_diff("wait-timeout", diff);
    assert_finding(&out, "tests/dashboard.spec.tsx", "wait-timeout-added");
}

#[test]
fn timeout_raised_in_options_object_is_reported() {
    let diff = "\
diff --git a/tests/upload.spec.ts b/tests/upload.spec.ts
index 1111111..2222222 100644
--- a/tests/upload.spec.ts
+++ b/tests/upload.spec.ts
@@ -6,6 +6,6 @@ test('upload completes', async ({ page }) => {
   await page.goto('/upload');
   await page.setInputFiles('#file', 'fixture.png');
-  await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });
+  await expect(page.locator('#done')).toBeVisible({ timeout: 30000 });
 });
";
    let out = run_with_diff("timeout-opts", diff);
    assert_finding(&out, "tests/upload.spec.ts", "timeout-raised");
}

#[test]
fn timeout_raised_via_set_timeout_is_reported() {
    let diff = "\
diff --git a/tests/report.spec.mjs b/tests/report.spec.mjs
index 1111111..2222222 100644
--- a/tests/report.spec.mjs
+++ b/tests/report.spec.mjs
@@ -3,6 +3,6 @@ import { test, expect } from '@playwright/test';
 test('report renders', async ({ page }) => {
-  test.setTimeout(5000);
+  test.setTimeout(60000);
   await page.goto('/report');
   await expect(page.locator('#table')).toBeVisible();
 });
";
    let out = run_with_diff("timeout-set", diff);
    assert_finding(&out, "tests/report.spec.mjs", "timeout-raised");
}

#[test]
fn matcher_weakened_is_reported() {
    let diff = "\
diff --git a/tests/home.test.ts b/tests/home.test.ts
index 1111111..2222222 100644
--- a/tests/home.test.ts
+++ b/tests/home.test.ts
@@ -4,6 +4,6 @@ import { test, expect } from '@playwright/test';
 test('home heading', async ({ page }) => {
   await page.goto('/');
-  await expect(page.getByRole('heading')).toHaveText('Welcome');
+  expect(page.getByRole('heading')).toBeTruthy();
 });
";
    let out = run_with_diff("matcher", diff);
    assert_finding(&out, "tests/home.test.ts", "matcher-weakened");
}

// ---------------------------------------------------------------- clean cases

#[test]
fn harmless_spec_diff_adding_a_test_exits_0() {
    let diff = "\
diff --git a/tests/login.spec.ts b/tests/login.spec.ts
index 1111111..2222222 100644
--- a/tests/login.spec.ts
+++ b/tests/login.spec.ts
@@ -10,3 +10,9 @@ test('user can log in', async ({ page }) => {
   await expect(page.locator('#welcome')).toBeVisible();
 });

+test('user sees error on bad password', async ({ page }) => {
+  await page.goto('/login');
+  await page.fill('#password', 'wrong');
+  await page.click('#submit');
+  await expect(page.locator('#error')).toHaveText('Invalid credentials');
+});
";
    let out = run_with_diff("harmless", diff);
    assert_exit(&out, 0);
}

#[test]
fn non_spec_file_with_skip_pattern_is_ignored() {
    let diff = "\
diff --git a/src/app.ts b/src/app.ts
index 1111111..2222222 100644
--- a/src/app.ts
+++ b/src/app.ts
@@ -1,3 +1,5 @@
 export function run() {
+  test.skip('not a spec', () => {});
+  await page.waitForTimeout(1000);
   return 1;
 }
";
    let out = run_with_diff("non-spec", diff);
    assert_exit(&out, 0);
}

#[test]
fn empty_diff_exits_0() {
    let out = run_with_diff("empty", "");
    assert_exit(&out, 0);
}

// ---------------------------------------------------------------- cannot determine

#[test]
fn unreadable_diff_file_exits_2() {
    let missing = unique_path("missing");
    let _ = std::fs::remove_file(&missing);
    assert!(!missing.exists());
    let out = Command::new(env!("CARGO_BIN_EXE_webqa"))
        .arg("weaken-check")
        .arg("--diff-file")
        .arg(&missing)
        .output()
        .expect("spawn webqa");
    assert_exit(&out, 2);
}

#[test]
fn garbage_non_diff_input_exits_2() {
    let garbage = "hello world\nthis is not a unified diff\njust some prose test.skip( here\n";
    let out = run_with_diff("garbage", garbage);
    assert_exit(&out, 2);
}

// ---------------------------------------------------------------- hardening

#[test]
fn timeout_raised_across_hunks_of_same_file_is_reported() {
    let diff = "\
diff --git a/tests/slow.spec.ts b/tests/slow.spec.ts
index 1111111..2222222 100644
--- a/tests/slow.spec.ts
+++ b/tests/slow.spec.ts
@@ -3,4 +3,3 @@ import { test, expect } from '@playwright/test';
 test('slow page', async ({ page }) => {
-  await expect(page.locator('#a')).toBeVisible({ timeout: 1000 });
   await page.goto('/slow');
 });
@@ -20,3 +19,4 @@ test('other', async ({ page }) => {
   await page.goto('/other');
+  await expect(page.locator('#a')).toBeVisible({ timeout: 60000 });
 });
";
    let out = run_with_diff("timeout-cross-hunk", diff);
    assert_finding(&out, "tests/slow.spec.ts", "timeout-raised");
}

#[test]
fn timeout_raised_via_product_expression_is_reported() {
    let diff = "\
diff --git a/tests/prod.spec.ts b/tests/prod.spec.ts
index 1111111..2222222 100644
--- a/tests/prod.spec.ts
+++ b/tests/prod.spec.ts
@@ -4,5 +4,5 @@ import { test, expect } from '@playwright/test';
 test('prod', async ({ page }) => {
   await page.goto('/prod');
-  await expect(page.locator('#x')).toBeVisible({ timeout: 1000 });
+  await expect(page.locator('#x')).toBeVisible({ timeout: 60 * 1000 });
 });
";
    let out = run_with_diff("timeout-product", diff);
    assert_finding(&out, "tests/prod.spec.ts", "timeout-raised");
}

#[test]
fn non_evaluable_timeout_replacing_literal_exits_2() {
    let diff = "\
diff --git a/tests/ident.spec.ts b/tests/ident.spec.ts
index 1111111..2222222 100644
--- a/tests/ident.spec.ts
+++ b/tests/ident.spec.ts
@@ -4,5 +4,5 @@ import { test, expect } from '@playwright/test';
 test('ident', async ({ page }) => {
   await page.goto('/ident');
-  await expect(page.locator('#x')).toBeVisible({ timeout: 1000 });
+  await expect(page.locator('#x')).toBeVisible({ timeout: TIMEOUT_MS });
 });
";
    let out = run_with_diff("timeout-ident", diff);
    assert_exit(&out, 2);
}

#[test]
fn new_set_timeout_above_default_is_reported() {
    let diff = "\
diff --git a/tests/newset.spec.ts b/tests/newset.spec.ts
index 1111111..2222222 100644
--- a/tests/newset.spec.ts
+++ b/tests/newset.spec.ts
@@ -3,4 +3,5 @@ import { test, expect } from '@playwright/test';
 test('newset', async ({ page }) => {
+  test.setTimeout(120000);
   await page.goto('/newset');
   await expect(page.locator('#t')).toBeVisible();
 });
";
    let out = run_with_diff("setto-new-high", diff);
    assert_finding(&out, "tests/newset.spec.ts", "timeout-raised");
}

#[test]
fn new_set_timeout_below_default_is_clean() {
    let diff = "\
diff --git a/tests/newset.spec.ts b/tests/newset.spec.ts
index 1111111..2222222 100644
--- a/tests/newset.spec.ts
+++ b/tests/newset.spec.ts
@@ -3,4 +3,5 @@ import { test, expect } from '@playwright/test';
 test('newset', async ({ page }) => {
+  test.setTimeout(10000);
   await page.goto('/newset');
   await expect(page.locator('#t')).toBeVisible();
 });
";
    let out = run_with_diff("setto-new-low", diff);
    assert_exit(&out, 0);
}

#[test]
fn rename_spec_to_non_spec_removing_expect_is_reported() {
    let diff = "\
diff --git a/tests/a.spec.ts b/tests/a.ts
similarity 80%
rename from tests/a.spec.ts
rename to tests/a.ts
index 1111111..2222222 100644
--- a/tests/a.spec.ts
+++ b/tests/a.ts
@@ -3,5 +3,4 @@ import { test, expect } from '@playwright/test';
 test('a', async ({ page }) => {
   await page.goto('/a');
-  await expect(page.locator('#a')).toBeVisible();
 });
";
    let out = run_with_diff("rename", diff);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    assert!(
        stdout(&out).lines().any(|l| l.contains("expect-removed")),
        "expected an expect-removed finding\n{}",
        describe(&out)
    );
}

#[test]
fn binary_spec_diff_exits_2() {
    let diff = "\
diff --git a/tests/x.spec.ts b/tests/x.spec.ts
index 1111111..2222222 100644
Binary files a/tests/x.spec.ts and b/tests/x.spec.ts differ
";
    let out = run_with_diff("binary", diff);
    assert_exit(&out, 2);
}

fn skip_diff_for(path: &str) -> String {
    format!(
        "\
diff --git a/{path} b/{path}
index 1111111..2222222 100644
--- a/{path}
+++ b/{path}
@@ -1,4 +1,4 @@
 import {{ test, expect }} from '@playwright/test';

-test('x works', async ({{ page }}) => {{
+test.skip('x works', async ({{ page }}) => {{
   await expect(page.locator('#x')).toBeVisible();
"
    )
}

#[test]
fn spec_jsx_is_inspected() {
    let out = run_with_diff("jsx", &skip_diff_for("tests/x.spec.jsx"));
    assert_finding(&out, "tests/x.spec.jsx", "skip-added");
}

#[test]
fn spec_mts_is_inspected() {
    let out = run_with_diff("mts", &skip_diff_for("tests/x.spec.mts"));
    assert_finding(&out, "tests/x.spec.mts", "skip-added");
}

#[test]
fn spec_cts_is_inspected() {
    let out = run_with_diff("cts", &skip_diff_for("tests/x.spec.cts"));
    assert_finding(&out, "tests/x.spec.cts", "skip-added");
}

fn unique_dir(tag: &str) -> PathBuf {
    let p = unique_path(tag).with_extension("d");
    std::fs::create_dir_all(&p).expect("mkdir temp dir");
    p
}

fn run_staged_in(dir: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_webqa"))
        .arg("weaken-check")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().expect("parent"))
        .output()
        .expect("spawn webqa")
}

fn git_in(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn staged_mode_outside_git_repo_exits_2() {
    let dir = unique_dir("nogit");
    let out = run_staged_in(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_exit(&out, 2);
}

#[test]
fn staged_mode_reports_only_added_in_staged_spec() {
    let dir = unique_dir("staged");
    git_in(&dir, &["init", "-q"]);
    git_in(&dir, &["config", "user.email", "t@example.com"]);
    git_in(&dir, &["config", "user.name", "t"]);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("tests/s.spec.ts"),
        "import { test, expect } from '@playwright/test';\ntest('s', async ({ page }) => {\n  await expect(page.locator('#s')).toBeVisible();\n});\n",
    )
    .unwrap();
    git_in(&dir, &["add", "-A"]);
    git_in(&dir, &["commit", "-q", "--no-verify", "-m", "init"]);
    std::fs::write(
        dir.join("tests/s.spec.ts"),
        "import { test, expect } from '@playwright/test';\ntest.only('s', async ({ page }) => {\n  await expect(page.locator('#s')).toBeVisible();\n});\n",
    )
    .unwrap();
    git_in(&dir, &["add", "tests/s.spec.ts"]);
    let out = run_staged_in(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_finding(&out, "tests/s.spec.ts", "only-added");
}

// ------------------------------------------------ verifier-observed fail-opens

fn assert_not_zero(out: &Output) {
    let c = out.status.code();
    assert!(
        c == Some(1) || c == Some(2),
        "must never be exit 0\n{}",
        describe(out)
    );
}

#[test]
fn w1_path_with_space_and_trailing_tab_header_detects_only() {
    let diff = "diff --git a/e2e/my login.spec.ts b/e2e/my login.spec.ts\n\
index 1111111..2222222 100644\n\
--- a/e2e/my login.spec.ts\t\n\
+++ b/e2e/my login.spec.ts\t\n\
@@ -1,3 +1,3 @@\n\
 import { test } from '@playwright/test';\n\
-test('x', async () => {});\n\
+test.only('x', async () => {});\n";
    let out = run_with_diff("w1-tab", diff);
    assert_exit(&out, 1);
    assert!(stdout(&out).contains("only-added"), "{}", describe(&out));
}

#[test]
fn w1_quoted_path_header_is_never_exit_zero() {
    let diff = "diff --git \"a/e2e/q\\\"x.spec.ts\" \"b/e2e/q\\\"x.spec.ts\"\n\
index 1111111..2222222 100644\n\
--- \"a/e2e/q\\\"x.spec.ts\"\n\
+++ \"b/e2e/q\\\"x.spec.ts\"\n\
@@ -1,2 +1,2 @@\n\
 import { test } from '@playwright/test';\n\
-test('x', async () => {});\n\
+test.only('x', async () => {});\n";
    let out = run_with_diff("w1-quoted", diff);
    assert_not_zero(&out);
}

fn one_line_change(old: &str, new: &str) -> String {
    format!(
        "diff --git a/tests/t.spec.ts b/tests/t.spec.ts\nindex 1111111..2222222 100644\n--- a/tests/t.spec.ts\n+++ b/tests/t.spec.ts\n@@ -1,3 +1,3 @@\n import {{ test, expect }} from '@playwright/test';\n-{old}\n+{new}\n"
    )
}

#[test]
fn w2_timeout_option_set_to_zero_is_timeout_raised() {
    let d = one_line_change(
        "  await expect(page.locator('#a')).toBeVisible({ timeout: 5000 });",
        "  await expect(page.locator('#a')).toBeVisible({ timeout: 0 });",
    );
    let out = run_with_diff("w2-opt", &d);
    assert_finding(&out, "tests/t.spec.ts", "timeout-raised");
}

#[test]
fn w2_set_timeout_zero_is_timeout_raised() {
    let d = one_line_change("test.setTimeout(5000);", "test.setTimeout(0);");
    let out = run_with_diff("w2-set", &d);
    assert_finding(&out, "tests/t.spec.ts", "timeout-raised");
}

#[test]
fn w3_computed_timeout_expression_is_never_exit_zero() {
    let d = one_line_change(
        "  await expect(page.locator('#a')).toBeVisible({ timeout: 5000 });",
        "  await expect(page.locator('#a')).toBeVisible({ timeout: (5 * 60_000) });",
    );
    let out = run_with_diff("w3-expr", &d);
    assert_not_zero(&out);
}

#[test]
fn w3_underscore_literal_is_evaluated() {
    let d = one_line_change(
        "  await expect(page.locator('#a')).toBeVisible({ timeout: 5000 });",
        "  await expect(page.locator('#a')).toBeVisible({ timeout: 60_000 });",
    );
    let out = run_with_diff("w3-us", &d);
    assert_finding(&out, "tests/t.spec.ts", "timeout-raised");
}

#[test]
fn w4_test_slow_added_is_reported() {
    let d = "diff --git a/tests/t.spec.ts b/tests/t.spec.ts\nindex 1111111..2222222 100644\n--- a/tests/t.spec.ts\n+++ b/tests/t.spec.ts\n@@ -1,2 +1,3 @@\n test('x', async () => {\n+  test.slow();\n   await page.goto('/');\n"
        .to_string();
    let out = run_with_diff("w4-slow", &d);
    assert_exit(&out, 1);
    let so = stdout(&out).to_lowercase();
    assert!(
        so.contains("slow") || so.contains("timeout"),
        "{}",
        describe(&out)
    );
}
