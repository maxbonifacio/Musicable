# Musicable

An open-source digital audio workstation in Rust, with a workflow modelled on
Ableton Live's arrangement view. The first goal is a studio DAW for writing,
recording, editing and mixing. Live performance (Session View) comes later.

> Status: pre-alpha. Tier 1, the minimum usable DAW, is built: record
> audio and MIDI (MPE too) into takes and comp them, arrange clips, edit
> notes in the piano roll, play CLAP and VST3 plugins and eight built-in
> devices, mix with groups, sends and returns, automate, undo and redo
> anything, change any key, and export mixdowns and stems faster than real
> time. Tier 2 is next. See [docs/app.md](docs/app.md), which starts with
> the demos, and [docs/architecture.md](docs/architecture.md) for how it
> fits together and where Tier 2 starts.

[The manual](https://maxbonifacio.github.io/Musicable/) is for people
making music with Musicable. Its source is in [manual](manual), and
[CONTRIBUTING.md](CONTRIBUTING.md#the-manual) says how it's kept.

## Goals

- **Efficient multithreading.** The engine schedules work per plugin node, not
  per track, and runs tracks that aren't monitored live ahead of time with
  larger buffers.
- **Remappable hotkeys.** Every action is a named command, and its keys can
  be changed in the app. Keymaps are plain data you can edit, share and
  swap.
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
| `crates/audio` | Audio device output: our own WASAPI backend and ASIO host on Windows; cpal on macOS and Linux. See [docs/audio.md](docs/audio.md). |
| `crates/plugins` | CLAP and VST3 hosting, then ARA. See [docs/plugins.md](docs/plugins.md). |
| `crates/test-plugins` | Plugins that test the plugin hosting. Not shipped. |
| `crates/io` | Project files and audio/MIDI import and export. See [docs/project-format.md](docs/project-format.md). |
| `crates/library` | The browser's library of samples and presets: indexing, searching, and file work in the background. |
| `crates/analysis` | Analysis of audio for warping: onsets, the tempo, and beats and downbeats. See [docs/analysis.md](docs/analysis.md). |
| `crates/ui` | User interface, drawn with egui. Talks to the model through commands. See [docs/app.md](docs/app.md). |
| `crates/mcp` | Lets agents, such as Claude Code, control Musicable over MCP through its commands. See [docs/agents.md](docs/agents.md). |
| `crates/app` | The `musicable` app: its window, the audio output, and the session that runs commands and keeps the engine in step with the project. |

The engine never depends on the UI, and the UI never drives the engine
directly. Cargo dependencies enforce this.

## Building

```sh
scripts/fetch-models.sh                                 # the beat tracker's model files, once (fetch-models.ps1 in PowerShell)
cargo build
cargo test --workspace
cargo run --release -p musicable-app                    # the app; File > New Demo Project plays the demo
cargo run --release -p musicable-app -- null-test       # the parallel scheduler against the reference renderer
cargo run --release -p musicable-audio --example tone   # a test tone on the default output
```

On Windows, install the Visual Studio Build Tools and use Rust's MSVC
toolchain first ([CONTRIBUTING.md](CONTRIBUTING.md#toolchain) has the steps).
On Linux, install the ALSA development files first (`libasound2-dev` on
Debian and Ubuntu).

## License

Copyright © 2026 the Musicable contributors.

Musicable is free software: you can share it and change it under the terms
of the [GNU General Public License](LICENSE) as published by the Free Software
Foundation, either version 3 of the License or, at your option, any later
version. It comes with no warranty.

Musicable was licensed under either of the Apache License, Version 2.0 or
the MIT license, at your option, until it moved to the GPL in October 2026.
Copies from before then, such as v0.1.0, keep those terms.

Contributions are accepted under the same terms. See
[CONTRIBUTING.md](CONTRIBUTING.md). To report a security problem, see
[SECURITY.md](SECURITY.md).

ASIO and VST are trademarks of Steinberg Media Technologies GmbH, Ableton and
Live are trademarks of Ableton AG, and Melodyne is a trademark of Celemony
Software GmbH. Musicable is an independent project and isn't affiliated with
or endorsed by any of them.
