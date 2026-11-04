'use strict';
// Fake `playwright` for the sitecraft-capabilities spec tests
// (tests/spec_r3_probe.rs, tests/spec_r4_check_capability.rs).
//
// The tests copy this file to `<project root>/node_modules/playwright/index.js`,
// so it is what `createRequire(path.join(<root>, "<any>.cjs"))("playwright")`
// resolves to — the same resolution `check pages` uses (rulings D3). The real
// `node` runs sitecraft's own bundled script; only Playwright is faked.
//
// FAKE_PLAYWRIGHT_SCENARIO: path to a JSON scenario
//   {
//     "launch_error": "message",   // chromium.launch rejects with Error(message)
//     "webgl": "webgl2" | "webgl" | "none",
//                                  // which canvas contexts page.evaluate sees
//                                  // (webgl2 implies webgl is also available)
//     "visible": true | false,     // whether the awaited selector ever becomes visible
//     "screenshot": "/abs/file.png" // bytes returned by element/locator screenshot()
//   }
// FAKE_PLAYWRIGHT_LOG: path; one JSON line per call ({op, ..., t: Date.now()}).
//
// API surface (anything else throws "outside the contracted API surface"):
//   chromium.launch(opts) -> browser
//   browser.newPage(opts), browser.newContext(opts), browser.close(), browser.version()
//   context.newPage(), context.close()
//   page.goto(url, opts), page.waitForLoadState(state), page.waitForTimeout(ms) (really waits),
//   page.setDefaultTimeout(ms), page.setDefaultNavigationTimeout(ms), page.setViewportSize(s),
//   page.on(ev, fn) / page.off(ev, fn) (no events are ever emitted), page.url(), page.close(),
//   page.evaluate(fnOrString, arg) (runs in node with a fake `document` / `OffscreenCanvas`
//     whose canvas.getContext("webgl2"|"webgl"|"experimental-webgl") follows `webgl`),
//   page.waitForSelector(sel, opts) -> handle (rejects with errors.TimeoutError when not visible),
//   page.$(sel) -> handle | null, page.locator(sel) -> locator,
//   page.screenshot(opts) (whole page; returns the scenario PNG too),
//   locator.first(), locator.nth(i), locator.waitFor(opts), locator.isVisible(),
//   locator.screenshot(opts), locator.boundingBox(), locator.elementHandle(),
//   handle.screenshot(opts), handle.isVisible(), handle.boundingBox()
//   screenshot(opts): returns the PNG Buffer, and also writes it to opts.path if given.
//   errors.TimeoutError, devices = {}.

const fs = require('fs');

class TimeoutError extends Error {
  constructor(m) {
    super(m);
    this.name = 'TimeoutError';
  }
}

function scenario() {
  const p = process.env.FAKE_PLAYWRIGHT_SCENARIO;
  return p ? JSON.parse(fs.readFileSync(p, 'utf8')) : {};
}

function log(entry) {
  const p = process.env.FAKE_PLAYWRIGHT_LOG;
  if (p) fs.appendFileSync(p, JSON.stringify({ ...entry, t: Date.now() }) + '\n');
}

function strict(name, obj) {
  return new Proxy(obj, {
    get(t, prop, recv) {
      if (typeof prop === 'symbol' || prop in t) return Reflect.get(t, prop, recv);
      if (prop === 'then' || prop === 'toJSON') return undefined;
      throw new Error(
        `fake playwright: ${name}.${String(prop)} is outside the contracted API surface ` +
          '(see crates/sitecraft/tests/fixtures/spec/fake_playwright/index.js)'
      );
    },
  });
}

function shot(sc, what, opts) {
  log({ op: 'screenshot', what, opts: opts || null });
  if (!sc.screenshot) throw new Error('fake playwright: scenario has no screenshot');
  const buf = fs.readFileSync(sc.screenshot);
  if (opts && typeof opts.path === 'string') fs.writeFileSync(opts.path, buf);
  return buf;
}

function contexts(sc) {
  const mode = sc.webgl || 'none';
  const ok = (type) =>
    (type === 'webgl2' && mode === 'webgl2') ||
    ((type === 'webgl' || type === 'experimental-webgl') && (mode === 'webgl2' || mode === 'webgl'));
  const canvas = () => ({
    width: 300,
    height: 150,
    getContext(type) {
      log({ op: 'getContext', type });
      return ok(type) ? { type, getParameter: () => 'fake', getExtension: () => null } : null;
    },
  });
  return { canvas };
}

async function evaluate(sc, fn, arg) {
  log({ op: 'evaluate' });
  const { canvas } = contexts(sc);
  const saved = { document: globalThis.document, OffscreenCanvas: globalThis.OffscreenCanvas };
  globalThis.document = {
    createElement: (tag) => (String(tag).toLowerCase() === 'canvas' ? canvas() : {}),
    querySelector: () => null,
    body: { appendChild() {} },
  };
  globalThis.OffscreenCanvas = function OffscreenCanvas() {
    return canvas();
  };
  try {
    const f = typeof fn === 'function' ? fn : new Function(`return (${fn});`);
    const r = typeof fn === 'function' ? await f(arg) : await f();
    return typeof r === 'function' ? await r(arg) : r;
  } finally {
    globalThis.document = saved.document;
    globalThis.OffscreenCanvas = saved.OffscreenCanvas;
  }
}

function handle(sc, sel) {
  return strict('elementHandle', {
    screenshot: async (opts) => shot(sc, sel, opts),
    isVisible: async () => sc.visible !== false,
    boundingBox: async () => ({ x: 0, y: 0, width: 100, height: 100 }),
    dispose: async () => {},
  });
}

async function waitVisible(sc, sel, opts) {
  log({ op: 'waitForSelector', selector: sel, opts: opts || null });
  if (sc.visible === false) {
    throw new TimeoutError(`fake playwright: waiting for ${sel} to be visible timed out`);
  }
  log({ op: 'visible', selector: sel });
}

function locator(sc, sel) {
  const self = strict('locator', {
    first: () => self,
    nth: () => self,
    waitFor: async (opts) => waitVisible(sc, sel, opts),
    isVisible: async () => sc.visible !== false,
    screenshot: async (opts) => shot(sc, sel, opts),
    boundingBox: async () => ({ x: 0, y: 0, width: 100, height: 100 }),
    elementHandle: async () => handle(sc, sel),
  });
  return self;
}

function makePage(sc) {
  let current = 'about:blank';
  return strict('page', {
    async goto(url, opts) {
      log({ op: 'goto', url, opts: opts || null });
      current = url;
      return null;
    },
    async waitForLoadState(state) {
      log({ op: 'waitForLoadState', state: state || null });
    },
    async waitForTimeout(ms) {
      log({ op: 'waitForTimeout', ms });
      await new Promise((r) => setTimeout(r, ms));
    },
    setDefaultTimeout(ms) {
      log({ op: 'setDefaultTimeout', ms });
    },
    setDefaultNavigationTimeout() {},
    async setViewportSize() {},
    on() {},
    off() {},
    url: () => current,
    async close() {
      log({ op: 'page.close' });
    },
    evaluate: (fn, arg) => evaluate(sc, fn, arg),
    async waitForSelector(sel, opts) {
      await waitVisible(sc, sel, opts);
      return handle(sc, sel);
    },
    async $(sel) {
      log({ op: '$', selector: sel });
      return sc.visible === false ? null : handle(sc, sel);
    },
    locator: (sel) => {
      log({ op: 'locator', selector: sel });
      return locator(sc, sel);
    },
    screenshot: async (opts) => shot(sc, 'page', opts),
  });
}

const chromium = strict('chromium', {
  async launch(opts) {
    log({ op: 'launch', opts: opts || null });
    const sc = scenario();
    if (sc.launch_error) throw new Error(sc.launch_error);
    return strict('browser', {
      newPage: async () => makePage(sc),
      newContext: async () =>
        strict('context', {
          newPage: async () => makePage(sc),
          close: async () => log({ op: 'context.close' }),
        }),
      version: () => '0.0.0-fake',
      close: async () => log({ op: 'browser.close' }),
    });
  },
});

module.exports = { chromium, errors: { TimeoutError }, devices: {} };
