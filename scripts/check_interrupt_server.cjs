const assert = require("node:assert/strict");
const { WebSocket } = require("ws");
const { fixture } = require("./browser_check.cjs");

// The real player-chat socket and the pause controls: what the server reports
// for Interrupt and Never mind, with no page involved. The pause rules are
// covered by Rust tests; this checks they reach a connected player.
async function run() {
  const url = fixture.base.replace(/^http/, "ws") + "/browser/chat/0/ws";
  const socket = new WebSocket(url, { headers: { cookie: fixture.cookie } });
  const events = [];
  let wake = () => {};
  socket.on("message", (raw) => {
    events.push(JSON.parse(raw));
    wake();
  });
  const failure = new Promise((_, reject) => socket.on("error", reject));
  const next = async (predicate, what, timeout = 5000) => {
    const deadline = Date.now() + timeout;
    for (;;) {
      const found = events.findIndex(predicate);
      if (found >= 0) return events.splice(found, 1)[0];
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}; saw ${JSON.stringify(events)}`);
      await Promise.race([failure, new Promise((resolve) => { wake = resolve; setTimeout(resolve, 50); })]);
    }
  };
  const isControl = (own_hold, paused) => (event) =>
    event.type === "control" && event.own_hold === own_hold && event.paused === paused;

  await next((event) => event.type === "can_act" && event.value === true, "the chat to be ready");
  // Nothing is held when the page opens.
  await next(isControl(false, false), "the initial control state");

  socket.send(JSON.stringify({ type: "interrupt" }));
  // Nothing is running in the group, so the pause is reached at once.
  await next(isControl(true, true), "the pause to be acknowledged");

  socket.send(JSON.stringify({ type: "never_mind" }));
  await next(isControl(false, false), "the pause to be released");

  // Never mind without a pause changes nothing, so nothing is reported.
  socket.send(JSON.stringify({ type: "never_mind" }));
  await new Promise((resolve) => setTimeout(resolve, 400));
  assert.deepEqual(events.filter((event) => event.type === "control"), [], "a Never mind without an Interrupt reported a change");

  // A pause the player still holds ends when they disconnect.
  socket.send(JSON.stringify({ type: "interrupt" }));
  await next(isControl(true, true), "the second pause");
  return socket;
}

run()
  .then((socket) => socket.close())
  .catch((error) => {
    console.error(error);
    process.exitCode = 1;
    // An open socket would keep this process, and the test waiting on it, alive.
    process.exit();
  });
