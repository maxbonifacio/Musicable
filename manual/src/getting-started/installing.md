# Installing Musicable

Musicable has no downloads yet. You build it from its source code with
Rust, which takes a few minutes the first time.

## Windows

1. Install the Visual Studio Build Tools, with the MSVC compiler and the
   Windows 11 SDK. [CONTRIBUTING.md](https://github.com/maxbonifacio/Musicable/blob/main/CONTRIBUTING.md#toolchain)
   has the command that installs just what's needed.
2. Install Rust from [rustup.rs](https://rustup.rs), and make the MSVC
   toolchain its default:

   ```sh
   rustup set default-host x86_64-pc-windows-msvc
   rustup default stable
   ```

3. Get the source code, with Git or as a ZIP file from
   [GitHub](https://github.com/maxbonifacio/Musicable), and build and start
   Musicable in its folder:

   ```sh
   git clone https://github.com/maxbonifacio/Musicable.git
   cd Musicable
   cargo run --release -p musicable-app
   ```

After that, `target\release\musicable.exe` starts Musicable without a
command line, so you can make a shortcut to it.

## macOS and Linux

Install Rust from [rustup.rs](https://rustup.rs). On Linux, install the
ALSA development files too (`libasound2-dev` on Debian and Ubuntu). Then
get the source code and run the same `cargo run --release -p musicable-app`
in its folder. The program is `target/release/musicable`.

## The first start

Musicable opens with an empty project. To hear something at once, choose
**File > New Demo Project** and press Space: a dozen test tones play,
looping the first four bars. If nothing plays, **Options > Audio Settings**
picks the audio device.

## Updating

In the source code's folder, get the latest code and build it again:

```sh
git pull
cargo run --release -p musicable-app
```

Your settings stay where they are.

## Where your settings are kept

Musicable keeps its settings, your keys, your themes, your workspaces and
your profiles in a folder of its own:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\Musicable` |
| macOS | `~/Library/Application Support/Musicable` |
| Linux | `$XDG_CONFIG_HOME/musicable`, or `~/.config/musicable` |

To use another folder, such as one for trying things out, set the
`MUSICABLE_CONFIG_DIR` environment variable to it before starting Musicable.
