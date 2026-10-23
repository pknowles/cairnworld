const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { fixture, forward, checkBrowser } = require("./browser_check.cjs");
const { WebSocketServer } = require("ws");

// What the page sends when the player uses the Interrupt controls, and how the
// controls follow what the server reports back. The server side of the
// protocol is covered by Rust tests; this checks the page.
const history = [{ role: "assistant", text: "The kettle whistles." }];
const received = [];
let socket;

const server = createServer((request, response) => {
  if (request.url === "/") {
    return forward(request, response, "/browser/render", { world_id: fixture.world_id, developer_mode: false, history })
      .catch(error => server.emit("error", error));
  }
  const control = /^\/control\/(true|false)$/.exec(request.url);
  if (control) {
    socket.send(JSON.stringify({ type: "control", hand_raised: control[1] === "true" }));
    response.writeHead(204);
    return response.end();
  }
  forward(request, response).catch(error => server.emit("error", error));
});

const sockets = new WebSocketServer({ noServer: true });
const devEventSockets = new WebSocketServer({ noServer: true });
server.on("upgrade", (request, upgraded, head) => {
  if (request.url === `/world/${fixture.world_id}/inspect/events`) {
    return devEventSockets.handleUpgrade(request, upgraded, head, () => {});
  }
  sockets.handleUpgrade(request, upgraded, head, (connection) => sockets.emit("connection", connection));
});

sockets.on("connection", (connection) => {
  socket = connection;
  connection.send(JSON.stringify({ type: "can_act", value: true }));
  connection.on("message", (raw) => {
    try {
      received.push(JSON.parse(raw).type);
    } catch (error) {
      server.emit("error", error);
    }
  });
  connection.on("error", (error) => server.emit("error", error));
});

async function until(condition, what) {
  const deadline = Date.now() + 5000;
  while (!(await condition())) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

checkBrowser(server, async (tab, port) => {
  const get = (path) => fetch(`http://127.0.0.1:${port}${path}`).then((response) => assert.equal(response.status, 204));
  await tab.goto(`http://127.0.0.1:${port}/`);
  await until(() => socket && tab.locator('input[name="text"]').isEnabled(), "the chat connection");

  // Nothing is held: only Interrupt is offered, and Never mind is not shown.
  assert.equal(await tab.locator("#interrupt").isVisible(), true);
  assert.equal(await tab.locator("#never-mind").isVisible(), false);

  await tab.locator("#interrupt").click();
  await until(() => received.length === 1, "the Interrupt message");
  assert.deepEqual(received, ["interrupt"]);
  // The page changes only when the server says the hold exists.
  assert.equal(await tab.locator("#never-mind").isVisible(), false);
  await get("/control/true");
  await tab.locator("#never-mind").waitFor({ state: "visible" });
  assert.equal(await tab.locator("#interrupt").isVisible(), false);
  await tab.locator("#chat-status").getByText("Your hand is up. NPCs wait for you: send a message, or Never mind to continue.").waitFor();

  await tab.locator("#never-mind").click();
  await until(() => received.length === 2, "the Never mind message");
  assert.deepEqual(received, ["interrupt", "never_mind"]);
  await get("/control/false");
  await tab.locator("#interrupt").waitFor({ state: "visible" });
  assert.equal(await tab.locator("#never-mind").isVisible(), false);
  assert.equal(await tab.locator("#chat-status").count(), 0);

  // Another player speaking lowers the hand without this page doing anything.
  await get("/control/true");
  await tab.locator("#never-mind").waitFor({ state: "visible" });
  await get("/control/false");
  await tab.locator("#interrupt").waitFor({ state: "visible" });
  assert.equal(await tab.locator("#never-mind").isVisible(), false);
  assert.deepEqual(received, ["interrupt", "never_mind"], "nothing else was sent");
}).catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
