const { createServer } = require("node:http");
const { forward, checkBrowser } = require("./browser_check.cjs");

let ready = false;
const server = createServer((request, response) => {
  if (request.url === "/game-status") {
    ready = true;
    response.writeHead(204);
    return response.end();
  }
  if (request.url === "/" && ready) {
    response.writeHead(200, { "content-type": "text/html" });
    return response.end("<!doctype html><p id=ready>hydration reloaded after game-ready response</p>");
  }
  forward(request, response, request.url === "/" ? "/browser/loading" : request.url)
    .catch(error => server.emit("error", error));
});

checkBrowser(server, async (tab, port) => {
  await tab.goto(`http://127.0.0.1:${port}/`);
  await tab.locator("#ready").waitFor();
}).catch(error => { console.error(error); process.exitCode = 1; });
