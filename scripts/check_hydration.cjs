const { createServer } = require("node:http");
const { mkdtempSync, readFileSync, rmSync, createReadStream } = require("node:fs");
const { tmpdir } = require("node:os");
const { join, basename } = require("node:path");
const { spawn } = require("node:child_process");

const packageRoot = join(process.cwd(), "target/site/pkg");
const packageSource = readFileSync(join(packageRoot, "cairnworld.js"), "utf8");
const island = packageSource.match(/export function (GameLoading_\d+)/)?.[1];
if (!island) throw new Error("generated package does not export the GameLoading island");

let ready = false;
const loadingPage = `<!doctype html>
<leptos-island data-component="${island}"><main><p role="status">Preparing the game world…</p></main></leptos-island>
<script type="module">
  const app = await import("/pkg/cairnworld.js");
  await app.default({module_or_path: "/pkg/cairnworld.wasm"});
  app.hydrate();
  app.${island}(document.querySelector("leptos-island"));
</script>`;

const server = createServer((request, response) => {
  if (request.url === "/game-status") {
    ready = true;
    response.writeHead(204);
    return response.end();
  }
  if (request.url === "/") {
    response.writeHead(200, { "content-type": "text/html" });
    return response.end(
      ready
        ? "<!doctype html><p id=ready>hydration reloaded after game-ready response</p>"
        : loadingPage,
    );
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

async function checkHydration() {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const profile = mkdtempSync(join(tmpdir(), "cairnworld-hydration-"));
  try {
    const browser = spawn(
      process.env.CHROME ?? "google-chrome",
      [
        "--headless",
        "--no-sandbox",
        "--disable-gpu",
        "--virtual-time-budget=5000",
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
    if (status !== 0 || !stdout.includes("id=\"ready\"")) {
      throw new Error(
        `compiled hydration did not reload after a ready response:\n${stderr}\n${stdout}`,
      );
    }
  } finally {
    await new Promise((resolve) => server.close(resolve));
    rmSync(profile, { recursive: true, force: true });
  }
}

checkHydration().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
