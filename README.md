# Musicable

An open-source digital audio workstation in Rust, with a workflow modelled on
Ableton Live's arrangement view. The first goal is a studio DAW for writing,
recording, editing and mixing. Live performance (Session View) comes later.

> Status: pre-alpha. Nothing usable yet. Tier 0, the foundation, is done:
> the app plays, loops and saves projects of test tones. Tier 1 is under
> way, with undo and a mixer so far. See [docs/app.md](docs/app.md), and
> [docs/architecture.md](docs/architecture.md) for how it fits together.

## Goals

- **Efficient multithreading.** The engine schedules work per plugin node, not
  per track, and runs tracks that aren't monitored live ahead of time with
  larger buffers.
- **Remappable hotkeys.** Every action is a named command, and keymaps are
  plain data you can edit, share and swap.
- **ARA 2 plugin support** for tools like Melodyne.
- CLAP and VST3 hosting, and plugin sandboxing later.
- A familiar layout and default shortcuts for Ableton users. This is a
  clean-room project: no Ableton code, assets, names or device designs.

Windows is the first-class platform for now. macOS and Linux build in CI.

## Layout

| Crate | Role |
|---|---|
| `crates/model` | Pure-data project model. No audio, no UI. |
| `crates/commands` | Command registry, keymaps and transactions. See [docs/commands.md](docs/commands.md). |
| `crates/engine` | Real-time audio engine: graph, reference renderer and parallel scheduler. See [docs/engine.md](docs/engine.md). |
| `crates/audio` | Audio device output: our own WASAPI backend on Windows, cpal on macOS and Linux. See [docs/audio.md](docs/audio.md). |
| `crates/plugins` | CLAP, VST3 and ARA hosting. |
| `crates/io` | Project files and audio/MIDI import and export. See [docs/project-format.md](docs/project-format.md). |
| `crates/ui` | User interface, drawn with egui. Talks to the model through commands. See [docs/app.md](docs/app.md). |
| `crates/app` | The `musicable` app: its window, the audio output, and the session that runs commands and keeps the engine in step with the project. |

The engine never depends on the UI, and the UI never drives the engine
directly. Cargo dependencies enforce this.

## Building

```sh
cargo build
cargo test --workspace
cargo run --release -p musicable-app                    # the app; File > New Demo Project plays the demo
cargo run --release -p musicable-app -- null-test       # the parallel scheduler against the reference renderer
cargo run --release -p musicable-audio --example tone   # a test tone on the default output
```

On Linux, install the ALSA development files first (`libasound2-dev` on
Debian and Ubuntu).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Optional components that depend on GPL-licensed SDKs or libraries (the ASIO
SDK and Rubber Band) will live behind opt-in build features. Binaries built
with those features are distributed under GPLv3. The source in this
repository stays MIT OR Apache-2.0.

Contributions are accepted under the same terms. See
[CONTRIBUTING.md](CONTRIBUTING.md).
