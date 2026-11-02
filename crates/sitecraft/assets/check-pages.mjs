// sitecraft check pages — bundled Playwright runner.
//
// Usage: node check-pages.mjs '{"base_url":"http://127.0.0.1:5173","pages":["/","/about"]}'
// cwd:   the project root (Playwright is resolved from its node_modules).
//
// Contract: the doc comment at the top of crates/sitecraft/tests/check_pages.rs.
// stdout carries exactly one JSON object (schema sitecraft-check-pages/1);
// exit 0 = every page checked (findings may be non-empty), 2 = could not
// check, 3 = browser missing, 4 = playwright missing. Findings are never
// signalled through the exit status.
import { createRequire } from 'node:module';
import path from 'node:path';

const SCHEMA = 'sitecraft-check-pages/1';
const EXIT = { ok: 0, error: 2, 'browser-missing': 3, 'playwright-missing': 4 };
const NAV_TIMEOUT_MS = 30_000;
const SETTLE_MS = 500;
const LINK_TIMEOUT_MS = 10_000;
// Whole-run ceiling: if anything hangs, emit `error` (exit 2) rather than
// hang the caller. Kept well inside the 600 s Stop-hook budget.
const GLOBAL_TIMEOUT_BASE_MS = 30_000;
const GLOBAL_TIMEOUT_PER_PAGE_MS = 45_000;
const GLOBAL_TIMEOUT_MAX_MS = 480_000;

let emitted = false;
let pagesChecked = 0;
const findings = [];

/** Write the single result object once and exit with the matching code. */
function emit(status, error) {
  if (emitted) return;
  emitted = true;
  const out = { schema: SCHEMA, status, pages_checked: pagesChecked, findings };
  if (status !== 'ok') out.error = String(error ?? status);
  process.stdout.write(JSON.stringify(out) + '\n', () => process.exit(EXIT[status] ?? 2));
}

function errText(e) {
  return e && typeof e.message === 'string' ? e.message : String(e);
}

function parseInput() {
  const raw = process.argv[2];
  if (process.argv.length !== 3 || typeof raw !== 'string') {
    throw new Error('expected exactly one argument: the input JSON {"base_url", "pages"}');
  }
  const input = JSON.parse(raw);
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new Error('input is not a JSON object');
  if (typeof input.base_url !== 'string' || input.base_url === '') throw new Error('input.base_url is not a non-empty string');
  if (!Array.isArray(input.pages) || !input.pages.every((p) => typeof p === 'string')) {
    throw new Error('input.pages is not an array of strings');
  }
  const origin = new URL(input.base_url).origin;
  return { base: input.base_url, origin, pages: input.pages };
}

function sameOrigin(url, origin) {
  try {
    return new URL(url).origin === origin;
  } catch {
    return false;
  }
}

function stripFragment(url) {
  const u = new URL(url);
  u.hash = '';
  return u.href;
}

async function checkPage(browser, pageUrl, origin) {
  const page = await browser.newPage();
  const add = (f) => findings.push({ page: pageUrl, ...f });
  try {
    page.setDefaultTimeout(NAV_TIMEOUT_MS);
    page.setDefaultNavigationTimeout(NAV_TIMEOUT_MS);
    // Listeners first: anything fired during goto must be observed.
    page.on('console', (msg) => {
      if (msg.type() === 'error') add({ kind: 'console-error', detail: String(msg.text()) });
    });
    page.on('pageerror', (err) => add({ kind: 'page-error', detail: errText(err) }));
    page.on('response', (res) => {
      const url = res.url();
      const status = res.status();
      if (status >= 400 && sameOrigin(url, origin)) {
        add({ kind: 'asset-http-error', detail: `GET ${url} -> ${status}`, url, status });
      }
    });
    page.on('requestfailed', (req) => {
      const url = req.url();
      if (sameOrigin(url, origin)) {
        const f = req.failure();
        const why = f && f.errorText ? f.errorText : 'request failed';
        add({ kind: 'asset-http-error', detail: `${req.method()} ${url} -> ${why}`, url, status: null });
      }
    });

    await page.goto(pageUrl, { waitUntil: 'load', timeout: NAV_TIMEOUT_MS });
    await page.waitForTimeout(SETTLE_MS);

    // A redirect off the configured origin would make every later failure
    // look cross-origin (and so be filtered out): that is itself a finding.
    // `page-error` is the closest of the four contracted kinds.
    let landed;
    try {
      landed = new URL(page.url());
    } catch (e) {
      add({ kind: 'page-error', detail: `final page URL ${JSON.stringify(page.url())} is not a URL: ${errText(e)}` });
      return;
    }
    if (landed.origin !== origin) {
      add({
        kind: 'page-error',
        detail: `navigated off the configured origin ${origin}: ${pageUrl} ended at ${landed.href}`,
      });
      return;
    }

    const hrefs = await page.$$eval('a[href]', (els) => els.map((e) => e.href));
    const links = [...new Set(hrefs.filter((h) => typeof h === 'string' && sameOrigin(h, origin)).map(stripFragment))];
    for (const link of links) {
      try {
        const res = await page.request.get(link, { timeout: LINK_TIMEOUT_MS });
        const status = res.status();
        await res.dispose();
        if (status >= 400) add({ kind: 'broken-link', detail: `GET ${link} -> ${status}`, url: link, status });
      } catch (e) {
        add({ kind: 'broken-link', detail: `GET ${link} -> ${errText(e)}`, url: link, status: null });
      }
    }
  } finally {
    await page.close().catch(() => {});
  }
}

async function main() {
  let input;
  try {
    input = parseInput();
  } catch (e) {
    return emit('error', `bad input: ${errText(e)}`);
  }
  if (input.pages.length === 0) return emit('error', 'no pages to check (sitecraft.toml `pages` is empty)');

  // Every entry must stay on the configured origin; an absolute or
  // protocol-relative entry to another host would be checked with all of its
  // failures filtered out as cross-origin.
  const pageUrls = [];
  for (const p of input.pages) {
    let u;
    try {
      u = new URL(p, input.base);
    } catch (e) {
      return emit('error', `pages entry ${JSON.stringify(p)} is not a valid URL path: ${errText(e)}`);
    }
    if (u.origin !== input.origin) {
      return emit(
        'error',
        `pages entry ${JSON.stringify(p)} resolves to ${u.href}, outside the configured origin ${input.origin}; ` +
          'use a same-origin path such as "/about" in sitecraft.toml `pages`'
      );
    }
    pageUrls.push(u.href);
  }

  // Whole-run ceiling, armed before anything that can hang. Never unref'd:
  // it must be able to fire while an await is stuck.
  const timeoutMs = Math.min(
    GLOBAL_TIMEOUT_BASE_MS + GLOBAL_TIMEOUT_PER_PAGE_MS * input.pages.length,
    GLOBAL_TIMEOUT_MAX_MS
  );
  setTimeout(() => emit('error', `global timeout of ${timeoutMs} ms exceeded`), timeoutMs);

  let playwright;
  try {
    playwright = createRequire(path.join(process.cwd(), '__sitecraft__.cjs'))('playwright');
  } catch (e) {
    if (e && e.code === 'MODULE_NOT_FOUND' && /playwright/.test(errText(e))) {
      return emit('playwright-missing', `cannot resolve playwright from ${process.cwd()}: ${errText(e)}`);
    }
    return emit('error', `loading playwright failed: ${errText(e)}`);
  }

  let browser;
  try {
    browser = await playwright.chromium.launch({ headless: true });
  } catch (e) {
    const msg = errText(e);
    return emit(/Executable doesn't exist/.test(msg) ? 'browser-missing' : 'error', msg);
  }

  // The result is decided inside the try and emitted only after the browser
  // is closed (emit exits the process).
  let result = ['ok'];
  try {
    for (const pageUrl of pageUrls) {
      try {
        await checkPage(browser, pageUrl, input.origin);
      } catch (e) {
        result = ['error', `checking ${pageUrl} failed: ${errText(e)}`];
        break;
      }
      pagesChecked += 1;
    }
  } catch (e) {
    result = ['error', `unexpected failure: ${errText(e)}`];
  } finally {
    await browser.close().catch(() => {});
  }
  return emit(...result);
}

main().catch((e) => emit('error', `unexpected failure: ${errText(e)}`));
