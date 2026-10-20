const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { fixture, forward, checkBrowser } = require("./browser_check.cjs");
const { WebSocketServer } = require("ws");

const entries = Array.from({ length: 24 }, (_, index) => ({
  role: "assistant",
  text: index === 0 ? "The kettle whistles." : "The clock ticks (" + index + ").",
}));
let messages = 0;
let connections = 0;
let disconnections = 0;
let releaseOpening;
let releaseReply;
let closeReply;
const openingReady = new Promise((resolve) => (releaseOpening = resolve));
const replyReady = new Promise((resolve) => (releaseReply = resolve));
const closeReady = new Promise((resolve) => (closeReply = resolve));

const server = createServer((request, response) => {
  if (request.url === "/") {
    return forward(request, response, "/browser/render", { world_id: fixture.world_id, developer_mode: false, history: entries })
      .catch(error => server.emit("error", error));
  }
  if (request.url === "/release-opening") {
    releaseOpening();
    response.writeHead(204);
    return response.end();
  }
  if (request.url === "/release-reply") {
    releaseReply();
    response.writeHead(204);
    return response.end();
  }
  if (request.url === "/close-reply") {
    closeReply();
    response.writeHead(204);
    return response.end();
  }
  forward(request, response).catch(error => server.emit("error", error));
});

const sockets = new WebSocketServer({ noServer: true });
const devEventSockets = new WebSocketServer({ noServer: true });
server.on("upgrade", (request, socket, head) => {
  if (request.url === `/world/${fixture.world_id}/inspect/events`) {
    // developer_mode is false for this fixture: the island still opens this
    // connection (any active member may learn the mode changed), but no
    // DevEvent is ever relevant here, so just accept and hold it open.
    return devEventSockets.handleUpgrade(request, socket, head, () => {});
  }
  assert.equal(request.url, `/world/${fixture.world_id}/characters/2/ws?after_message_id=${entries.length}`);
  sockets.handleUpgrade(request, socket, head, (connection) => sockets.emit("connection", connection));
});

sockets.on("connection", (socket) => {
  connections += 1;
  if (connections === 1) {
    socket.send(JSON.stringify({ type: "activity", activity: "preparing_opening" }));
    openingReady.then(() => {
      socket.send(JSON.stringify({ type: "entry", role: "narration", text: "The door opens." }));
      socket.send(JSON.stringify({ type: "error", message: "Recorded agent failure." }));
      socket.send(JSON.stringify({ type: "can_act", value: true }));
    });
  } else {
    socket.send(JSON.stringify({ type: "can_act", value: true }));
  }
  socket.once("message", (raw) => {
    try {
      const event = JSON.parse(raw);
      assert.equal(event.type, "message");
      assert.equal(typeof event.text, "string");
      assert.notEqual(event.text.trim(), "");
      entries.push({ role: "user", text: event.text });
      // The production socket echoes the durable player event after the
      // client has already shown its optimistic placeholder. Its ID makes
      // that one visible entry inspectable in Developer Mode.
      socket.send(JSON.stringify({ type: "entry", role: "user", text: event.text, id: 25, sequence_id: 1 }));
      socket.send(JSON.stringify({ type: "activity", activity: "responding" }));
      socket.send(JSON.stringify({ type: "can_act", value: false }));
      const text = ++messages === 1 ? "Mara nods." : "A bell rings.";
      const reply = () => {
        entries.push({ role: "assistant", text });
        socket.send(JSON.stringify({ type: "entry", role: "assistant", text }));
        socket.send(JSON.stringify({ type: "can_act", value: true }));
        if (messages === 2) closeReady.then(() => {
          socket.send(JSON.stringify({ type: "stopping" }));
          socket.close();
        });
      };
      if (messages === 1) replyReady.then(reply);
      else reply();
    } catch (error) {
      server.emit("error", error);
    }
  });
  socket.on("error", (error) => server.emit("error", error));
  socket.once("close", () => (disconnections += 1));
});

async function ready(tab, text) {
  await tab.waitForFunction((text) => {
    const input = document.querySelector('input[name="text"]');
    return input && !input.disabled && document.body.textContent.includes(text);
  }, text);
}

async function release(port, path) {
  const response = await fetch(`http://127.0.0.1:${port}${path}`);
  assert.equal(response.status, 204);
}

checkBrowser(server, async (tab, port) => {
    await tab.goto(`http://127.0.0.1:${port}/`);
    assert.equal(await tab.evaluate(() => document.scrollingElement.scrollHeight <= window.innerHeight), true);
    assert.equal(await tab.evaluate(() => {
      const form = document.querySelector("#message").getBoundingClientRect();
      const pane = document.querySelector("main > section").getBoundingClientRect();
      return form.bottom < pane.bottom;
    }), true);
    await tab.locator("#chat-status").getByText("The game is preparing its first response…").waitFor();
    assert.equal(await tab.locator("#chat-status .loading-dots").count(), 1);
    assert.match(await tab.locator("#chat").innerText(), /The kettle whistles\./);
    assert.equal(await tab.locator('input[name="text"]').isDisabled(), false);
    assert.equal(await tab.locator('button[type="submit"]').isDisabled(), true);
    await release(port, "/release-opening");
    await ready(tab, "The door opens.");
    assert.equal(await tab.locator("#chat-status").count(), 0);
    assert.equal(await tab.locator("#chat").getByText("The door opens.").count(), 1);
    assert.equal(await tab.locator('#chat [data-role="narration"]').getByText("GM").count(), 1);
    assert.equal(await tab.locator("#chat").getByText("Recorded agent failure.").count(), 1);
    await tab.locator("#chat").evaluate((chat) => (chat.scrollTop = 0));
    await tab.locator('input[name="text"]').fill("I listen.");
    await tab.locator('input[name="text"]').press("Enter");
    await tab.locator("#chat-status").getByText("The game is responding…").waitFor();
    await tab.locator('#chat [data-role="user"]').getByText("I listen.", { exact: true }).waitFor();
    assert.equal(await tab.locator('#chat [data-role="user"]').getByText("I listen.", { exact: true }).count(), 1, "the durable socket echo replaces the optimistic player entry instead of rendering a duplicate");
    assert.equal(await tab.locator('input[name="text"]').isDisabled(), false);
    assert.equal(await tab.locator('button[type="submit"]').isDisabled(), true);
    assert.equal(await tab.locator('input[name="text"]').evaluate((input) => document.activeElement === input), true);
    assert.equal(await tab.locator("#chat").evaluate((chat) => chat.scrollTop + chat.clientHeight >= chat.scrollHeight), true);
    await release(port, "/release-reply");
    await ready(tab, "Mara nods.");
    assert.equal(await tab.locator("#chat-status").count(), 0);

    await tab.reload();
    // The reloaded page's SSR history can satisfy ready()'s text/enabled
    // check before the new WebSocket finishes connecting; wait for the
    // connection itself so this assertion is not a timing race.
    await new Promise((resolve, reject) => {
      const deadline = Date.now() + 5000;
      const poll = () => {
        if (connections === 2) return resolve();
        if (Date.now() > deadline) return reject(new Error(`expected a second WebSocket connection, saw ${connections}`));
        setTimeout(poll, 20);
      };
      poll();
    });
    await ready(tab, "I listen.");
    assert.equal(connections, 2);
    assert.ok(disconnections >= 1, "page reload did not close the prior WebSocket");
    assert.match(await tab.locator("#chat").innerText(), /I listen\./);
    assert.match(await tab.locator("#chat").innerText(), /Mara nods\./);
    assert.equal(await tab.locator("#chat-status").count(), 0, "refresh retained stale chat activity");
    assert.equal(await tab.locator("body").innerText().then((text) => text.includes("Your guide is preparing")), false);
    await tab.locator('input[name="text"]').fill("I wait.");
    await tab.locator('input[name="text"]').press("Enter");
    await ready(tab, "A bell rings.");
    assert.equal(messages, 2);
    await release(port, "/close-reply");
    await tab.locator("#chat").getByText("The server is stopping. Reload this page once it is running again.").waitFor();
    await tab.locator("#chat-status").getByText("Connection lost. Reload to reconnect.").waitFor();
    assert.equal(await tab.locator('input[name="text"]').isDisabled(), true);
}).catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
