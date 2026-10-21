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

Code that runs on the audio thread (`Node::process` and anything it calls)
must not allocate, lock, block or do I/O.
