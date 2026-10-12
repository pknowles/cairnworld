const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { WebSocketServer } = require("ws");
const { fixture, forward, forwardWebSocket, checkBrowser } = require("./browser_check.cjs");

// The player-chat socket stays a JS fake here: App/GameLoad are hardwired to
// a real MistralRsBackend (see web.rs), so this browser-test process has no
// real Game loop to forward chat into without a much larger generic-backend
// refactor outside this slice's scope. The live inspection socket below has
// no such dependency - Store's broadcast is real regardless of which (or
// whether any) game model is loaded - so only it is forwarded to the actual
// server per design.md's Live inspection protocol.
const chatPath = `/world/${fixture.world_id}/characters/2/ws?after_message_id=24`;
const inspectPath = `/world/${fixture.world_id}/inspect/events`;

let connections = 0;
let messages = 0;
const server = createServer((request, response) => {
  const data = request.url === "/" ? {
    world_id: fixture.world_id, developer_mode: true,
    history: [{ role: "user", text: "I prepare for adventure.", id: 24, sequence_id: fixture.sequence_id }],
  } : undefined;
  forward(request, response, data ? "/browser/render" : request.url, data)
    .catch(error => server.emit("error", error));
});
const chatSockets = new WebSocketServer({ noServer: true });
server.on("upgrade", (request, socket, head) => {
  if (request.url === inspectPath) {
    return forwardWebSocket(server, inspectPath)(request, socket, head);
  }
  assert.equal(request.url, chatPath);
  chatSockets.handleUpgrade(request, socket, head, connection => chatSockets.emit("connection", connection));
});
chatSockets.on("connection", socket => {
  connections += 1;
  socket.on("error", error => server.emit("error", error));
  socket.send(JSON.stringify({ type: "can_act", value: true }));
  socket.on("message", raw => {
    const event = JSON.parse(raw);
    assert.equal(event.text, "I listen while inspecting.");
    messages += 1;
    socket.send(JSON.stringify({ type: "entry", role: "assistant", text: "The chat still works." }));
    socket.send(JSON.stringify({ type: "can_act", value: true }));
  });
});

async function startLiveInference(parentToolExecutionId) {
  const response = await fetch(`${fixture.base}/browser/inference/start`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({
      agent_id: fixture.agent_id, sequence_id: fixture.sequence_id,
      parent_tool_execution_id: parentToolExecutionId ?? null,
    }),
  });
  assert.equal(response.status, 200);
  return response.json();
}
async function sendLiveToken(inferenceId, offset, token) {
  const response = await fetch(`${fixture.base}/browser/inference/token`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ agent_id: fixture.agent_id, sequence_id: fixture.sequence_id, inference_id: inferenceId, offset, token }),
  });
  assert.equal(response.status, 200);
}
async function finishLiveInference(inferenceId) {
  const response = await fetch(`${fixture.base}/browser/inference/finish`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ agent_id: fixture.agent_id, sequence_id: fixture.sequence_id, inference_id: inferenceId }),
  });
  assert.equal(response.status, 200);
}
async function setDeveloperMode(enabled) {
  const response = await fetch(`${fixture.base}/world/${fixture.world_id}/developer-mode`, {
    method: "POST", headers: { "content-type": "application/x-www-form-urlencoded", cookie: fixture.cookie },
    body: `enabled=${enabled}`, redirect: "manual",
  });
  assert.equal(response.status, 303);
}
async function removeMember(userId) {
  const response = await fetch(`${fixture.base}/world/${fixture.world_id}/members/${userId}`, {
    method: "POST", headers: { cookie: fixture.cookie }, redirect: "manual",
  });
  assert.equal(response.status, 303);
}

checkBrowser(server, async (tab, port) => {
  // Records every WebSocket the page opens to /inspect/events, so the
  // failed-socket check below can sever the live inspection channel itself
  // (a real close, observed through the client's own onclose handling)
  // without guessing at internal state or replacing the whole WebSocket API
  // in a way that could mask a real connection failure elsewhere.
  await tab.addInitScript(() => {
    const OriginalWebSocket = window.WebSocket;
    window.__inspectSockets = [];
    window.__inspectSocketsReady = false;
    window.WebSocket = class extends OriginalWebSocket {
      constructor(url, protocols) {
        super(url, protocols);
        if (String(url).includes("/inspect/events")) {
          window.__inspectSockets.push(this);
          window.__inspectSocketsReady = true;
          window.__onInspectSocketsReady?.();
        }
      }
    };
  });
  await tab.goto(`http://127.0.0.1:${port}/`);
  await tab.waitForFunction(() => !document.querySelector('button[type="submit"]').disabled);
  const input = tab.locator('input[name="text"]');
  await input.fill("I listen while inspecting.");
  // Fetch and authorization use the actual Rust route and database record.
  const response = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.sequence_id}`));
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await tab.locator("aside li").getByText(`roll_hit_protection({}) -> {"hp":${fixture.hp}}`, { exact: true }).waitFor();
  assert.equal((await response).status(), 200);
  assert.equal(await tab.locator("aside p").filter({ hasText: "Ready for adventure:" }).textContent(), "Ready for adventure: <pack> & rope.\nStay alert.");
  // Cumulative totals count each inference once (design.md): the fixture's
  // two real recorded inferences are {1,6} and {1,10} tokens, 12ms and 10ms.
  await tab.locator("aside").getByText("2 inferences", { exact: true }).waitFor();
  assert.equal(await tab.locator("aside").getByText("2+16 tokens total", { exact: true }).count(), 1);
  assert.equal(await tab.locator("aside").getByText("22ms summed inference time", { exact: true }).count(), 1);
  assert.equal(await tab.locator("aside").getByText(`inference ${fixture.inference_id}`, { exact: true }).count(), 1);
  assert.equal(await tab.locator("aside").getByText(`agent ${fixture.agent_id}`, { exact: true }).count(), 2);
  // The verbatim input is real recorded history, not just the output -
  // opening it must show the exact message the inference actually received.
  await tab.locator("aside summary").first().click();
  await tab.locator("aside li").getByText("I prepare for adventure.", { exact: false }).first().waitFor();
  assert.equal(await input.inputValue(), "I listen while inspecting.");
  await input.press("Enter");
  await tab.locator("#chat").getByText("The chat still works.").waitFor();
  assert.equal(messages, 1);
  assert.equal(connections, 1, "inspection must not reopen the chat socket");

  // The world object browser shows real scenario data, not a projection -
  // installed_scenario's own name/location/NPC must be visible verbatim.
  const objectsResponse = tab.waitForResponse(r => r.url().endsWith("/inspect/objects"));
  await tab.locator("aside").getByText("Objects", { exact: true }).click();
  assert.equal((await objectsResponse).status(), 200);
  await tab.locator("aside").getByText("The Stolen Flour", { exact: false }).waitFor();
  await tab.locator("aside").getByText("charcoal hut", { exact: false }).first().waitFor();
  await tab.locator("aside").getByText("Mara", { exact: false }).first().waitFor();

  // The agent navigator links to each agent's own raw chat history - the
  // player's own triggering message, stored with no source inference of its
  // own, must still appear verbatim.
  const agentsResponse = tab.waitForResponse(r => r.url().endsWith("/inspect/agents"));
  await tab.locator("aside").getByText("Agents", { exact: true }).click();
  assert.equal((await agentsResponse).status(), 200);
  const historyResponse = tab.waitForResponse(r => r.url().includes(`/inspect/agents/${fixture.agent_id}/history`));
  await tab.locator("aside button").filter({ hasText: "player" }).first().click();
  assert.equal((await historyResponse).status(), 200);
  await tab.locator("aside").getByText("I prepare for adventure.", { exact: false }).first().waitFor();
  assert.equal(tab.url(), `http://127.0.0.1:${port}/`, "browsing the navigator never navigates");

  // The world_gm's history is a real DAG (the fixture split its starting
  // group in two): opening it must fetch the whole branch graph, then each
  // branch's own page, and render every branch as its own linked section -
  // not the single flat list a plain agent's history still is (D6,
  // user_declarations.md: "authentically display the DAG").
  const branchesResponse = tab.waitForResponse(r => r.url().endsWith(`/inspect/agents/${fixture.gm_agent_id}/branches`));
  await tab.locator("aside").getByText("Agents", { exact: true }).click();
  await tab.locator("aside button").filter({ hasText: "world_gm" }).first().click();
  assert.equal((await branchesResponse).status(), 200);
  await tab.locator("aside").getByText("retired", { exact: false }).first().waitFor();
  await tab.locator("aside").getByText("player side (current)", { exact: false }).first().waitFor();
  await tab.locator("aside").getByText("Toma side (current)", { exact: false }).first().waitFor();
  await tab.locator("aside").getByText("continues from branch", { exact: false }).first().waitFor();

  // Rapid selection changes: switching Agents -> Objects -> Agents faster
  // than any one fetch can land must still end on Agents' own data, not a
  // stale Objects (or earlier Agents) response applied out of order -
  // design.md: "a response applies only to that generation".
  await tab.locator("aside").getByText("Agents", { exact: true }).click();
  await tab.locator("aside").getByText("Objects", { exact: true }).click();
  await tab.locator("aside").getByText("Agents", { exact: true }).click();
  await tab.locator("aside").getByText("The Stolen Flour", { exact: false }).waitFor({ state: "detached" }).catch(() => {});
  assert.equal(await tab.locator("aside").getByText("The Stolen Flour", { exact: false }).count(), 0, "a stale Objects response must not land after switching back to Agents");
  await tab.locator("aside button").filter({ hasText: "player" }).first().waitFor();

  // The turn view's inference badge opens the dedicated inference view -
  // the same verbatim data, reached by a different route, still in place.
  const sequenceResponse = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.sequence_id}`));
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await sequenceResponse;
  const inferenceResponse = tab.waitForResponse(r => r.url().endsWith(`/inspect/inferences/${fixture.inference_id}`));
  await tab.locator("aside button").getByText(`inference ${fixture.inference_id}`, { exact: true }).first().click();
  assert.equal((await inferenceResponse).status(), 200);
  // A tool call renders as its own id/name/arguments, not one joined string
  // (each call must stay individually identifiable, never merged together).
  await tab.locator("aside").getByText("call hp-roll", { exact: true }).waitFor();
  await tab.locator("aside").getByText("roll_hit_protection", { exact: true }).waitFor();
  assert.equal(await tab.locator("aside").getByText(`turn ${fixture.sequence_id}`, { exact: true }).count(), 1, "the inference view links back to its turn");
  // Design.md: exact text must be copyable without truncation. The button's
  // own click handler drives the copy, so this exercises the real code path.
  await tab.locator("aside").getByRole("button", { name: "Copy", exact: true }).first().click();
  await tab.locator("aside").getByRole("button", { name: "Copied", exact: true }).first().waitFor();
  const copied = await tab.evaluate(() => navigator.clipboard.readText());
  assert.equal(copied, "{}", "copy captures the exact tool call arguments, not a truncated or escaped version");

  // The inference view also exposes the complete record as raw JSON
  // (design.md: "Render raw JSON ... with ... clipboard controls"), not
  // only the structured rendering above.
  await tab.locator("aside details summary").getByText("Raw JSON", { exact: true }).click();
  const rawJson = await tab.locator("aside details pre").textContent();
  const parsed = JSON.parse(rawJson);
  assert.equal(parsed.id, fixture.inference_id, "raw JSON is the real inference record, not a placeholder");
  assert.equal(parsed.outcome.Response.content.value[0].id, "hp-roll", "raw JSON keeps the tool call's id");
  await tab.locator("aside details").filter({ hasText: "Raw JSON" }).getByRole("button", { name: "Copy", exact: true }).click();
  await tab.locator("aside details").filter({ hasText: "Raw JSON" }).getByRole("button", { name: "Copied", exact: true }).waitFor();
  const copiedJson = await tab.evaluate(() => navigator.clipboard.readText());
  assert.equal(copiedJson, rawJson, "copy on the raw JSON panel captures exactly what is displayed");

  // Live inference progress over the real /inspect/events socket: driving
  // Store::start_inference/publish_token/finish_inference through the
  // /browser/inference/* test-only routes exercises the actual production
  // broadcast, not a simulated event - only what triggers those store
  // writes is synthetic (design.md's Live inspection protocol).
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await tab.locator("aside").getByText(`inference ${fixture.inference_id}`, { exact: true }).first().waitFor();
  const liveInferenceId = await startLiveInference(fixture.parent_tool_execution_id);
  await tab.locator("aside").getByText(`inference ${liveInferenceId}`, { exact: true }).waitFor();
  await sendLiveToken(liveInferenceId, 0, "You reach for your ");
  await sendLiveToken(liveInferenceId, 19, "pack.");
  await tab.locator("aside").getByText("You reach for your pack.…", { exact: true }).waitFor();
  const completionRefetch = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.sequence_id}`));
  await finishLiveInference(liveInferenceId);
  assert.equal((await completionRefetch).status(), 200, "completion re-fetches the turn for the durable recorded text");
  await tab.locator("aside").getByText("Live reply.", { exact: true }).waitFor();

  // Completion during snapshot load: reselecting the same turn starts a
  // fresh fetch, and completing a still-running inference concurrently with
  // that fetch (a `Changed` invalidation arriving mid-flight) must still end
  // on a snapshot that reflects the completed record, not the running one
  // the in-flight fetch started with - design.md: "fetch again once ... if
  // the just-loaded snapshot was invalidated".
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  const raceInferenceId = await startLiveInference(fixture.parent_tool_execution_id);
  const snapshotDuringLoad = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.sequence_id}`));
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await snapshotDuringLoad;
  // The fetch above already returned the running inference (finish has not
  // been called yet); finishing now fires Changed while - or just after -
  // that snapshot is applied, exercising the same-generation refetch path.
  await finishLiveInference(raceInferenceId);
  await tab.locator("aside").getByText(`inference ${raceInferenceId}`, { exact: true }).first().waitFor();
  await tab.locator("aside").getByText("Live reply.", { exact: true }).first().waitFor();

  // A missing partial-token prefix must be visibly marked: a second live
  // inference whose first token arrives with a nonzero offset (as if an
  // earlier chunk were lost to broadcast lag) shows the loss inline.
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await tab.locator("aside").getByText(`inference ${liveInferenceId}`, { exact: true }).first().waitFor();
  const gappedInferenceId = await startLiveInference(fixture.parent_tool_execution_id);
  await tab.locator("aside").getByText(`inference ${gappedInferenceId}`, { exact: true }).waitFor();
  await sendLiveToken(gappedInferenceId, 12, "continues.");
  await tab.locator("aside").getByText("…missing…continues.…", { exact: true }).waitFor();
  await finishLiveInference(gappedInferenceId);
  await tab.locator("aside").getByText("Close", { exact: true }).click();

  // Disabling mode mid-stream must suppress the rest of that inference's
  // tokens: a frame already authorized (this token) can still land, but no
  // further private frame may follow disable until mode is re-enabled.
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await tab.locator("aside").getByText(`inference ${fixture.inference_id}`, { exact: true }).first().waitFor();
  const queuedInferenceId = await startLiveInference(fixture.parent_tool_execution_id);
  await tab.locator("aside").getByText(`inference ${queuedInferenceId}`, { exact: true }).waitFor();
  await sendLiveToken(queuedInferenceId, 0, "Before disable.");
  await tab.locator("aside").getByText("Before disable.…", { exact: true }).waitFor();
  await setDeveloperMode(false);
  await tab.waitForFunction(() => !document.querySelector("aside"));
  // Tokens sent while disabled must never reach this viewer - re-enabling
  // afterward must not reveal them retroactively either, since the pane
  // reloads the selected view fresh rather than replaying suppressed frames.
  await sendLiveToken(queuedInferenceId, 15, " Hidden while disabled.");
  await finishLiveInference(queuedInferenceId);
  await setDeveloperMode(true);
  await tab.waitForFunction(() => document.body.textContent.includes("Developer Mode"));
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await tab.locator("aside").getByText(`inference ${queuedInferenceId}`, { exact: true }).first().waitFor();
  assert.equal(
    await tab.locator("aside").getByText("Hidden while disabled.", { exact: false }).count(),
    0,
    "a token sent while disabled must never appear, even after re-enabling",
  );
  await tab.locator("aside").getByText("Close", { exact: true }).click();

  // Live ban: a second, non-owner active member's own inspection socket
  // must close as soon as the owner removes their membership - the real
  // remove_member commit and its AccessChanged broadcast, not a simulated
  // close. Opened directly (not through the shared browser tab) since it
  // needs the viewer's own session, not the owner's.
  {
    const viewerSocketUrl = `${fixture.base.replace(/^http/, "ws")}${inspectPath}`;
    const { WebSocket } = require("ws");
    const viewerSocket = new WebSocket(viewerSocketUrl, { headers: { cookie: fixture.viewer_cookie } });
    await new Promise((resolve, reject) => {
      viewerSocket.on("open", resolve);
      viewerSocket.on("error", reject);
    });
    const closed = new Promise(resolve => viewerSocket.on("close", resolve));
    await removeMember(fixture.viewer_id);
    await closed;
  }

  // The developer_mode toggle itself must reach an already-open page: the
  // two-column layout, its badge, and inspect links must all disappear
  // without a reload, and reappear the same way when re-enabled.
  await setDeveloperMode(false);
  await tab.waitForFunction(() => !document.querySelector("aside") && !document.body.textContent.includes("Developer Mode"));
  // The static SSR history's data-inspect-sequence attributes are baked in
  // at first render (the island's static children are deliberately
  // non-reactive) and are not expected to disappear from the DOM; what must
  // actually stop working is the click itself opening the (now-closed) pane.
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  assert.equal(await tab.locator("aside").count(), 0, "a stale inspect link does nothing once the mode is live-disabled");
  assert.equal(await tab.locator("#chat").getByText("I prepare for adventure.").count(), 1, "the chat transcript survives the live mode toggle");
  await setDeveloperMode(true);
  await tab.waitForFunction(() => document.body.textContent.includes("Developer Mode"));
  assert.equal(tab.url(), `http://127.0.0.1:${port}/`, "the live toggle never navigates");

  // A failed/closed socket shows a visible error with a Reconnect control
  // (design.md: "fail-loudly", not the old silent best-effort degradation).
  // Open the turn view so live progress is visible again, then sever the
  // live channel from the browser side - the client's own onclose handling,
  // not a simulated server event, must report this.
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await tab.locator("aside").getByText(`inference ${fixture.inference_id}`, { exact: true }).first().waitFor();
  await tab.evaluate(() => new Promise(resolve => {
    // The live inspection socket is the one open WebSocket whose URL
    // targets /inspect/events; there is exactly one per page per the
    // island's own connect_dev_events.
    const patch = () => {
      const sockets = window.__inspectSockets ?? [];
      for (const socket of sockets) socket.close();
      resolve();
    };
    if (window.__inspectSocketsReady) return patch();
    window.__onInspectSocketsReady = patch;
  }));
  await tab.locator("aside").getByText("Live updates connection closed.", { exact: false }).waitFor();
  const reconnectButton = tab.locator("aside").getByRole("button", { name: "Reconnect", exact: true });
  await reconnectButton.waitFor();
  await reconnectButton.click();
  await tab.waitForFunction(() => !document.querySelector("aside").textContent.includes("Live updates connection closed."));
  // Reconnecting repeats the subscribe/snapshot handshake: a fresh live
  // inference on the same sequence must appear again without a reload.
  const afterReconnectId = await startLiveInference(fixture.parent_tool_execution_id);
  await tab.locator("aside").getByText(`inference ${afterReconnectId}`, { exact: true }).waitFor();
  await finishLiveInference(afterReconnectId);
}).catch(error => { console.error(error); process.exitCode = 1; });
