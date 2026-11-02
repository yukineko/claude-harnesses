// Tests for the bundled crates/sitecraft/assets/check-pages.mjs.
// Run: node --test crates/sitecraft/assets/tests/
//
// The script is run as a child process (`node check-pages.mjs <input-json>`)
// with a fake `playwright` (./fake_modules/playwright) on NODE_PATH; real
// Playwright is never used. The contract being tested is the doc comment at
// the top of crates/sitecraft/tests/check_pages.rs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.resolve(HERE, '..', 'check-pages.mjs');
const FAKE_MODULES = path.join(HERE, 'fake_modules');
const SCHEMA = 'sitecraft-check-pages/1';
const BASE = 'http://127.0.0.1:5173';
const ROOT = `${BASE}/`;
const ABOUT = `${BASE}/about`;
const KINDS = new Set(['console-error', 'page-error', 'asset-http-error', 'broken-link']);

function run(scenario, input, { fakePlaywright = true } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sitecraft-check-pages-'));
  const scenarioPath = path.join(dir, 'scenario.json');
  const logPath = path.join(dir, 'calls.jsonl');
  fs.writeFileSync(scenarioPath, JSON.stringify(scenario));
  const env = { ...process.env, FAKE_PLAYWRIGHT_SCENARIO: scenarioPath, FAKE_PLAYWRIGHT_LOG: logPath };
  delete env.NODE_PATH;
  delete env.NODE_OPTIONS;
  if (fakePlaywright) env.NODE_PATH = FAKE_MODULES;
  const r = spawnSync(process.execPath, [SCRIPT, JSON.stringify(input)], {
    cwd: dir,
    env,
    encoding: 'utf8',
    timeout: 30_000,
  });
  const calls = fs.existsSync(logPath)
    ? fs.readFileSync(logPath, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l))
    : [];
  fs.rmSync(dir, { recursive: true, force: true });
  let json;
  let parseError;
  try {
    json = JSON.parse(r.stdout);
  } catch (e) {
    parseError = e;
  }
  return { status: r.status, stdout: r.stdout, stderr: r.stderr, json, parseError, calls };
}

function describe(r) {
  return `exit=${r.status}\nstdout=${r.stdout}\nstderr=${r.stderr}`;
}

/** Exit 0, schema-valid `ok` output. Returns the findings. */
function okResult(r, pagesChecked) {
  assert.equal(r.status, 0, describe(r));
  assert.ok(r.json, `stdout must be exactly one JSON object: ${r.parseError}\n${describe(r)}`);
  assert.equal(r.json.schema, SCHEMA);
  assert.equal(r.json.status, 'ok');
  assert.equal(r.json.pages_checked, pagesChecked);
  assert.ok(Array.isArray(r.json.findings), describe(r));
  for (const f of r.json.findings) {
    assert.equal(typeof f.page, 'string');
    assert.ok(KINDS.has(f.kind), `unknown kind ${f.kind}`);
    assert.equal(typeof f.detail, 'string');
  }
  return r.json.findings;
}

const page = (extra = {}) => ({ status: 200, events: [], links: [], ...extra });
const twoPages = (root = {}, about = {}) => ({ pages: { [ROOT]: page(root), [ABOUT]: page(about) } });
const INPUT = { base_url: BASE, pages: ['/', '/about'] };

test('the bundled check-pages.mjs exists', () => {
  assert.ok(fs.existsSync(SCRIPT), `missing ${SCRIPT}`);
});

test('clean pages: exit 0, every page checked, no findings, browser closed', () => {
  const r = run(twoPages(), INPUT);
  assert.deepEqual(okResult(r, 2), []);
  const gotos = r.calls.filter((c) => c.op === 'goto').map((c) => c.url);
  assert.deepEqual(gotos, [ROOT, ABOUT]);
  assert.ok(r.calls.some((c) => c.op === 'browser.close'), 'browser must be closed');
});

test('listeners are registered before goto', () => {
  const r = run(twoPages(), INPUT);
  okResult(r, 2);
  const gotos = r.calls.map((c, i) => ({ c, i })).filter(({ c }) => c.op === 'goto');
  assert.equal(gotos.length, 2, describe(r));
  for (const { c: g, i } of gotos) {
    const before = r.calls.slice(0, i).filter((c) => c.op === 'on' && c.page === g.page).map((c) => c.event);
    for (const ev of ['console', 'pageerror', 'response']) {
      assert.ok(before.includes(ev), `'${ev}' listener not registered on page ${g.page} before goto ${g.url}`);
    }
  }
});

test('console error during navigation is a console-error finding; other console types are not', () => {
  const r = run(
    twoPages({}, {
      events: [
        { type: 'console', level: 'warning', text: 'just a warning' },
        { type: 'console', level: 'log', text: 'hello' },
        { type: 'console', level: 'error', text: 'Uncaught TypeError: boom' },
      ],
    }),
    INPUT
  );
  const f = okResult(r, 2);
  assert.equal(f.length, 1, describe(r));
  assert.equal(f[0].kind, 'console-error');
  assert.equal(f[0].page, ABOUT);
  assert.match(f[0].detail, /boom/);
});

test('pageerror is a page-error finding', () => {
  const r = run(twoPages({ events: [{ type: 'pageerror', message: 'ReferenceError: foo is not defined' }] }), INPUT);
  const f = okResult(r, 2);
  assert.equal(f.length, 1, describe(r));
  assert.equal(f[0].kind, 'page-error');
  assert.equal(f[0].page, ROOT);
  assert.match(f[0].detail, /foo is not defined/);
});

test('same-origin 4xx/5xx assets are findings; other-origin 4xx/5xx are not', () => {
  const r = run(
    twoPages({
      events: [
        { type: 'response', url: `${BASE}/assets/app.js`, status: 404, resource_type: 'script' },
        { type: 'response', url: `${BASE}/api/data`, status: 500, resource_type: 'fetch' },
        { type: 'response', url: 'https://cdn.example.com/lib.js', status: 404, resource_type: 'script' },
        { type: 'response', url: 'http://127.0.0.1:9999/other-port.css', status: 403, resource_type: 'stylesheet' },
        { type: 'response', url: `${BASE}/ok.css`, status: 200, resource_type: 'stylesheet' },
      ],
    }),
    INPUT
  );
  const f = okResult(r, 2);
  const urls = f.filter((x) => x.kind === 'asset-http-error').map((x) => [x.url, x.status, x.page]);
  assert.deepEqual(
    urls.sort(),
    [
      [`${BASE}/api/data`, 500, ROOT],
      [`${BASE}/assets/app.js`, 404, ROOT],
    ],
    describe(r)
  );
  assert.equal(f.length, 2, `cross-origin responses must not be findings: ${describe(r)}`);
});

test('a same-origin page document 404 is an asset-http-error finding', () => {
  const r = run(twoPages({}, { status: 404 }), INPUT);
  const f = okResult(r, 2);
  assert.deepEqual(
    f.map((x) => [x.kind, x.page, x.status]),
    [['asset-http-error', ABOUT, 404]],
    describe(r)
  );
});

test('same-origin broken links are findings; other-origin links are not requested', () => {
  const r = run(
    {
      ...twoPages({
        links: [`${BASE}/about`, `${BASE}/missing`, `${BASE}/missing#frag`, `${BASE}/gone`, 'https://other.example/x'],
      }),
      requests: { [`${BASE}/missing`]: 404, [`${BASE}/gone`]: 'throw', 'https://other.example/x': 404 },
    },
    INPUT
  );
  const f = okResult(r, 2);
  const broken = f.filter((x) => x.kind === 'broken-link');
  assert.deepEqual(
    broken.map((x) => [x.page, x.url]).sort(),
    [
      [ROOT, `${BASE}/gone`],
      [ROOT, `${BASE}/missing`],
    ],
    describe(r)
  );
  assert.equal(f.length, 2, describe(r));
  // Compare parsed origins, not a URL prefix: a prefix check misses the same
  // host under another scheme or spelling.
  const requests = r.calls.filter((c) => c.op === 'request');
  assert.ok(requests.length > 0, `same-origin links must be requested: ${describe(r)}`);
  assert.deepEqual(
    requests.filter((c) => new URL(c.url).origin !== new URL(BASE).origin).map((c) => c.url),
    [],
    'cross-origin links must not be requested'
  );
});

test('zero pages: non-zero exit (2, status error), never ok', () => {
  const r = run({ pages: {} }, { base_url: BASE, pages: [] });
  assert.notEqual(r.status, 0, describe(r));
  assert.equal(r.status, 2, describe(r));
  assert.ok(r.json, describe(r));
  assert.equal(r.json.schema, SCHEMA);
  assert.equal(r.json.status, 'error');
  assert.equal(r.json.pages_checked, 0);
});

test('navigation failure: exit 2, status error, browser still closed', () => {
  const r = run(twoPages({}, { goto_error: 'net::ERR_CONNECTION_REFUSED' }), INPUT);
  assert.equal(r.status, 2, describe(r));
  assert.ok(r.json, describe(r));
  assert.equal(r.json.status, 'error');
  assert.ok(r.calls.some((c) => c.op === 'browser.close'), 'browser must be closed in finally');
});

test('missing browser executable: exit 3, status browser-missing', () => {
  const r = run(
    { launch_error: "browserType.launch: Executable doesn't exist at /x/Chromium\nRun: npx playwright install", pages: {} },
    INPUT
  );
  assert.equal(r.status, 3, describe(r));
  assert.ok(r.json, describe(r));
  assert.equal(r.json.schema, SCHEMA);
  assert.equal(r.json.status, 'browser-missing');
  assert.match(r.json.error, /Executable doesn't exist/);
});

test('playwright not installed: exit 4, status playwright-missing', () => {
  const r = run(twoPages(), INPUT, { fakePlaywright: false });
  assert.equal(r.status, 4, describe(r));
  assert.ok(r.json, describe(r));
  assert.equal(r.json.schema, SCHEMA);
  assert.equal(r.json.status, 'playwright-missing');
});
