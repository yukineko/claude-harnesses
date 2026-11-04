// sitecraft capability probe / check capability — bundled Playwright runner
// for `builtin:threejs-render` (rulings D3, D4).
//
// Usage: node capability-threejs.mjs '<input JSON>'
//   {"mode":"probe","timeout_ms":N}
//   {"mode":"check","url":"http://127.0.0.1:5173/viewer.html","selector":"canvas","timeout_ms":N}
// cwd:   the web-app package dir (resolved by sitecraft exactly like
//        `check pages`); Playwright is resolved from its node_modules and
//        those of its ancestors.
//
// stdout carries exactly one JSON object (schema sitecraft-capability-threejs/1):
//   {"schema", "status": "ok" | "not-visible" | "playwright-missing" | "launch-failed" | "error",
//    "webgl": "webgl2" | "webgl" | null      (probe, status ok),
//    "png_base64": "<screenshot PNG>"        (check, status ok),
//    "error": "<why>"                         (status other than ok)}
// exit 0 = ok or not-visible (a conclusion), 2 = anything else. sitecraft
// judges the screenshot (D4 pixel rule) itself; this script never decides
// "drawn".
import { createRequire } from 'node:module';
import path from 'node:path';

const SCHEMA = 'sitecraft-capability-threejs/1';
// The same Chromium launch for the probe and the check (rulings D3). Software
// GL keeps headless WebGL available without a GPU.
const LAUNCH = { headless: true, args: ['--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] };
const NAV_TIMEOUT_MS = 30_000;
const VISIBLE_TIMEOUT_MS = 30_000;
const SETTLE_MS = 2_000;

let emitted = false;

function emit(status, extra) {
  if (emitted) return;
  emitted = true;
  const out = { schema: SCHEMA, status, ...(extra || {}) };
  const code = status === 'ok' || status === 'not-visible' ? 0 : 2;
  process.stdout.write(JSON.stringify(out) + '\n', () => process.exit(code));
}

function errText(e) {
  return e && typeof e.message === 'string' ? e.message : String(e);
}

function isTimeout(e, playwright) {
  if (e && e.name === 'TimeoutError') return true;
  const T = playwright && playwright.errors && playwright.errors.TimeoutError;
  return typeof T === 'function' && e instanceof T;
}

function parseInput() {
  const raw = process.argv[2];
  if (process.argv.length !== 3 || typeof raw !== 'string') {
    throw new Error('expected exactly one argument: the input JSON');
  }
  const input = JSON.parse(raw);
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new Error('input is not a JSON object');
  if (input.mode !== 'probe' && input.mode !== 'check') throw new Error('input.mode must be "probe" or "check"');
  if (!Number.isInteger(input.timeout_ms) || input.timeout_ms <= 0) throw new Error('input.timeout_ms is not a positive integer');
  if (input.mode === 'check') {
    if (typeof input.url !== 'string' || input.url === '') throw new Error('input.url is not a non-empty string');
    if (typeof input.selector !== 'string' || input.selector === '') throw new Error('input.selector is not a non-empty string');
  }
  return input;
}

// Runs in the page (about:blank): which WebGL context can a canvas get?
function webglKind() {
  const a = document.createElement('canvas');
  if (a.getContext('webgl2')) return 'webgl2';
  const b = document.createElement('canvas');
  if (b.getContext('webgl')) return 'webgl';
  return null;
}

async function probe(browser) {
  const page = await browser.newPage();
  try {
    const kind = await page.evaluate(webglKind);
    return ['ok', { webgl: kind === 'webgl2' || kind === 'webgl' ? kind : null }];
  } finally {
    await page.close().catch(() => {});
  }
}

async function check(browser, input, playwright) {
  const page = await browser.newPage();
  try {
    page.setDefaultTimeout(VISIBLE_TIMEOUT_MS);
    try {
      await page.goto(input.url, { waitUntil: 'load', timeout: NAV_TIMEOUT_MS });
    } catch (e) {
      return ['error', { error: `opening ${input.url} failed: ${errText(e)}` }];
    }
    let handle;
    try {
      handle = await page.waitForSelector(input.selector, { state: 'visible', timeout: VISIBLE_TIMEOUT_MS });
    } catch (e) {
      if (isTimeout(e, playwright)) {
        return [
          'not-visible',
          { error: `${input.selector} did not become visible on ${input.url} within ${VISIBLE_TIMEOUT_MS} ms` },
        ];
      }
      return ['error', { error: `waiting for ${input.selector} failed: ${errText(e)}` }];
    }
    if (!handle) {
      return ['not-visible', { error: `${input.selector} was not found on ${input.url}` }];
    }
    await page.waitForTimeout(SETTLE_MS);
    const png = await handle.screenshot({ type: 'png' });
    return ['ok', { png_base64: Buffer.from(png).toString('base64') }];
  } finally {
    await page.close().catch(() => {});
  }
}

async function main() {
  let input;
  try {
    input = parseInput();
  } catch (e) {
    return emit('error', { error: `bad input: ${errText(e)}` });
  }
  // Whole-run ceiling (sitecraft also kills the process group at its limit).
  setTimeout(() => emit('error', { error: 'timeout' }), input.timeout_ms);

  let playwright;
  try {
    playwright = createRequire(path.join(process.cwd(), '__sitecraft__.cjs'))('playwright');
  } catch (e) {
    if (e && e.code === 'MODULE_NOT_FOUND' && /playwright/.test(errText(e))) {
      return emit('playwright-missing', { error: `cannot resolve playwright from ${process.cwd()}: ${errText(e)}` });
    }
    return emit('error', { error: `loading playwright failed: ${errText(e)}` });
  }

  let browser;
  try {
    browser = await playwright.chromium.launch(LAUNCH);
  } catch (e) {
    return emit('launch-failed', { error: errText(e) });
  }

  let result;
  try {
    result = input.mode === 'probe' ? await probe(browser) : await check(browser, input, playwright);
  } catch (e) {
    result = ['error', { error: `unexpected failure: ${errText(e)}` }];
  } finally {
    await browser.close().catch(() => {});
  }
  return emit(...result);
}

main().catch((e) => emit('error', { error: `unexpected failure: ${errText(e)}` }));
