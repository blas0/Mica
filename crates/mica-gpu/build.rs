//! Compiles the Metal shaders into a `default.metallib` next to the binary.
//!
//! Precompiled rather than built at runtime: `newLibraryWithSource:` costs tens
//! of milliseconds of launch time, and a shader that fails to compile should
//! break the build, not the first window.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let shader = PathBuf::from("shaders/mica.metal");
    println!("cargo:rerun-if-changed={}", shader.display());
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let air = out_dir.join("mica.air");
    let metallib = out_dir.join("default.metallib");

    run(Command::new("xcrun").args([
        "-sdk",
        "macosx",
        "metal",
        "-O2",
        // Terminal text is not a physics simulation; fast math costs nothing
        // here and the SDF maths is well inside its error budget.
        "-ffast-math",
        "-c",
    ])
    .arg(&shader)
    .arg("-o")
    .arg(&air));

    run(Command::new("xcrun")
        .args(["-sdk", "macosx", "metallib"])
        .arg(&air)
        .arg("-o")
        .arg(&metallib));

    // The path is compiled in so the renderer can load the library without a
    // bundle, which is what makes headless tests possible.
    println!("cargo:rustc-env=MICA_METALLIB={}", metallib.display());
}

fn run(command: &mut Command) {
    let described = format!("{command:?}");
    let output = command
        .output()
        .unwrap_or_else(|e| panic!("failed to run {described}: {e}\nIs Xcode installed?"));
    if !output.status.success() {
        panic!(
            "{described} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
