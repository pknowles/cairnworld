# CairnWorld

A multiplayer text adventure RPG with AI storytelling

![banner image](media/banner.png)

## Build prerequisites on Windows ARM

Development tools are installed by the developer in their normal machine/user
locations. Keep Cargo's ordinary `target` layout; do not install toolchains,
download archives, or environment bootstrap scripts under it.

- Native ARM64 Rust through rustup, including rustfmt and Clippy. Browser builds
  also require the `wasm32-unknown-unknown` target and `cargo-leptos` in the normal
  Cargo tools directory.
- Visual Studio C++ ARM64 build tools and Windows SDK, with Clang and CMake
  available from the normal Visual Studio components. The current HTTPS
  dependencies compile native cryptographic code on Windows ARM.
- NVIDIA driver and a Windows ARM64 CUDA toolkit for this GPU. NVIDIA introduced
  RTX Spark Windows ARM support in CUDA 13.4. The vendored inference dependency
  combines upstream CUDA 13.4 support with Windows paged-attention compatibility.
- Node.js/npm for the checked-in Tailwind/DaisyUI stylesheet build. Browser tests
  additionally use an installed Chrome. Python is not a project prerequisite.

Install `cargo-leptos` and `wasm-bindgen-cli` through normal `cargo install`.
The `wasm-bindgen-cli` version must match `wasm-bindgen` in `Cargo.lock` so
`cargo-leptos` can use it from `PATH`.
JavaScript packages use the ordinary project-local `npm ci` installation.

Build in an ARM64 Visual Studio Developer PowerShell with `clang-cl` and `nvcc`
on `PATH`. Match CUDA's host compiler to Rust's default dynamic C runtime and
use MSVC's conforming preprocessor, including for Candle's kernels:

```powershell
$env:NVCC_PREPEND_FLAGS = '-Xcompiler=/Zc:preprocessor -Xcompiler=/MD'
$env:HF_HOME = "$PWD/models/huggingface"
cargo build --locked --bin cairnworld -j 4
```

The first build compiles CUDA kernels and can take tens of minutes. Cargo reuses
those artifacts on subsequent builds.

`HF_HOME` above reuses the downloaded model metadata. CUDA headers, compiler,
ARM64 import libraries, and runtime DLLs must come from the same toolkit.
`build.rs` uses `CUDA_PATH` to put ARM64 libraries first for the final application
link; the binding crate's automatic search still includes x64 directories.
On Windows unified-memory GPUs, admission uses the CUDA driver's combined
dedicated/shared budget. `nvidia-smi` reports only the dedicated carveout;
free CPU RAM is also not the CUDA allocation budget.
The backend requires paged attention and reserves the configured context-token
pool at startup. Model weights, recurrent state, and temporary computation
buffers also contribute to GPU memory usage. The device test
`configured_context_capacity_rejects_overflow_and_remains_usable` exercises the
checked-in context/concurrency limits, logs memory across repeated full-context
requests, and verifies fixed-cache rejection followed by a valid request.

References: [NVIDIA CUDA release notes](https://docs.nvidia.com/cuda/cuda-toolkit-release-notes/index.html),
[ring build requirements](https://github.com/briansmith/ring/blob/main/BUILDING.md),
and [mistral.rs paged attention](https://docs.mistralrs.dev/guides/perf/paged-attention/).

## Browser development

The browser is an Axum server with a shared Leptos WASM island, not a separate
JavaScript application. Build its package and dark-theme stylesheet before
starting the server:

```sh
npm ci
npm run build:frontend
cargo run -- serve --database /path/to/cairnworld.sqlite --model dev-qwen35
```

Provide the `[web]` Google OAuth values declared in `default.toml` through
`local.toml` or environment variables. `CAIRNWORLD__` variables override TOML;
`__` separates nested fields, for example `CAIRNWORLD__WEB__GOOGLE_CLIENT_ID`.
For local development, export the values saved in the Git-excluded
`secrets/dev.env` before the `cargo run` command (Git Bash):

Exclude `/secrets/` in `.git/info/exclude` before creating that local file.

```sh
set -a
. secrets/dev.env
set +a
```

The server serves the generated package at `/pkg` and the
checked-in landing artwork at `/media`.

`npm run test:hydration` builds the frontend and runs Rust's browser-test fixture
server with Playwright driving installed Chrome (`CHROME` can select its
executable). It needs permission to listen on loopback. Test databases and browser
profiles stay under `target/browser-tests`.

The checks use the production page renderer, generated hydration loader, and
authorized inspection routes. They cover loading-page reload, chat interaction
and reconnect, URL-backed Developer Mode navigation, raw-history/object paging,
object search and typed relationship links, Close/reopen, and narrow-screen
Chat/Inspect tabs while chat stays usable. Scripted WebSocket events and stored
inference fixtures avoid requiring OAuth or a model. Unexpected browser errors
fail the command, and negative checks verify that behavior.

The real-model tests share one model load in a single ignored test, `device_suite`,
which prints each scenario's time; it needs a CUDA driver and the `dev-qwen35` model
from `default.toml`. The whole test suite, including this and the browser test
below, must stay under the five-minute budget in `AGENTS.md`:

```sh
cargo test device_suite -- --ignored --nocapture
```

The device-only acceptance test additionally drives a configured CUDA model
through normal game play, then opens that exact recorded tool inference in the
browser and compares its request, rendered prompt, token evidence, and tool
call. It requires Chrome, a working CUDA driver, and the `dev-qwen35` model
from `default.toml`:

```sh
cargo test real_model_browser_opens_exact_inference_evidence -- --ignored --nocapture
```

The Leptos configuration disables component erasure so `cargo leptos` frontend
builds match ordinary `cargo run` and `cargo test` server rendering. Both sides
must use the same rendering mode or hydration expects different HTML markers.

## Editing prompts

Everything the models are told lives in `prompts.toml` (roles, per-event
requests, tool descriptions and tool-argument descriptions), grouped by agent:
the GM, the player agent, NPCs, compaction, and the speech tools players and
NPCs share. The server rereads the file whenever it changes, so an edit applies
to the next model request with no rebuild or restart. A mistake (unknown key,
missing argument text, wrong `{placeholder}`) fails the request that needs it
and names the problem. `CAIRNWORLD_PROMPTS` selects another file. Text the game
builds from live data, such as tool results, is still in code.

## Quality checks

Run the reproducible Rust quality gate before committing:

```sh
scripts/check-quality.sh
```

It checks formatting and treats warnings as errors for both the server and the
hydrated WASM library, and checks SQL against migrations using the matching
SQLx Cargo tool:

```sh
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features sqlite --target-dir target
```

Ordinary builds use committed `.sqlx` metadata and need no schema database.
The quality script uses only `target/sqlx-schema.sqlite`. After changing a
checked query or migration, regenerate metadata against that isolated schema:

```sh
export DATABASE_URL=sqlite://target/sqlx-schema.sqlite
cargo sqlx database setup --source migrations
cargo sqlx prepare -- --all-targets
```

Cargo's package lint settings also deny warnings and forbid unsafe code in
every application target, including the build script and tests. Do not suppress
lints with `allow` or `expect` attributes. Third-party dependencies retain their
own lint policies; the application's unsafe-code prohibition does not imply
that CUDA or other dependencies contain no unsafe code.
