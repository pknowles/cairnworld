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
const chatPath = `/world/${fixture.world_id}/characters/2/ws?after_message_id=${fixture.live_message_id}`;
const inspectPath = `/world/${fixture.world_id}/inspect/events`;

let connections = 0;
let messages = 0;
const server = createServer((request, response) => {
  const data = request.url === "/" ? {
    world_id: fixture.world_id, developer_mode: true,
    history: [
      { role: "user", text: "I prepare for adventure.", id: 24, sequence_id: fixture.sequence_id },
      {
        role: "user", text: "I am ready for a live inspection test.",
        id: fixture.live_message_id, sequence_id: fixture.live_sequence_id,
      },
    ],
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
      agent_id: fixture.live_agent_id, sequence_id: fixture.live_sequence_id,
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
async function finishLiveInference(inferenceId, content) {
  const response = await fetch(`${fixture.base}/browser/inference/finish`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ agent_id: fixture.agent_id, sequence_id: fixture.sequence_id, inference_id: inferenceId, content }),
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
  await tab.locator("aside").getByText("Tool call: ready_to_begin", { exact: true }).waitFor();
  await tab.locator("aside").getByText("Result", { exact: true }).waitFor();
  assert.equal(await tab.locator("aside").getByText('ready_to_begin({}) -> {"ready":true}', { exact: true }).count(), 0, "a tool execution is nested under its model call instead of repeated as a second summary");
  assert.equal((await response).status(), 200);
  const selectedEntry = await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).first().getAttribute("data-inspect-entry");
  assert.equal(tab.url(), `http://127.0.0.1:${port}/?inspect=turn&id=${fixture.sequence_id}&entry=${selectedEntry}`, "an entry link keeps its exact durable entry in the turn URL");
  assert.equal(await tab.locator("aside .ring-2").count() > 0, true, "the turn visibly retains the selected entry context");
  await tab.locator("aside").getByText("In the dim light of the weak fire", { exact: false }).waitFor();
  // Cumulative totals count every inference caused by this action once. The
  // tool call, its follow-up reply, and the deferred compaction are all tied
  // to the triggering sequence; the saved real-model responses contribute
  // 1,726 input and 767 output tokens. Duration is measured wall time, so it must be displayed but has
  // no fixture-specific value.
  await tab.locator("aside details summary").getByText("Technical details", { exact: true }).first().click();
  await tab.locator("aside").getByText("4 inferences", { exact: true }).waitFor();
  assert.equal(await tab.locator("aside").getByText("1726+767 tokens total", { exact: true }).count(), 1);
  assert.equal(await tab.locator("aside").getByText(/^\d+ms summed inference time$/).count(), 1);
  await tab.locator("aside").getByText("What would you like to do next?", { exact: true }).waitFor();
  const compaction = tab.locator("aside details.border-warning").filter({ hasText: "Certainly, I understand the request." });
  await compaction.waitFor();
  assert.equal(await compaction.evaluate((element) => element.open), false, "a compaction summary stays collapsed until the developer asks to read it");
  await compaction.locator("summary").click();
  await tab.locator("aside").getByText("Certainly, I understand the request.", { exact: false }).waitFor();
  assert.ok(await tab.locator("aside").getByText(/^Player agent/).count());
  assert.ok(await tab.locator("aside").getByText("Game Master", { exact: true }).count());
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
  // A real second object page must append while the live chat draft remains
  // in its still-mounted input. This is also the pagination step F.4 must
  // survive when later switching narrow panes.
  await input.fill("draft survives pagination and narrow Inspect");
  const objectPage = tab.waitForResponse(r => r.url().includes("/inspect/objects?after="));
  await tab.locator("aside").getByRole("button", { name: "Load more", exact: true }).click();
  assert.equal((await objectPage).status(), 200);
  await tab.locator("aside").getByText("pagination fixture item 59", { exact: false }).first().waitFor();
  assert.equal(await input.inputValue(), "draft survives pagination and narrow Inspect");
  // F.2's server-side object index must search real state before rendering,
  // then a returned typed relationship must navigate to its exact durable
  // target rather than trying to infer it from a repeated display name.
  const objectSearch = tab.locator('aside input[placeholder="Search name, type, or ID"]');
  await objectSearch.fill("flour sack");
  const searchResponse = tab.waitForResponse(r => r.url().includes("/inspect/objects?search=flour%20sack"));
  await tab.locator("aside").getByRole("button", { name: "Search", exact: true }).click();
  assert.equal((await searchResponse).status(), 200);
  await tab.locator("aside").getByText("item flour sack", { exact: false }).waitFor();
  const relatedObject = tab.waitForResponse(r => /\/inspect\/objects\?type=item_type&id=\d+$/.test(new URL(r.url()).pathname + new URL(r.url()).search));
  await tab.locator("aside button").filter({ hasText: /^item_type provisions/ }).click();
  assert.equal((await relatedObject).status(), 200);
  assert.match(tab.url(), /\?inspect=objects&type=item_type&id=\d+$/, "a relationship selects its typed object URL");
  // F.4 switches narrow panes through CSS without remounting either one:
  // the selected inspect record and unsent chat draft both survive, and the
  // chat transport remains the original single connection.
  await tab.setViewportSize({ width: 375, height: 800 });
  await tab.locator("#chat-tab").click();
  await tab.locator("#inspect-tab").click();
  await tab.locator("aside").getByText("item_type provisions", { exact: false }).waitFor();
  await tab.locator("#chat-tab").click();
  assert.equal(await input.inputValue(), "draft survives pagination and narrow Inspect");
  assert.equal(connections, 1, "narrow pane tabs must not reopen the chat socket");
  await tab.setViewportSize({ width: 1280, height: 720 });

  // The agent navigator links to each agent's own raw chat history - the
  // player's own triggering message, stored with no source inference of its
  // own, must still appear verbatim.
  const agentsResponse = tab.waitForResponse(r => r.url().endsWith("/inspect/agents"));
  await tab.locator("aside").getByText("Agents", { exact: true }).click();
  assert.equal((await agentsResponse).status(), 200);
  const historyResponse = tab.waitForResponse(r => r.url().includes(`/inspect/agents/${fixture.agent_id}/history`));
  await tab.locator("aside button").filter({ hasText: "player" }).first().click();
  assert.equal((await historyResponse).status(), 200);
  await tab.locator("aside").getByText("The required rolls are complete. Begin play now.", { exact: true }).first().waitFor();
  assert.equal(tab.url(), `http://127.0.0.1:${port}/?inspect=history&agent=${fixture.agent_id}`, "browsing the navigator updates the stable inspect URL");
  // Back and Forward must restore the previous inspection selection without
  // writing replacement entries. The pane stays mounted throughout.
  await tab.goBack();
  await tab.locator("aside button").filter({ hasText: "player" }).first().waitFor();
  assert.equal(tab.url(), `http://127.0.0.1:${port}/?inspect=agents`, "Back restores the prior inspect view");
  await tab.goForward();
  await tab.locator("aside").getByText("The required rolls are complete. Begin play now.", { exact: true }).first().waitFor();
  assert.equal(tab.url(), `http://127.0.0.1:${port}/?inspect=history&agent=${fixture.agent_id}`, "Forward restores the history view without a replacement entry");

  // An open raw history is a live view too: the fixture's otherwise-untyped
  // agent receives a real inference through Store's browser-only driver.
  // Its Changed event must refetch the currently open history without a
  // manual close/reopen.
  await tab.locator("aside").getByText("Agents", { exact: true }).click();
  const liveHistoryResponse = tab.waitForResponse(r => r.url().includes(`/inspect/agents/${fixture.live_agent_id}/history`));
  await tab.locator("aside button").filter({ hasText: "Unknown agent role (unknown)" }).click();
  assert.equal((await liveHistoryResponse).status(), 200);
  await tab.locator("aside").getByText("I am ready for a live inspection test.", { exact: true }).waitFor();
  const historyInferenceId = await startLiveInference();
  const liveHistoryRefresh = tab.waitForResponse(r => r.url().includes(`/inspect/agents/${fixture.live_agent_id}/history`));
  await finishLiveInference(historyInferenceId);
  assert.equal((await liveHistoryRefresh).status(), 200, "a Changed event refreshes an already-open agent history");

  // The world_gm's history is a real DAG (the fixture split its starting
  // group in two): opening it must fetch the whole branch graph, then each
  // branch's own page, and render every branch as its own linked section -
  // not the single flat list a plain agent's history still is (D6,
  // user_declarations.md: "authentically display the DAG").
  const branchesResponse = tab.waitForResponse(r => r.url().endsWith(`/inspect/agents/${fixture.gm_agent_id}/branches`));
  await tab.locator("aside").getByText("Agents", { exact: true }).click();
  await tab.locator("aside button").getByText("Game Master", { exact: true }).click();
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

  // A chat entry opens an explicitly titled Turn. It summarizes execution
  // without muddling the detailed inference input into the turn itself.
  const sequenceResponse = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.sequence_id}`));
  await tab.locator(`[data-inspect-sequence="${fixture.sequence_id}"]`).click();
  await sequenceResponse;
  await tab.locator("aside h3").getByText("Turn", { exact: true }).waitFor();
  assert.equal(await tab.locator("aside h4").getByText("Input", { exact: true }).count(), 0, "a Turn summarizes execution rather than repeating inference input");
  const inferenceResponse = tab.waitForResponse(r => r.url().endsWith(`/inspect/inferences/${fixture.inference_id}`));
  await tab.locator("aside button").getByText("Inspect inference", { exact: true }).first().click();
  assert.equal((await inferenceResponse).status(), 200);
  await tab.locator("aside h3").getByText("Inference", { exact: true }).waitFor();
  await tab.locator("aside").getByText("Tool call: ready_to_begin", { exact: true }).waitFor();
  assert.equal(await tab.locator("aside").getByText(`call ${fixture.tool_call_id}`, { exact: true }).count(), 0, "opaque tool-call IDs are not the primary label");
  assert.equal(await tab.locator("aside").getByText("Call reference", { exact: true }).count(), 0, "persistence-only call IDs are not rendered");
  assert.equal(await tab.locator("aside h4").getByText("Input", { exact: true }).count(), 1);
  assert.equal(await tab.locator("aside h4").getByText("Output", { exact: true }).count(), 1);
  const headings = await tab.locator("aside h4").allTextContents();
  assert.deepEqual(headings.slice(0, 2), ["Input", "Output"], "the detailed inference shows input before output");
  assert.equal(await tab.locator("aside").getByText("View turn", { exact: true }).count(), 1, "the inference view links back to its turn");
  await tab.locator('aside summary[title="Technical details"]').click();
  await tab.locator("aside").getByText("Model: browser-test", { exact: true }).waitFor();
  assert.equal(await tab.locator("aside").getByText("Rendered model prompt", { exact: true }).count(), 0, "the final backend prompt is concealed in the technical popover rather than duplicated in input");
  assert.ok(await tab.locator("aside").getByText(/^Player agent/).count(), "the actor is named by game role rather than agent ID");
  const inferenceHistory = tab.waitForResponse(r => r.url().includes(`/inspect/agents/${fixture.agent_id}/history?branch=`) && r.url().includes("&entry="));
  await tab.locator("aside button").getByText(/^Player agent/).click();
  assert.equal((await inferenceHistory).status(), 200);
  await tab.locator("aside").getByText(/^Player agent.*chat history$/).waitFor();
  assert.match(tab.url(), new RegExp(`\\?inspect=history&agent=${fixture.agent_id}&branch=\\d+&entry=\\d+$`), "an inference history link keeps its exact durable branch and entry");
  // Design.md: exact text must be copyable without truncation. The button's
  // own click handler drives the copy, so this exercises the real code path.
  await tab.locator("aside").getByRole("button", { name: "Copy text", exact: true }).first().click();
  await tab.locator("aside").getByRole("button", { name: "Copied text", exact: true }).first().waitFor();
  const copied = await tab.evaluate(() => navigator.clipboard.readText());
  assert.equal(copied, "{}", "copy captures the exact tool call arguments, not a truncated or escaped version");
  assert.equal(await tab.locator("aside").getByText("Raw JSON", { exact: true }).count(), 0, "serialized HTTP transport data is not rendered as an inspection panel");
  assert.equal(await tab.locator("aside").getByText(/^prompt evidence:/).count(), 0, "hashes are not presented as work for the developer");

  // Live inference progress over the real /inspect/events socket: driving
  // Store::start_inference/publish_token/finish_inference through the
  // /browser/inference/* test-only routes exercises the actual production
  // broadcast, not a simulated event - only what triggers those store
  // writes is synthetic (design.md's Live inspection protocol).
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  // Close collapses to a thin always-visible strip - never removes the pane
  // from the DOM and never requires a separate reopen control elsewhere
  // (user_exerpts.md 2026-09-25/2026-09-28: options 1+3 combined).
  await tab.locator("#inspect-strip").waitFor({ state: "visible" });
  assert.equal(await tab.locator("aside").isVisible(), false, "Close hides the panel, not only clears the selection");
  await tab.locator("#inspect-strip").click();
  await tab.locator("aside").waitFor({ state: "visible" });
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  const liveSequenceResponse = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.live_sequence_id}`));
  await tab.locator(`[data-inspect-sequence="${fixture.live_sequence_id}"]`).click();
  assert.equal((await liveSequenceResponse).status(), 200);
  const liveInferenceId = await startLiveInference();
  await tab.locator("aside").getByText("Running…", { exact: true }).waitFor();
  await sendLiveToken(liveInferenceId, 0, "You reach for your ");
  await sendLiveToken(liveInferenceId, 19, "pack.");
  await tab.locator("aside").getByText("You reach for your pack.", { exact: false }).waitFor();
  await tab.locator("aside").getByText("Streaming…", { exact: true }).waitFor();
  const completionRefetch = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.live_sequence_id}`));
  await finishLiveInference(liveInferenceId);
  assert.equal((await completionRefetch).status(), 200, "completion re-fetches the turn for the durable recorded text");
  await tab.locator("aside").getByText("Live reply.", { exact: true }).first().waitFor();
  const silentInferenceId = await startLiveInference();
  const silentCompletionRefetch = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.live_sequence_id}`));
  await finishLiveInference(silentInferenceId, "");
  assert.equal((await silentCompletionRefetch).status(), 200, "a silent completion still refreshes the durable turn");
  await tab.locator("aside").getByText("No player-facing response.", { exact: true }).waitFor();
  assert.equal(await tab.locator("#chat").getByText("No player-facing response.", { exact: true }).count(), 0, "a silent completion is not player chat text");

  // Completion during snapshot load: reselecting the same turn starts a
  // fresh fetch, and completing a still-running inference concurrently with
  // that fetch (a `Changed` invalidation arriving mid-flight) must still end
  // on a snapshot that reflects the completed record, not the running one
  // the in-flight fetch started with - design.md: "fetch again once ... if
  // the just-loaded snapshot was invalidated".
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  const raceInferenceId = await startLiveInference();
  const snapshotDuringLoad = tab.waitForResponse(r => r.url().endsWith(`/inspect/sequences/${fixture.live_sequence_id}`));
  await tab.locator(`[data-inspect-sequence="${fixture.live_sequence_id}"]`).click();
  await snapshotDuringLoad;
  // The fetch above already returned the running inference (finish has not
  // been called yet); finishing now fires Changed while - or just after -
  // that snapshot is applied, exercising the same-generation refetch path.
  await finishLiveInference(raceInferenceId);
  await tab.locator("aside").getByText("Live reply.", { exact: true }).first().waitFor();

  // A missing partial-token prefix must be visibly marked: a second live
  // inference whose first token arrives with a nonzero offset (as if an
  // earlier chunk were lost to broadcast lag) shows the loss inline.
  await tab.locator("aside").getByText("Close", { exact: true }).click();
  await tab.locator(`[data-inspect-sequence="${fixture.live_sequence_id}"]`).click();
  await tab.locator("aside").getByText("Live reply.", { exact: true }).first().waitFor();
  const gappedInferenceId = await startLiveInference();
  await tab.locator("aside").getByText("Running…", { exact: true }).waitFor();
  await sendLiveToken(gappedInferenceId, 12, "continues.");
  await tab.locator("aside").getByText("…missing…continues.", { exact: true }).waitFor();
  await tab.locator("aside").getByText("Streaming…", { exact: true }).waitFor();
  await finishLiveInference(gappedInferenceId);
  await tab.locator("aside").getByText("Close", { exact: true }).click();

  // Disabling mode mid-stream must suppress the rest of that inference's
  // tokens: a frame already authorized (this token) can still land, but no
  // further private frame may follow disable until mode is re-enabled.
  await tab.locator(`[data-inspect-sequence="${fixture.live_sequence_id}"]`).click();
  await tab.locator("aside").getByText("Live reply.", { exact: true }).first().waitFor();
  const queuedInferenceId = await startLiveInference();
  await tab.locator("aside").getByText("Running…", { exact: true }).waitFor();
  await sendLiveToken(queuedInferenceId, 0, "Before disable.");
  await tab.locator("aside").getByText("Before disable.", { exact: true }).waitFor();
  await setDeveloperMode(false);
  await tab.waitForFunction(() => !document.querySelector("aside"));
  // Tokens sent while disabled must never reach this viewer - re-enabling
  // afterward must not reveal them retroactively either, since the pane
  // reloads the selected view fresh rather than replaying suppressed frames.
  await sendLiveToken(queuedInferenceId, 15, " Hidden while disabled.");
  await finishLiveInference(queuedInferenceId);
  await setDeveloperMode(true);
  await tab.waitForFunction(() => document.body.textContent.includes("Developer Mode"));
  await tab.locator(`[data-inspect-sequence="${fixture.live_sequence_id}"]`).click();
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
  await tab.locator(`[data-inspect-sequence="${fixture.live_sequence_id}"]`).click();
  await tab.locator("aside").getByText("Live reply.", { exact: true }).first().waitFor();
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
  const afterReconnectId = await startLiveInference();
  await tab.locator("aside").getByText("Running…", { exact: true }).waitFor();
  await finishLiveInference(afterReconnectId);
}).catch(error => { console.error(error); process.exitCode = 1; });
