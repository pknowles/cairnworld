const assert = require("node:assert/strict");
const { WebSocket } = require("ws");
const { fixture } = require("./browser_check.cjs");

// The real player-chat socket and the Interrupt controls: what the server reports
// for Interrupt and Never mind, with no page involved. The raised-hand rules are
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
  const isControl = (hand_raised) => (event) =>
    event.type === "control" && event.hand_raised === hand_raised;

  await next((event) => event.type === "can_act" && event.value === true, "the chat to be ready");
  // No hand is raised when the page opens.
  await next(isControl(false), "the initial control state");

  socket.send(JSON.stringify({ type: "interrupt" }));
  await next(isControl(true), "the hand to go up");

  socket.send(JSON.stringify({ type: "never_mind" }));
  await next(isControl(false), "the hand to come down");

  // Never mind without a raised hand changes nothing, so nothing is reported.
  socket.send(JSON.stringify({ type: "never_mind" }));
  await new Promise((resolve) => setTimeout(resolve, 400));
  assert.deepEqual(events.filter((event) => event.type === "control"), [], "a Never mind without an Interrupt reported a change");

  // A hand still raised comes down when the player disconnects.
  socket.send(JSON.stringify({ type: "interrupt" }));
  await next(isControl(true), "the second raised hand");
  socket.close();
  await new Promise((resolve) => socket.once("close", resolve));
  await submittedMessageSurvivesDisconnect(url);
}

// A message the player has sent is accepted even if the connection closes at
// once: the turn belongs to the game, not to the socket.
async function submittedMessageSurvivesDisconnect(url) {
  const text = "Sent just before the connection closed.";
  const sender = new WebSocket(url, { headers: { cookie: fixture.cookie } });
  await new Promise((resolve, reject) => {
    sender.on("error", reject);
    sender.on("message", (raw) => JSON.parse(raw).type === "can_act" && resolve());
  });
  sender.send(JSON.stringify({ type: "message", text }));
  sender.close();
  const deadline = Date.now() + 5000;
  for (;;) {
    const page = await fetch(`${fixture.base}/browser/chat/0`, { headers: { cookie: fixture.cookie } });
    if ((await page.text()).includes(text)) return;
    if (Date.now() > deadline) throw new Error("the message sent just before the connection closed was lost");
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

run()
  .catch((error) => {
    console.error(error);
    process.exitCode = 1;
    // An open socket would keep this process, and the test waiting on it, alive.
    process.exit();
  });
