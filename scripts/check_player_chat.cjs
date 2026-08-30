const { createHash } = require("node:crypto");
const { createServer } = require("node:http");
const { mkdtempSync, readFileSync, rmSync, createReadStream } = require("node:fs");
const { tmpdir } = require("node:os");
const { join, basename } = require("node:path");
const { spawn } = require("node:child_process");

const packageRoot = join(process.cwd(), "target/site/pkg");
const packageSource = readFileSync(join(packageRoot, "cairnworld.js"), "utf8");
const island = packageSource.match(/export function (PlayerChat_\d+)/)?.[1];
if (!island) throw new Error("generated package does not export the PlayerChat island");

const page = `<!doctype html>
<leptos-island data-component="${island}" data-props='{"world_id":1,"history":[]}'><ol id="chat" aria-live="polite"></ol><form id="message"><input name="text" autocomplete="off" disabled><button type="submit" disabled>Send</button></form></leptos-island>
<script type="module">
  const app = await import("/pkg/cairnworld.js");
  await app.default({module_or_path: "/pkg/cairnworld.wasm"});
  app.hydrate();
  app.${island}(document.querySelector("leptos-island"));
</script>`;

function websocketAccept(key) {
  return createHash("sha1")
    .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
    .digest("base64");
}

function websocketText(text) {
  const payload = Buffer.from(text);
  if (payload.length >= 126) throw new Error("test websocket message is unexpectedly large");
  return Buffer.concat([Buffer.from([0x81, payload.length]), payload]);
}

const server = createServer((request, response) => {
  if (request.url === "/") {
    response.writeHead(200, { "content-type": "text/html" });
    return response.end(page);
  }
  if (request.url?.startsWith("/pkg/")) {
    const file = join(packageRoot, basename(request.url));
    const contentType = file.endsWith(".js")
      ? "text/javascript"
      : file.endsWith(".wasm")
        ? "application/wasm"
        : "application/octet-stream";
    response.writeHead(200, { "content-type": contentType });
    return createReadStream(file).pipe(response);
  }
  response.writeHead(404);
  response.end("not found");
});

let socketError;

server.on("upgrade", (request, socket) => {
  if (request.url !== "/world/1/ws" || !request.headers["sec-websocket-key"]) {
    socket.destroy();
    return;
  }
  socket.write([
    "HTTP/1.1 101 Switching Protocols",
    "Upgrade: websocket",
    "Connection: Upgrade",
    `Sec-WebSocket-Accept: ${websocketAccept(request.headers["sec-websocket-key"])}`,
    "",
    "",
  ].join("\r\n"));
  socket.write(websocketText('{"type":"history","entries":[{"role":"assistant","text":"The kettle whistles."}]}'));
  socket.write(websocketText('{"type":"can_act","value":true}'));
  socket.on("error", (error) => {
    if (error.code !== "ECONNRESET") socketError = error;
  });
});

async function checkPlayerChat() {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const profile = mkdtempSync(join(tmpdir(), "cairnworld-player-chat-"));
  try {
    const browser = spawn(
      process.env.CHROME ?? "google-chrome",
      [
        "--headless",
        "--no-sandbox",
        "--disable-gpu",
        "--enable-logging=stderr",
        "--virtual-time-budget=1000",
        `--user-data-dir=${profile}`,
        "--dump-dom",
        `http://127.0.0.1:${port}/`,
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    let stdout = "";
    let stderr = "";
    browser.stdout.on("data", (chunk) => (stdout += chunk));
    browser.stderr.on("data", (chunk) => (stderr += chunk));
    const status = await new Promise((resolve, reject) => {
      browser.on("error", reject);
      browser.on("close", resolve);
    });
    if (socketError) throw socketError;
    const input = stdout.match(/<input[^>]*name="text"[^>]*>/)?.[0];
    if (status !== 0 || !input || input.includes("disabled") || !stdout.includes("The kettle whistles.")) {
      throw new Error(
        `compiled player chat did not render its server history and enable input after readiness:\n${stderr}\n${stdout}`,
      );
    }
  } finally {
    await new Promise((resolve) => server.close(resolve));
    rmSync(profile, { recursive: true, force: true });
  }
}

checkPlayerChat().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
