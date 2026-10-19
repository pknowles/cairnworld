const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { fixture, forward, forwardWebSocket, checkBrowser } = require("./browser_check.cjs");

const server = createServer((request, response) => {
  const index = request.url === "/other" ? 1 : 0;
  const path = request.url === "/" || request.url === "/other" ? `/browser/chat/${index}` : request.url;
  forward(request, response, path).catch(error => server.emit("error", error));
});
server.on("upgrade", (request, socket, head) => {
  const url = new URL(request.url, fixture.base);
  const match = url.pathname.match(/\/characters\/(\d+)\/ws$/);
  // Inspection uses the ordinary authenticated route; only chat binds the
  // fixture's already-authenticated character to the generic production body.
  const upstream = match
    ? `/browser/chat/${fixture.chat_characters.indexOf(Number(match[1]))}/ws${url.search}`
    : request.url;
  forwardWebSocket(server, request.url, upstream)(request, socket, head);
});

checkBrowser(server, async (tab, port) => {
  const base = `http://127.0.0.1:${port}`;
  const ready = page => page.waitForFunction(() => document.querySelector('input[name="text"]')?.form.querySelector('button[type="submit"]')?.disabled === false);
  const action = async stage => {
    const response = await fetch(`${fixture.base}/browser/chat/action/${stage}`, {method: "POST"});
    assert.equal(response.status, 204);
  };
  const inspect = async () => {
    const started = performance.now();
    const response = await fetch(`${fixture.base}/world/${fixture.world_id}/inspect/agents`, {headers: {cookie: fixture.cookie}});
    assert.equal(response.status, 200);
    await response.arrayBuffer();
    return performance.now() - started;
  };
  const metrics = async () => {
    const response = await fetch(`${fixture.base}/browser/chat/metrics`);
    assert.equal(response.status, 200);
    return response.json();
  };
  const difference = (after, before) => Object.fromEntries(Object.keys(after).map(key => [key, after[key] - before[key]]));
  await tab.goto(base);
  await ready(tab);
  await action(0);
  await tab.locator("#chat").getByText("Passive split narration.", {exact: true}).waitFor();
  const beforeOne = await metrics();
  let started = performance.now();
  const [, oneViewerInspectionMs] = await Promise.all([action(1), inspect()]);
  await tab.locator("#chat").getByText("Private one-viewer narration.", {exact: true}).waitFor();
  const oneViewerMs = performance.now() - started;
  const oneViewerReads = difference(await metrics(), beforeOne);
  const pages = [tab];
  for (const path of ["/", "/other", "/other"]) {
    const page = await tab.context().newPage();
    await page.goto(base + path);
    await ready(page);
    pages.push(page);
  }
  const beforeFour = await metrics();
  started = performance.now();
  const [, fourViewerInspectionMs] = await Promise.all([action(2), inspect()]);
  for (const page of pages.slice(0, 2)) await page.locator("#chat").getByText("Private four-viewer narration.", {exact: true}).waitFor();
  const fourViewersMs = performance.now() - started;
  const fourViewerReads = difference(await metrics(), beforeFour);
  for (const page of pages.slice(2)) assert.equal(await page.locator("#chat").getByText(/Private (one|four)-viewer narration/).count(), 0);
  await action(3);
  for (const page of pages) await page.locator("#chat").getByText("Passive merge narration.", {exact: true}).waitFor();
  assert.equal((await fetch(`${fixture.base}/browser/chat/lag`, {method: "POST"})).status, 204);
  for (const page of pages) {
    await page.locator("#chat").getByText(/live notifications were missed/).waitFor();
    assert.equal(await page.locator("#chat").getByText("Passive merge narration.", {exact: true}).count(), 1);
  }
  await tab.reload();
  await ready(tab);
  assert.equal(await tab.locator("#chat").getByText("Passive merge narration.", {exact: true}).count(), 1);
  console.log(JSON.stringify({oneViewerMs, fourViewersMs, oneViewerInspectionMs, fourViewerInspectionMs, oneViewerReads, fourViewerReads}));
  for (const page of pages.slice(1)) await page.close();
}).catch(error => { console.error(error); process.exitCode = 1; });
