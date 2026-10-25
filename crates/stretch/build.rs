//! Builds Signalsmith Stretch, header-only C++, and Rubber Band Library,
//! from its single-file source, each with Musicable's C interface to it
//! (`src/stretch.cpp` and `src/rubber_band.cpp`), using the C++ compiler the
//! toolchain has: MSVC on Windows, Clang on macOS and GCC or Clang on Linux.
//! No bindgen, so no LLVM: `src/lib.rs` and `src/rubber_band.rs` declare the
//! interfaces by hand.

fn main() {
    println!("cargo::rerun-if-changed=src/stretch.cpp");
    println!("cargo::rerun-if-changed=src/rubber_band.cpp");
    println!("cargo::rerun-if-changed=vendor");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++14")
        .file("src/stretch.cpp")
        .file("src/rubber_band.cpp")
        .include("vendor")
        // Optimised in debug builds too: unoptimised, the stretchers run up
        // to ten times slower, which the tests and the live preview feel.
        .opt_level(3)
        // The vendored code is its authors', warnings and all.
        .warnings(false);
    if target == "windows" {
        // As Rubber Band's own Visual Studio project builds it.
        build
            .define("NOMINMAX", None)
            .define("_USE_MATH_DEFINES", None);
    }
    build.compile("musicable-stretch");
    if target == "macos" {
        // Rubber Band's single-file build uses Apple's FFT there.
        println!("cargo::rustc-link-lib=framework=Accelerate");
    }
}
