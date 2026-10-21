//! Builds the overlay for 32-bit games too (TR2013's `tomb_helper_ui32.dll`), next to the exe, so a plain `cargo build`
//! builds everything. Cargo can't build a crate for two targets in one build, so this runs a second cargo for the 32-bit
//! overlay, in a target folder of its own (sharing this build's would wait on the lock it holds), and copies the DLL
//! over. It needs the 32-bit target installed, unless `TOMB_HELPER_SKIP_TR2013` is set, which skips it.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET_32: &str = "i686-pc-windows-msvc";
// What the overlay is built from
const OVERLAY_SOURCES: [&str; 6] =
    ["src/lib.rs", "src/ui_ipc_data.rs", "src/position.rs", "Cargo.toml", "Cargo.lock", ".cargo/config.toml"];
// Set for the second cargo, so its build of this script doesn't start another
const NESTED: &str = "TOMB_HELPER_32_BIT_OVERLAY_BUILD";
// Set to build without the 32-bit overlay
const SKIP: &str = "TOMB_HELPER_SKIP_TR2013";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for path in OVERLAY_SOURCES {
        println!("cargo:rerun-if-changed={}", path);
    }
    println!("cargo:rerun-if-env-changed={}", SKIP);
    // Only for the 64-bit Windows build (the 32-bit one is this script's own)
    if env::var("TARGET").map_or(true, |t| t != "x86_64-pc-windows-msvc") || env::var_os(NESTED).is_some() {
        return;
    }
    if env::var_os(SKIP).is_some() {
        println!("cargo:warning=Not building the 32-bit overlay for Tomb Raider 2013, as {} is set", SKIP);
        return;
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // OUT_DIR is <profile folder>/build/<package>-<hash>/out, and the exe goes in the profile folder
    let profile_dir = out_dir.ancestors().nth(3).unwrap().to_path_buf();
    let copied = profile_dir.join("tomb_helper_ui32.dll");

    if !target_installed() {
        panic!(
            "The 32-bit overlay (for Tomb Raider 2013) needs the {} target: `rustup target add {}`, or set {} to \
             build without it",
            TARGET_32, TARGET_32, SKIP
        );
    }
    let release = env::var("PROFILE").map_or(false, |p| p == "release");
    let target_dir = profile_dir.join("overlay-32-bit");
    let mut cargo = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()));
    cargo
        .args(["build", "--lib", "--target", TARGET_32, "--target-dir"])
        .arg(&target_dir)
        .current_dir(env::var("CARGO_MANIFEST_DIR").unwrap())
        .env(NESTED, "1")
        // This build's flags, which it would take as its own
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if release {
        cargo.arg("--release");
    }
    let output = cargo.output().expect("couldn't run cargo to build the 32-bit overlay");
    if !output.status.success() {
        panic!("Building the 32-bit overlay failed:\n{}", String::from_utf8_lossy(&output.stderr));
    }
    let built = target_dir.join(TARGET_32).join(if release { "release" } else { "debug" }).join("tomb_helper_ui.dll");
    // It can't be replaced while a game has it loaded
    if let Err(e) = std::fs::copy(&built, &copied) {
        println!("cargo:warning=Couldn't copy the 32-bit overlay to {} (is it loaded in the game?): {}", copied.display(), e);
    }
}

// Whether the 32-bit target's standard library is installed
fn target_installed() -> bool {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let sysroot = match Command::new(rustc).args(["--print", "sysroot"]).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(_) => return false,
    };
    Path::new(&sysroot).join("lib").join("rustlib").join(TARGET_32).is_dir()
}
