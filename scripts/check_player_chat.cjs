const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { readFileSync, createReadStream } = require("node:fs");
const { join, basename } = require("node:path");
const { chromium } = require("playwright-core");
const { WebSocketServer } = require("ws");

const packageRoot = join(process.cwd(), "target/site/pkg");
const packageSource = readFileSync(join(packageRoot, "cairnworld.js"), "utf8");
const island = packageSource.match(/export function (PlayerChat_\d+)/)?.[1];
if (!island) throw new Error("generated package does not export the PlayerChat island");

const entries = Array.from({ length: 24 }, (_, index) => ({
  role: "assistant",
  text: index === 0 ? "The kettle whistles." : "The clock ticks (" + index + ").",
}));
let messages = 0;
let socketError;
let connections = 0;
let disconnections = 0;
let releaseOpening;
let releaseReply;
const openingReady = new Promise((resolve) => (releaseOpening = resolve));
const replyReady = new Promise((resolve) => (releaseReply = resolve));

function escapeHtml(text) {
  return text.replace(/[&<>\"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[character]);
}

function page() {
  const transcript = entries.map(({ role, text }) => `<li data-role="${role}" class="chat chat-${role === "user" ? "end" : "start"}"><div class="chat-bubble">${escapeHtml(text)}</div></li>`).join("");
  return `<!doctype html>
<link rel="stylesheet" href="/pkg/cairnworld.css">
<main class="h-dvh bg-base-200 p-3 sm:p-6">
  <section class="mx-auto flex h-[calc(100dvh-1.5rem)] max-w-5xl flex-col rounded-box bg-base-100 shadow-xl sm:h-[calc(100dvh-3rem)]">
    <header class="navbar border-b border-base-300 px-4">Chat</header>
    <div id="chat-pane" class="flex min-h-0 flex-1 flex-col p-3 sm:p-5">
      <leptos-island data-component="${island}" data-props='{"world_id":1,"after_message_id":24}'>
        <ol id="chat" class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-1 py-4" aria-live="polite"><leptos-children>${transcript}</leptos-children></ol>
        <form id="message" class="join w-full"><input class="input join-item min-w-0 flex-1" name="text" autocomplete="off" disabled><button class="btn btn-primary join-item" type="submit" disabled>Send</button></form>
      </leptos-island>
    </div>
  </section>
</main>
<script type="module">
  const app = await import("/pkg/cairnworld.js");
  await app.default({module_or_path: "/pkg/cairnworld.wasm"});
  app.hydrate();
  app.${island}(document.querySelector("leptos-island"));
</script>`;
}

const server = createServer((request, response) => {
  if (request.url === "/") {
    response.writeHead(200, { "content-type": "text/html" });
    return response.end(page());
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
  if (request.url?.startsWith("/pkg/")) {
    const file = join(packageRoot, basename(request.url));
    const contentType = file.endsWith(".js")
      ? "text/javascript"
      : file.endsWith(".css")
        ? "text/css"
      : file.endsWith(".wasm")
        ? "application/wasm"
        : "application/octet-stream";
    response.writeHead(200, { "content-type": contentType });
    return createReadStream(file).pipe(response);
  }
  response.writeHead(404);
  response.end("not found");
});

const sockets = new WebSocketServer({ noServer: true });
server.on("upgrade", (request, socket, head) => {
  if (request.url !== "/world/1/ws?after_message_id=24") return socket.destroy();
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
      socket.send(JSON.stringify({ type: "activity", activity: "responding" }));
      socket.send(JSON.stringify({ type: "can_act", value: false }));
      const text = ++messages === 1 ? "Mara nods." : "A bell rings.";
      const reply = () => {
        entries.push({ role: "assistant", text });
        socket.send(JSON.stringify({ type: "entry", role: "assistant", text }));
        socket.send(JSON.stringify({ type: "can_act", value: true }));
        if (messages === 2) socket.close();
      };
      if (messages === 1) replyReady.then(reply);
      else reply();
    } catch (error) {
      socketError = error;
      socket.close();
    }
  });
  socket.on("error", (error) => (socketError = error));
  socket.once("close", () => (disconnections += 1));
});

async function ready(tab, text) {
  await tab.waitForFunction((text) => {
    const input = document.querySelector('input[name="text"]');
    return input && !input.disabled && document.body.textContent.includes(text);
  }, text);
  if (socketError) throw socketError;
}

async function release(port, path) {
  const response = await fetch(`http://127.0.0.1:${port}${path}`);
  assert.equal(response.status, 204);
}

async function checkPlayerChat() {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  let browser;
  try {
    browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome", headless: true, args: ["--no-sandbox"] });
    const tab = await browser.newPage();
    await tab.goto(`http://127.0.0.1:${port}/`);
    assert.equal(await tab.evaluate(() => document.scrollingElement.scrollHeight <= window.innerHeight), true);
    assert.equal(await tab.evaluate(() => {
      const form = document.querySelector("#message").getBoundingClientRect();
      const pane = document.querySelector("#chat-pane").getBoundingClientRect();
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
    assert.equal(await tab.locator('input[name="text"]').isDisabled(), false);
    assert.equal(await tab.locator('button[type="submit"]').isDisabled(), true);
    assert.equal(await tab.locator('input[name="text"]').evaluate((input) => document.activeElement === input), true);
    assert.equal(await tab.locator("#chat").evaluate((chat) => chat.scrollTop + chat.clientHeight >= chat.scrollHeight), true);
    await release(port, "/release-reply");
    await ready(tab, "Mara nods.");
    assert.equal(await tab.locator("#chat-status").count(), 0);

    await tab.reload();
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
    await tab.locator("#chat-status").getByText("Connection lost. Reload to reconnect.").waitFor();
    assert.equal(await tab.locator('input[name="text"]').isDisabled(), true);
  } finally {
    await browser?.close();
    for (const socket of sockets.clients) socket.terminate();
    await new Promise((resolve) => server.close(resolve));
  }
}

checkPlayerChat().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
