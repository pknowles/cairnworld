# CairnWorld

A multiplayer text adventure RPG with AI storytelling

![banner image](media/banner.png)

## Browser development

The browser is an Axum server with a shared Leptos WASM island, not a separate
JavaScript application. Build its package and dark-theme stylesheet before
starting the server:

```sh
npm install
npm run build:frontend
cargo run -- serve --database /path/to/cairnworld.sqlite --model dev-qwen3
```

`local.toml` must provide the `[web]` Google OAuth values declared in
`default.toml`. The server serves the generated package at `/pkg` and the
checked-in landing artwork at `/media`.

`npm run test:hydration` builds the frontend and runs Rust's browser-test fixture
server with Playwright driving installed Chrome (`CHROME` can select its
executable). It needs permission to listen on loopback. Test databases and browser
profiles stay under `target/browser-tests`.

The checks use the production page renderer, generated hydration loader, and
authorized inspection routes. They cover loading-page reload, chat interaction
and reconnect, and inspecting recorded inference text and actions while chat
stays usable. Scripted WebSocket events and stored inference fixtures avoid
requiring OAuth or a model; these checks do not replace the full real-model
Developer Mode acceptance playtest. Unexpected browser errors fail the command,
and negative checks verify that behavior.

The Leptos configuration disables component erasure so `cargo leptos` frontend
builds match ordinary `cargo run` and `cargo test` server rendering. Both sides
must use the same rendering mode or hydration expects different HTML markers.
