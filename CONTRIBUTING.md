# Contributing

Thanks for helping out. The project is at a very early stage, so please open
an issue to discuss bigger changes before starting on them.

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

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

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

[docs/engine.md](docs/engine.md) explains the engine's threads, timing and
rules in full.
