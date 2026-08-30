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

`npm run test:hydration` runs the compiled loading and chat islands in headless
Chrome. It verifies both the ready-page reload and that a WebSocket readiness
event enables the rendered chat input, without requiring Google OAuth or an
inference model.
