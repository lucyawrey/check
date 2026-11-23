use std::env;
use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "wasm32-unknown-unknown";

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let runtime = manifest.join("../check-runtime");
    println!("cargo::rerun-if-changed={}", runtime.display());
    let target_dir = PathBuf::from(env::var("OUT_DIR").unwrap()).join("runtime");
    let output = Command::new(env::var("CARGO").unwrap())
        .args(["build", "--release", "--target", TARGET, "--manifest-path"])
        .arg(runtime.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .args([
            "--config",
            "profile.release.panic=\"abort\"",
            "--config",
            "profile.release.opt-level=\"s\"",
            "--config",
            "profile.release.lto=true",
            "--config",
            "profile.release.strip=true",
        ])
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("run cargo for check-runtime");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains(TARGET) && stderr.contains("not installed") {
            panic!("check-wasm needs the {TARGET} target: rustup target add {TARGET}");
        }
        panic!("building check-runtime for {TARGET} failed:\n{stderr}");
    }
    let wasm = target_dir.join(TARGET).join("release/check_runtime.wasm");
    println!("cargo::rustc-env=CHECK_RUNTIME_WASM={}", wasm.display());
}
