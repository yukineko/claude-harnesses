'use strict';
// Fake `playwright` for crates/sitecraft/assets/tests/check-pages.test.mjs.
// Loaded by check-pages.mjs through NODE_PATH. Implements exactly the API
// surface listed in the contract at the top of
// crates/sitecraft/tests/check_pages.rs; any other property access throws.
//
// FAKE_PLAYWRIGHT_SCENARIO: path to a JSON scenario:
//   {
//     "launch_error": "message"            // chromium.launch rejects with it
//     "pages": { "<absolute page url>": {
//         "status": 200,                    // document response status
//         "goto_error": "message",          // page.goto rejects with it
//         "events": [                       // fired *during* goto, in order
//           {"type":"console","level":"error","text":"..."},
//           {"type":"pageerror","message":"..."},
//           {"type":"response","url":"...","status":404,"resource_type":"script"},
//           {"type":"requestfailed","url":"...","error":"net::ERR_FAILED"}
//         ],
//         "links": ["absolute href", ...]   // what a[href] elements report
//     }},
//     "requests": { "<url>": 404 | "throw" } // request.get/head/fetch result (default 200)
//   }
// FAKE_PLAYWRIGHT_LOG: path; one JSON line per call ({op, page, ...}).

const fs = require('fs');

function scenario() {
  const p = process.env.FAKE_PLAYWRIGHT_SCENARIO;
  return p ? JSON.parse(fs.readFileSync(p, 'utf8')) : { pages: {} };
}

function log(entry) {
  const p = process.env.FAKE_PLAYWRIGHT_LOG;
  if (p) fs.appendFileSync(p, JSON.stringify(entry) + '\n');
}

function strict(name, obj) {
  return new Proxy(obj, {
    get(t, prop, recv) {
      if (typeof prop === 'symbol' || prop in t) return Reflect.get(t, prop, recv);
      if (prop === 'then' || prop === 'toJSON') return undefined;
      throw new Error(
        `fake playwright: ${name}.${String(prop)} is outside the contracted API surface ` +
          '(see crates/sitecraft/tests/check_pages.rs)'
      );
    },
  });
}

function request(url, method, resourceType) {
  return strict('request', {
    url: () => url,
    method: () => method,
    resourceType: () => resourceType,
    failure: () => null,
  });
}

function response(url, status, resourceType) {
  return strict('response', {
    url: () => url,
    status: () => status,
    ok: () => status >= 200 && status < 300,
    statusText: () => String(status),
    request: () => request(url, 'GET', resourceType),
    headers: () => ({}),
  });
}

function apiRequest(sc, owner) {
  const call = (method) => async (url) => {
    log({ op: 'request', method, owner, url });
    const r = (sc.requests || {})[url];
    if (r === 'throw') throw new Error(`fake playwright: request to ${url} failed`);
    const status = typeof r === 'number' ? r : 200;
    return strict('apiResponse', {
      url: () => url,
      status: () => status,
      ok: () => status >= 200 && status < 300,
      dispose: async () => {},
      headers: () => ({}),
    });
  };
  return strict('request', { get: call('GET'), head: call('HEAD'), fetch: call('FETCH') });
}

let pageSeq = 0;

function makePage(sc) {
  const id = ++pageSeq;
  const listeners = {};
  let current = 'about:blank';
  let spec = null;
  const emit = (ev, payload) => {
    for (const fn of listeners[ev] || []) fn(payload);
  };
  const page = {
    on(ev, fn) {
      log({ op: 'on', page: id, event: ev });
      (listeners[ev] = listeners[ev] || []).push(fn);
      return proxy;
    },
    off(ev, fn) {
      listeners[ev] = (listeners[ev] || []).filter((f) => f !== fn);
      return proxy;
    },
    async goto(url) {
      log({ op: 'goto', page: id, url });
      current = url;
      spec = (sc.pages || {})[url];
      if (!spec) throw new Error(`fake playwright: no scenario for ${url}`);
      if (spec.goto_error) throw new Error(spec.goto_error);
      const doc = response(url, spec.status || 200, 'document');
      emit('response', doc);
      for (const e of spec.events || []) {
        if (e.type === 'console') {
          emit(
            'console',
            strict('consoleMessage', {
              type: () => e.level,
              text: () => e.text,
              location: () => ({ url, lineNumber: 1, columnNumber: 1 }),
            })
          );
        } else if (e.type === 'pageerror') {
          emit('pageerror', new Error(e.message));
        } else if (e.type === 'response') {
          emit('response', response(e.url, e.status, e.resource_type || 'other'));
        } else if (e.type === 'requestfailed') {
          emit(
            'requestfailed',
            strict('request', {
              url: () => e.url,
              method: () => 'GET',
              resourceType: () => 'other',
              failure: () => ({ errorText: e.error || 'net::ERR_FAILED' }),
            })
          );
        }
      }
      return doc;
    },
    async $$eval(selector, fn, arg) {
      log({ op: '$$eval', page: id, selector });
      const links = (spec && spec.links) || [];
      const els = links.map((href) => ({
        href,
        getAttribute: (n) => (n === 'href' ? href : null),
      }));
      return fn(els, arg);
    },
    request: apiRequest(sc, `page${id}`),
    async waitForLoadState() {},
    async waitForTimeout() {},
    setDefaultTimeout() {},
    setDefaultNavigationTimeout() {},
    url: () => current,
    async close() {
      log({ op: 'page.close', page: id });
    },
  };
  const proxy = strict('page', page);
  return proxy;
}

const chromium = strict('chromium', {
  async launch(opts) {
    log({ op: 'launch', opts: opts || null });
    const sc = scenario();
    if (sc.launch_error) throw new Error(sc.launch_error);
    const browser = {
      async newPage() {
        return makePage(sc);
      },
      async newContext() {
        return strict('context', {
          newPage: async () => makePage(sc),
          close: async () => log({ op: 'context.close' }),
          request: apiRequest(sc, 'context'),
        });
      },
      async close() {
        log({ op: 'browser.close' });
      },
    };
    return strict('browser', browser);
  },
});

module.exports = { chromium };
