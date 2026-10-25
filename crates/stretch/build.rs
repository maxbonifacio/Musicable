//! Builds Signalsmith Stretch, header-only C++, with Musicable's C interface
//! to it (`src/stretch.cpp`), using the C++ compiler the toolchain has: MSVC
//! on Windows, Clang on macOS and GCC or Clang on Linux. No bindgen, so no
//! LLVM: `src/lib.rs` declares the interface by hand.

fn main() {
    println!("cargo::rerun-if-changed=src/stretch.cpp");
    println!("cargo::rerun-if-changed=vendor");
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .file("src/stretch.cpp")
        .include("vendor")
        // Optimised in debug builds too: unoptimised, the stretcher runs up
        // to ten times slower, which the tests and the live preview feel.
        .opt_level(3)
        // The vendored code is as it was released, warnings and all.
        .warnings(false)
        .compile("musicable-stretch");
}
