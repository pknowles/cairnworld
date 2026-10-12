const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { fixture, forward, forwardWebSocket, checkBrowser } = require("./browser_check.cjs");

const playPath = `/world/${fixture.world_id}/characters/${fixture.character_id}/play`;
const chatPath = `/world/${fixture.world_id}/characters/${fixture.character_id}/ws?after_message_id=${fixture.after_message_id}`;
const inspectPath = `/world/${fixture.world_id}/inspect/events`;
const server = createServer((request, response) => {
  forward(request, response, request.url === "/" ? playPath : request.url)
    .catch(error => server.emit("error", error));
});

server.on("upgrade", (request, socket, head) => {
  if (request.url === chatPath) return forwardWebSocket(server, chatPath)(request, socket, head);
  if (request.url === inspectPath) return forwardWebSocket(server, inspectPath)(request, socket, head);
  server.emit("error", new Error(`unexpected websocket path ${request.url}`));
  socket.destroy();
});

checkBrowser(server, async (tab, port) => {
  await tab.goto(`http://127.0.0.1:${port}/`);
  const sequenceEntry = tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`);
  assert.ok(await sequenceEntry.count(), "the real tool turn must be present in player chat");
  const sequenceResponse = tab.waitForResponse(response =>
    response.url().endsWith(`/inspect/sequences/${fixture.sequence_id}`));
  await sequenceEntry.first().click();
  assert.equal((await sequenceResponse).status(), 200);
  const inferenceResponse = tab.waitForResponse(response =>
    response.url().endsWith(`/inspect/inferences/${fixture.inference_id}`));
  await tab.locator("aside button").getByText("Inspect inference", { exact: true }).first().click();
  assert.equal((await inferenceResponse).status(), 200);
  await tab.locator("aside h3").getByText("Inference", { exact: true }).waitFor();
  const headings = await tab.locator("aside h4").allTextContents();
  assert.deepEqual(headings.slice(0, 2), ["Input", "Output"]);
  await tab.locator("aside").getByText(`Tool call: ${fixture.tool}`, { exact: true }).waitFor();
  assert.equal(await tab.locator("aside").getByText("Raw JSON", { exact: true }).count(), 0);
  assert.equal(await tab.locator("aside").getByText(/^prompt evidence:/).count(), 0);
});
