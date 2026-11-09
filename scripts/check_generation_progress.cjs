const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { fixture, forward, forwardWebSocket, checkBrowser } = require("./browser_check.cjs");

// A world that is being made shows its phase, inferences and objects to a
// member who has Developer Mode off, and the page keeps that element current
// over the same kind of websocket the chat uses, without reloading.
const server = createServer((request, response) => {
  forward(request, response).catch(error => server.emit("error", error));
});
server.on("upgrade", forwardWebSocket(server, `/world/${fixture.making_world_id}/progress/ws`));

checkBrowser(server, async (tab, port) => {
  await tab.goto(`http://127.0.0.1:${port}/world/${fixture.making_world_id}`);
  const panel = tab.locator("#generation-progress");
  await panel.getByText("Preparing the setting", { exact: false }).waitFor();
  await panel.getByText("0 inferences so far", { exact: false }).waitFor();

  // Mark this document: a reload would lose the mark.
  await tab.evaluate(() => { window.generationPageMarker = "same page"; });
  // Give the island time to hydrate and connect before the world moves on.
  await new Promise(resolve => setTimeout(resolve, 2000));
  const advance = await fetch(`${fixture.base}/browser/generation/advance`, { method: "POST" });
  assert.equal(advance.status, 204);

  await panel.getByText("Developing the world summary", { exact: false }).waitFor({ timeout: 15000 });
  assert.equal(
    await tab.evaluate(() => window.generationPageMarker),
    "same page",
    "the page was updated in place, not reloaded",
  );
}).catch(error => { console.error(error); process.exitCode = 1; });
