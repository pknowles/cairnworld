// Leptos's typed view tree nests one generic layer per element/conditional;
// PlayerChat's inspect pane is now deep enough to exceed rustc's default.
#![recursion_limit = "256"]

pub mod inspection;
pub mod ui;

/// Entry point required by Leptos's generated islands loader.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    leptos::mount::hydrate_islands();
}
