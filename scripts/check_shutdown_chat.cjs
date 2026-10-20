const assert = require("node:assert/strict");
const { WebSocket } = require("ws");
const { fixture } = require("./browser_check.cjs");

// The production player-chat socket, idle: server shutdown must tell the
// browser and close the connection so HTTP graceful shutdown cannot wait on it.
async function main() {
  const socket = new WebSocket(`${fixture.base.replace(/^http/, "ws")}/browser/chat/0/ws`, { headers: { cookie: fixture.cookie } });
  const received = [];
  const closed = new Promise((resolve, reject) => {
    socket.on("message", (raw) => received.push(JSON.parse(raw)));
    socket.on("error", reject);
    socket.on("close", resolve);
  });
  while (!received.some((event) => event.type === "can_act")) {
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.ok(socket.readyState <= WebSocket.OPEN, "socket closed before chat was ready");
  }
  assert.equal((await fetch(`${fixture.base}/browser/stop`, { method: "POST" })).status, 204);
  await closed;
  assert.deepEqual(received.at(-1), { type: "stopping" });
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
