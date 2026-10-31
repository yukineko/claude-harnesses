// Origin-escape tests for the bundled crates/sitecraft/assets/check-pages.mjs.
// Run: node --test crates/sitecraft/assets/tests/check-pages-origin.test.mjs
//
// Two ways a checked page can leave the configured origin, after which every
// failure on it is filtered out as "cross-origin" and the run would read as
// clean:
//   A. a `pages` entry that is itself an absolute / protocol-relative URL to
//      another origin;
//   B. the page redirects (final `page.url()` after goto) to another origin.
// Neither may produce a clean result: the script must report a finding
// (exit 0, status ok, findings non-empty) or refuse (exit 2, status error).
// Only the clean outcome is rejected.
//
// Redirects are simulated by a thin wrapper around ./fake_modules/playwright,
// generated per run into a temp dir and put on NODE_PATH: it overrides
// `page.url()` to return `scenario.redirects[<goto url>]` (default: the goto
// url) once goto has resolved. Everything else is the shared fake.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.resolve(HERE, '..', 'check-pages.mjs');
const SHARED_FAKE = path.join(HERE, 'fake_modules', 'playwright', 'index.js');
const SCHEMA = 'sitecraft-check-pages/1';
const BASE = 'http://127.0.0.1:5173';
const ROOT = `${BASE}/`;
const OTHER = 'http://other.example/x';

const WRAPPER = `'use strict';
const fake = require(${JSON.stringify(SHARED_FAKE)});
const fs = require('fs');
const p = process.env.FAKE_PLAYWRIGHT_SCENARIO;
const sc = p ? JSON.parse(fs.readFileSync(p, 'utf8')) : {};
const redirects = sc.redirects || {};
function wrapPage(inner) {
  let landed = null;
  return new Proxy(inner, {
    get(t, prop) {
      if (prop === 'url') return () => (landed === null ? t.url() : landed);
      if (prop === 'goto') {
        return async (url, opts) => {
          const r = await t.goto(url, opts);
          landed = Object.prototype.hasOwnProperty.call(redirects, url) ? redirects[url] : url;
          return r;
        };
      }
      return Reflect.get(t, prop);
    },
  });
}
function wrapBrowser(b) {
  return new Proxy(b, {
    get(t, prop) {
      if (prop === 'newPage') return async () => wrapPage(await t.newPage());
      if (prop === 'newContext') {
        return async () => {
          const ctx = await t.newContext();
          return new Proxy(ctx, {
            get(c, q) {
              if (q === 'newPage') return async () => wrapPage(await c.newPage());
              return Reflect.get(c, q);
            },
          });
        };
      }
      return Reflect.get(t, prop);
    },
  });
}
module.exports = {
  chromium: { launch: async (opts) => wrapBrowser(await fake.chromium.launch(opts)) },
};
`;

function run(scenario, input) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sitecraft-check-pages-origin-'));
  const mods = path.join(dir, 'mods');
  fs.mkdirSync(path.join(mods, 'playwright'), { recursive: true });
  fs.writeFileSync(path.join(mods, 'playwright', 'index.js'), WRAPPER);
  const scenarioPath = path.join(dir, 'scenario.json');
  const logPath = path.join(dir, 'calls.jsonl');
  fs.writeFileSync(scenarioPath, JSON.stringify(scenario));
  const cwd = path.join(dir, 'project');
  fs.mkdirSync(cwd);
  const env = { ...process.env, FAKE_PLAYWRIGHT_SCENARIO: scenarioPath, FAKE_PLAYWRIGHT_LOG: logPath };
  delete env.NODE_OPTIONS;
  env.NODE_PATH = mods;
  const r = spawnSync(process.execPath, [SCRIPT, JSON.stringify(input)], {
    cwd,
    env,
    encoding: 'utf8',
    timeout: 30_000,
  });
  const calls = fs.existsSync(logPath)
    ? fs.readFileSync(logPath, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l))
    : [];
  fs.rmSync(dir, { recursive: true, force: true });
  let json;
  try {
    json = JSON.parse(r.stdout);
  } catch {
    json = undefined;
  }
  return { status: r.status, stdout: r.stdout, stderr: r.stderr, json, calls };
}

function describe(r) {
  return `exit=${r.status}\nstdout=${r.stdout}\nstderr=${r.stderr}`;
}

const page = (extra = {}) => ({ status: 200, events: [], links: [], ...extra });

/**
 * Accept exactly two outcomes: a finding (exit 0, status ok, findings
 * non-empty) or a refusal (exit 2, status error, not caused by the fake
 * lacking a scenario). Reject everything else, in particular clean.
 */
function assertNotClean(r) {
  assert.ok(r.json, `stdout must be one JSON object\n${describe(r)}`);
  assert.equal(r.json.schema, SCHEMA, describe(r));
  const clean = r.status === 0 && r.json.status === 'ok' && Array.isArray(r.json.findings) && r.json.findings.length === 0;
  assert.ok(!clean, `left the configured origin but the run is CLEAN (fail-open)\n${describe(r)}`);
  const finding = r.status === 0 && r.json.status === 'ok' && Array.isArray(r.json.findings) && r.json.findings.length > 0;
  const refusal = r.status === 2 && r.json.status === 'error';
  assert.ok(finding || refusal, `expected a finding (exit 0) or an error (exit 2)\n${describe(r)}`);
  if (refusal) {
    assert.doesNotMatch(String(r.json.error), /no scenario for/, `error came from the fake, not the script\n${describe(r)}`);
  }
}

// ------------------------------------------------- A: off-origin entries ----

test('A: absolute off-origin pages entry whose document is 503 is not clean', () => {
  const r = run({ pages: { [OTHER]: page({ status: 503 }) } }, { base_url: BASE, pages: [OTHER] });
  assertNotClean(r);
});

test('A: protocol-relative off-origin pages entry whose document is 503 is not clean', () => {
  const r = run({ pages: { [OTHER]: page({ status: 503 }) } }, { base_url: BASE, pages: ['//other.example/x'] });
  assertNotClean(r);
});

test('A: off-origin entry next to a clean same-origin page is not clean', () => {
  const r = run(
    { pages: { [ROOT]: page(), [OTHER]: page({ status: 503 }) } },
    { base_url: BASE, pages: ['/', OTHER] }
  );
  assertNotClean(r);
});

test('A control: same-origin entry whose document is 503 is an asset-http-error finding', () => {
  const r = run({ pages: { [`${BASE}/x`]: page({ status: 503 }) } }, { base_url: BASE, pages: ['/x'] });
  assert.equal(r.status, 0, describe(r));
  assert.equal(r.json.status, 'ok', describe(r));
  assert.equal(r.json.pages_checked, 1, describe(r));
  const f = r.json.findings.filter((x) => x.kind === 'asset-http-error' && x.status === 503);
  assert.equal(f.length, 1, describe(r));
});

// --------------------------------------------- B: cross-origin redirects ----

test('B: base page redirecting to another host (503 there) is not clean', () => {
  const r = run(
    {
      redirects: { [ROOT]: 'http://evil.example/' },
      pages: {
        [ROOT]: page({
          events: [
            { type: 'response', url: 'http://evil.example/', status: 503, resource_type: 'document' },
            { type: 'requestfailed', url: 'http://evil.example/app.js', error: 'net::ERR_FAILED' },
          ],
        }),
      },
    },
    { base_url: BASE, pages: ['/'] }
  );
  // The redirect wrapper is in effect (otherwise this test proves nothing).
  assert.ok(r.calls.some((c) => c.op === 'goto' && c.url === ROOT), describe(r));
  assertNotClean(r);
});

test('B: localhost base landing on 127.0.0.1 (same port, different origin) is not clean', () => {
  const base = 'http://localhost:5173';
  const r = run(
    {
      redirects: { [`${base}/`]: 'http://127.0.0.1:5173/' },
      pages: { [`${base}/`]: page() },
    },
    { base_url: base, pages: ['/'] }
  );
  assertNotClean(r);
});

test('B control: same-origin redirect (path change only) stays clean', () => {
  const r = run(
    {
      redirects: { [ROOT]: `${BASE}/home` },
      pages: { [ROOT]: page() },
    },
    { base_url: BASE, pages: ['/'] }
  );
  assert.equal(r.status, 0, describe(r));
  assert.ok(r.json, describe(r));
  assert.equal(r.json.schema, SCHEMA);
  assert.equal(r.json.status, 'ok', describe(r));
  assert.equal(r.json.pages_checked, 1, describe(r));
  assert.deepEqual(r.json.findings, [], describe(r));
});
