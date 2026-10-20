const assert = require("node:assert/strict");
const { WebSocket } = require("ws");
const { fixture } = require("./browser_check.cjs");

// The production player-chat socket with a model that fails the turn: the
// player must be told about the failure exactly once, whichever path carries it.
const isError = (event) => event.type === "error" || (event.type === "entry" && event.role === "notice");

function open(index) {
  const socket = new WebSocket(`${fixture.base.replace(/^http/, "ws")}/browser/chat/${index}/ws`, { headers: { cookie: fixture.cookie } });
  const events = [];
  socket.on("message", (raw) => events.push(JSON.parse(raw)));
  socket.on("error", (error) => { throw error; });
  return { socket, events };
}

async function until(condition, what) {
  for (let waited = 0; waited < 10000; waited += 20) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error(`timed out waiting for ${what}`);
}

async function main() {
  const sender = open(1);
  const watcher = open(1);
  await until(() => [sender, watcher].every(({ events }) => events.some((event) => event.type === "can_act")), "both tabs to be ready");
  sender.events.length = watcher.events.length = 0;

  sender.socket.send(JSON.stringify({ type: "message", text: "This turn will fail." }));
  await until(() => sender.events.some((event) => event.type === "can_act" && event.value === true), "the failed turn to end");
  await until(() => watcher.events.some(isError), "the other tab to hear of the failure");
  await new Promise((resolve) => setTimeout(resolve, 300));

  for (const [name, { events }] of [["sending tab", sender], ["other tab", watcher]]) {
    assert.equal(events.filter(isError).length, 1, `${name} must show the failure exactly once: ${JSON.stringify(events)}`);
  }
  sender.socket.close();
  watcher.socket.close();
}

main().then(() => process.exit(0), (error) => {
  console.error(error);
  process.exit(1);
});
