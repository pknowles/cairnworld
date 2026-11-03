const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { fixture, forward, forwardWebSocket, checkBrowser } = require("./browser_check.cjs");

// Real production route: /world/:id resolves the real session and real
// store data (the browser_checks fixture's owner, world and developer_mode
// state), not a handwritten page.
const server = createServer((request, response) => {
  forward(request, response).catch(error => server.emit("error", error));
});

// With Developer Mode on, the page opens the live inspection socket.
server.on("upgrade", forwardWebSocket(server, `/world/${fixture.modal_world_id}/inspect/events`));

checkBrowser(server, async (tab, port) => {
  // Server-rendered controls must work before hydration attaches handlers.
  let allowScripts;
  const scriptsAllowed = new Promise(resolve => { allowScripts = resolve; });
  await tab.route("**/pkg/*.js", async route => {
    await scriptsAllowed;
    await route.continue();
  });
  try {
    await tab.goto(`http://127.0.0.1:${port}/world/${fixture.modal_world_id}`, { waitUntil: "commit" });
    await tab.getByRole("button", { name: "Enable Developer Mode", exact: true }).waitFor();

    // The warning is a real <dialog>: closed by default, not an inline block
    // that already occupies page flow.
    assert.equal(await tab.locator("dialog[open]").count(), 0, "the warning dialog starts closed");

    await tab.getByRole("button", { name: "Enable Developer Mode", exact: true }).click();
    await tab.locator("dialog[open]").waitFor();
    await tab.getByText("Enabling Developer Mode exposes complete internal game state", { exact: false }).waitFor();

    // Cancel closes the dialog without submitting the real enable form.
    await tab.getByRole("button", { name: "Cancel", exact: true }).click();
    await tab.waitForFunction(() => document.querySelector("dialog[open]") === null);
    assert.equal(tab.url(), `http://127.0.0.1:${port}/world/${fixture.modal_world_id}`, "cancelling never navigates");

    // Finish loading assets before the form navigates away from this page.
    allowScripts();
    await tab.waitForLoadState("networkidle");

    // Confirming submits the real enable form against the real owner-toggle
    // route (a 303 redirect back to world detail, like any plain HTML form
    // post) with the state now on.
    await tab.getByRole("button", { name: "Enable Developer Mode", exact: true }).click();
    await tab.locator("dialog[open]").waitFor();
    const enableRedirect = tab.waitForResponse(r => r.request().method() === "POST" && r.url().endsWith(`/world/${fixture.modal_world_id}/developer-mode`));
    await tab.getByRole("button", { name: "Enable anyway", exact: true }).click();
    assert.equal((await enableRedirect).status(), 303);
    await tab.getByText("Developer Mode is on", { exact: false }).waitFor();
    assert.equal(tab.url(), `http://127.0.0.1:${port}/world/${fixture.modal_world_id}`, "the redirect lands back on world detail");

    // The world's own page hosts the inspection panel, listing its agents.
    await tab.getByRole("heading", { name: "Inspect", exact: true }).waitFor();
    await tab.getByText("Game Master", { exact: false }).first().waitFor();
    // Its starting view is not a choice the visitor made, so it leaves the
    // address (and the Back button) alone; the panel is beside the page.
    assert.equal(tab.url(), `http://127.0.0.1:${port}/world/${fixture.modal_world_id}`, "opening the panel does not rewrite the address");
    const panel = await tab.locator("aside").boundingBox();
    const page = await tab.locator("section.card").boundingBox();
    assert.ok(panel.x >= page.x + page.width, "the panel sits beside the page, not below it");
  } finally {
    allowScripts();
    await tab.waitForLoadState("networkidle");
  }
}).catch(error => { console.error(error); process.exitCode = 1; });
