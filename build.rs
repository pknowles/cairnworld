// The final package must prefer ARM64 imports over cudarc's x64 search defaults.
use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    if env::var("TARGET")? == "aarch64-pc-windows-msvc"
        && env::var_os("CARGO_FEATURE_SSR").is_some()
    {
        let root =
            env::var_os("CUDA_PATH").ok_or("CUDA_PATH must name the installed CUDA toolkit")?;
        let libraries = PathBuf::from(root).join("lib").join("arm64");
        if !libraries.is_dir() {
            return Err(format!("CUDA ARM64 libraries missing at {}", libraries.display()).into());
        }
        println!("cargo:rustc-link-search=native={}", libraries.display());
    }
    Ok(())
}
