const assert = require("node:assert/strict");
const { chromium } = require("playwright-core");
const { WebSocket, WebSocketServer } = require("ws");

const fixture = JSON.parse(process.env.CAIRNWORLD_BROWSER_FIXTURE ?? "null");
assert.ok(fixture, "Run npm run test:hydration: Rust supplies the real rendered pages and inspection routes");

async function forward(request, response, path = request.url, data) {
  const init = data === undefined
    // No explicit JSON body: relay the real incoming request's own method
    // and body verbatim (e.g. a real HTML <form method="post"> submission),
    // not always a GET - a script only supplies path/data for its own
    // synthetic requests (the page shell, scripted fixture data).
    ? {
      method: request.method,
      headers: { cookie: fixture.cookie, ...(request.headers["content-type"] ? { "content-type": request.headers["content-type"] } : {}) },
      body: request.method === "GET" || request.method === "HEAD" ? undefined : request,
      duplex: request.method === "GET" || request.method === "HEAD" ? undefined : "half",
    }
    : {
      method: "POST", headers: { "content-type": "application/json", cookie: fixture.cookie }, body: JSON.stringify(data),
    };
  // Manual, not fetch()'s default auto-follow: a real browser navigation
  // (e.g. a plain <form method="post">) must see the real 303 and Location
  // itself and perform its own redirect, or its resulting URL and the
  // status a test observes would both be wrong.
  const result = await fetch(fixture.base + path, { ...init, redirect: "manual" });
  const headers = { "content-type": result.headers.get("content-type") ?? "text/plain" };
  if (result.headers.has("location")) headers.location = result.headers.get("location");
  response.writeHead(result.status, headers);
  response.end(Buffer.from(await result.arrayBuffer()));
}

// Proxies a websocket upgrade at `path` on the test's own Node server
// straight through to the real Axum server at `fixture.base`, verbatim in
// both directions - the production Store/Game/routes/broadcast stay in the
// exercised path, unlike a JavaScript-side WebSocketServer standing in for
// the real socket. `request.url` must equal `path` (query string included);
// a script forwards only the exact real path its own test drives. Errors on
// either leg are reported through the test's own HTTP `server` (the same
// object `checkBrowser` already listens for "error" on), so a proxy failure
// fails the check exactly like a forwarded HTTP request failure would.
function forwardWebSocket(errorServer, path) {
    const upstreamUrl = fixture.base.replace(/^http/, "ws") + path;
    return (request, socket, head) => {
        assert.equal(request.url, path);
        const upgrader = new WebSocketServer({ noServer: true });
        upgrader.handleUpgrade(request, socket, head, downstream => {
            const upstream = new WebSocket(upstreamUrl, { headers: { cookie: fixture.cookie } });
            const pending = [];
            let upstreamOpen = false;
            upstream.on("open", () => {
                upstreamOpen = true;
                for (const { payload, isBinary } of pending.splice(0)) upstream.send(payload, { binary: isBinary });
            });
            // `ws` infers text vs. binary framing from the JS type of the
            // value passed to `.send()`, not from the frame it was received
            // as - forwarding a `Buffer` verbatim (its default message type)
            // re-frames every text message as binary, which the browser's
            // `WebSocket.onmessage` then hands back as a Blob, not a string.
            // `isBinary` on the source event says which framing to preserve.
            downstream.on("message", (message, isBinary) => {
                const payload = isBinary ? message : message.toString();
                if (upstreamOpen) upstream.send(payload, { binary: isBinary });
                else pending.push({ payload, isBinary });
            });
            upstream.on("message", (message, isBinary) => {
                downstream.send(isBinary ? message : message.toString(), { binary: isBinary });
            });
            downstream.on("close", () => upstream.close());
            upstream.on("close", () => downstream.close());
            downstream.on("error", error => errorServer.emit("error", error));
            upstream.on("error", error => errorServer.emit("error", error));
        });
    };
}

async function checkBrowser(server, check) {
  const failures = [];
  let rejectFailure;
  const failure = new Promise((_, reject) => (rejectFailure = reject));
  const fail = (error) => { failures.push(error); rejectFailure(error); };
  server.on("error", fail);
  let browser;
  try {
    await Promise.race([new Promise(resolve => server.listen(0, "127.0.0.1", resolve)), failure]);
    browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome", headless: true, args: ["--no-sandbox"] });
    const tab = await browser.newPage();
    // Copy-to-clipboard buttons in Developer Mode need this to be readable
    // back for verification; harmless for checks that never touch it.
    await tab.context().grantPermissions(["clipboard-read", "clipboard-write"]);
    tab.on("pageerror", fail);
    tab.on("console", message => {
      if (message.type() === "error") fail(new Error(`Browser console: ${message.text()}`));
    });
    tab.on("response", response => {
      if (response.status() >= 400) fail(new Error(`HTTP ${response.status()}: ${response.url()}`));
    });
    tab.on("requestfailed", async request => {
      const error = request.failure().errorText;
      // Readiness reloads the page as soon as fetch receives its successful
      // headers. Chrome can report that already-successful request as aborted.
      const response = await request.response();
      if (error === "net::ERR_ABORTED" && response?.ok()) return;
      fail(new Error(`${error}: ${request.url()}`));
    });
    // Negative checks prove each browser failure channel fails the command.
    if (process.env.CAIRNWORLD_INJECT_BROWSER_ERROR) {
      await tab.addInitScript(kind => {
        if (kind === "exception") throw new Error("injected browser failure");
        if (kind === "console") console.error("injected browser console failure");
        if (kind === "resource") fetch("/injected-missing-resource");
      }, process.env.CAIRNWORLD_INJECT_BROWSER_ERROR);
    }
    await Promise.race([check(tab, server.address().port), failure]);
    assert.deepEqual(failures, []);
  } finally {
    await browser?.close();
    if (server.listening) {
      await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    }
  }
}

module.exports = { fixture, forward, forwardWebSocket, checkBrowser };
