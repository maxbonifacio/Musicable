# Contributing

Thanks for helping out. The project is at a very early stage, so please open
an issue to discuss bigger changes before starting on them. Security problems
are the exception: report them privately, as [SECURITY.md](SECURITY.md)
describes.

## Sign your commits (DCO)

We use the [Developer Certificate of Origin](https://developercertificate.org/)
instead of a CLA. Add a sign-off line to every commit:

```sh
git commit -s -m "Describe the change"
```

This adds `Signed-off-by: Your Name <you@example.com>` and certifies that you
wrote the change, or otherwise have the right to submit it, under the
project's license.

Unless you say otherwise, any contribution you submit is dual-licensed as
MIT OR Apache-2.0, with no additional terms.

## Clean-room rule

Don't contribute code, assets or text taken from Ableton Live or any other
proprietary DAW, including decompiled or disassembled code. Work from public
specifications and observed behaviour only.

## Toolchain

Musicable builds with stable Rust, 1.95 or newer; `rust-toolchain.toml` picks
the stable channel. On Windows, use the MSVC toolchain, as CI does:

1. Install the Visual Studio Build Tools with the MSVC compiler and the
   Windows 11 SDK, about 9 GB. Windows asks an administrator to allow it,
   and the prompt may wait on the taskbar:

   ```powershell
   winget install --id Microsoft.VisualStudio.BuildTools --exact --override "--passive --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100"
   ```

   If it asks for a restart, it is updating the Visual C++ runtime under
   programs that use it. The tools work straight away, and the restart can
   wait. Visual Studio Community with the "Desktop development with C++"
   workload has the same tools.
2. Make MSVC rustup's default, so `rust-toolchain.toml` picks it too:

   ```sh
   rustup set default-host x86_64-pc-windows-msvc
   rustup default stable
   ```

Plugins and their SDKs are built with MSVC, and Windows debuggers read its
debug information. The GNU toolchain (`x86_64-pc-windows-gnu`) builds
Musicable too, but nothing tests it.

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI also checks the ASIO build (the `asio` feature, [docs/audio.md](docs/audio.md#asio)),
which matters when you change `crates/audio` or the app's audio settings:

```sh
cargo clippy --workspace --all-targets --features musicable-app/asio -- -D warnings
cargo test -p musicable-audio --features asio
```

## Project files

Changing what a project file holds changes the file format, so it needs a
new format version and a migration step.
[docs/project-format.md](docs/project-format.md#changing-the-format) has the
steps.

## Real-time code

Code that runs on the audio thread (`Node::process`, `Node::adopt` and
anything they call) must not allocate or free memory, lock, block or do I/O.
Allocate when the node is built or in `Node::prepare`. In `adopt`, swap
resources with the old node rather than dropping them.

Debug builds check for allocations. Every binary and test crate that runs the
engine installs the checking allocator:

```rust
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;
```

A render that allocates then panics, naming the first allocation. Run the
test with `RUST_BACKTRACE=1` to see where it happened. Clippy bans locks,
sleeping and printing in the engine crate (`crates/engine/clippy.toml`).
CI also runs the engine's and the plugins' tests with RealtimeSanitizer,
which stops at what the checking allocator can't see, such as system calls
and a plugin's C or C++ allocations. `crates/engine/src/rt.rs` has the
command. Real-time code that has to make a call it stops at, such as waking
a thread, makes it in `rt::allowed`.

[docs/engine.md](docs/engine.md) explains the engine's threads, timing and
rules in full.

## Testing and measuring the scheduler

The parallel renderer is tested against the reference renderer on a few
thousand random graphs every run. To try more:

```sh
PROPTEST_CASES=100000 cargo test -p musicable-engine --lib random_graphs
```

`cargo +nightly miri test -p musicable-engine --lib parallel` checks the
small parallel tests for data races and undefined behaviour.

`musicable null-test [PROJECT]` plays a project, or the demo, with both
renderers on the machine at hand and checks they match sample for sample
([docs/app.md](docs/app.md#the-null-test)). It's worth running on a machine
with more cores than CI has.

Benchmarks run in release builds:

```sh
cargo bench -p musicable-engine --bench scheduler  # block times, 1 to N threads
cargo bench -p musicable-engine --bench realtime   # tables at a device's pace
```

Run them on a quiet machine, plugged in, and say which CPU when you report
numbers. They measure up to one thread per physical core, or as many as the
`MUSICABLE_BENCH_THREADS` environment variable says. CI runs both on Windows
when the engine changes, with one thread per logical processor, and puts the
numbers in the run's summary, but hosted runners are small shared machines:
treat their numbers as rough.
