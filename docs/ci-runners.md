# CI runners

GitHub's hosted runners are free for a public repository and billed by the
minute for a private one (in 2026: Linux $0.006, Windows $0.010, macOS $0.062).
With every pull request running the full matrix, a private Musicable would
cost about $1,000 a month, two thirds of it macOS. So CI can run on our own
machines instead, and each workflow picks the runner from a repository
variable:

| Variable | Jobs | Hosted fallback |
| --- | --- | --- |
| `RUNNER_WINDOWS` | Test (windows-latest), Benchmarks | `windows-latest` |
| `RUNNER_LINUX` | Test (ubuntu-latest), fmt, DCO sign-off, RealtimeSanitizer, the manual's build | `ubuntu-latest` |
| `RUNNER_MACOS` | Test (macos-latest) | `macos-latest` |

Each variable holds a label that only our runner carries, such as
`musicable-windows`. While a variable is unset, its jobs run on GitHub's
runners, as before. A pull request from a fork always runs on GitHub's runners,
so code from outside never runs on our machines.

## When things run

| Workflow | Runs on | macOS |
| --- | --- | --- |
| CI | every push to a pull request and to main, nightly at 03:17 UTC, and by hand (Actions > CI > Run workflow) | every push while the repository is public; nightly on main and by hand while it's private |
| Benchmarks, RealtimeSanitizer | pushes that change the engine, the model, the plugins or the lock file | n/a |
| Manual | pushes that change `manual/` | n/a |

A new push to a pull request cancels the run it replaces. Every push to main
runs in full. The manual is published to GitHub Pages only while the
repository is public: GitHub Free doesn't serve Pages from a private
repository, so the manual workflow then only checks the manual builds.

A job waits for its runner while that machine is off or asleep, and starts
when it's back. A nightly macOS run still waiting is cancelled by the next
one.

## Setting up a runner

Each runner is registered from **Settings > Actions > Runners > New
self-hosted runner**, which shows the download and a one-hour token for the
chosen system. Run the jobs as your own user, so they find your Rust
toolchain. Each runner takes one job at a time; register a second one on the
same machine, with the same label, to run two.

### Windows

The machine needs what CONTRIBUTING.md's toolchain section installs (the
Visual Studio Build Tools and rustup with MSVC as its default host), and Git
for Windows, whose `bash` the workflows use.

1. Make a folder outside the repository, such as `C:\actions-runner`, and
   follow GitHub's download steps in it from PowerShell.
2. Configure it as a service that runs as you (it asks for your password):

   ```powershell
   .\config.cmd --url https://github.com/maxbonifacio/Musicable --token <TOKEN> `
     --name pc-windows --labels musicable-windows --runasservice `
     --windowslogonaccount "$env:COMPUTERNAME\$env:USERNAME"
   ```
3. To pause it, for example while recording, stop the service:
   `Stop-Service "actions.runner.*"`, and `Start-Service "actions.runner.*"` to
   carry on.

A CI run builds the workspace from scratch in the runner's `_work` folder, so
leave 20 GB free on that drive. The tests that need a real audio device are
`#[ignore]`d. One test opens the default output for a moment, silently, in
shared mode.

### Linux, in WSL2 on the Windows machine

1. In an administrator PowerShell: `wsl --install -d Ubuntu-24.04`, and
   restart when it asks. Set a user name and password when Ubuntu first opens.
2. In Ubuntu, install the build dependencies and Rust:

   ```sh
   sudo apt-get update
   sudo apt-get install -y build-essential pkg-config libasound2-dev curl git
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   ```

   The CI job skips its own `apt-get` when `libasound2-dev` is there, so the
   runner never needs your password.
3. Follow GitHub's Linux x64 download steps in `~/actions-runner`, then:

   ```sh
   ./config.sh --url https://github.com/maxbonifacio/Musicable --token <TOKEN> \
     --name pc-wsl --labels musicable-linux
   sudo ./svc.sh install "$USER" && sudo ./svc.sh start
   ```
4. WSL stops a distribution once nothing uses it. Keep it running with a
   Task Scheduler task at logon that runs
   `wsl.exe -d Ubuntu-24.04 --exec sleep infinity`.

### macOS

1. Install the command line tools (`xcode-select --install`) and rustup
   (the same `curl ... | sh` line as above).
2. Follow GitHub's macOS download steps in `~/actions-runner`, for the Mac's
   architecture, then:

   ```sh
   ./config.sh --url https://github.com/maxbonifacio/Musicable --token <TOKEN> \
     --name mac --labels musicable-macos
   ./svc.sh install && ./svc.sh start
   ```

   The service runs while you're logged in. A sleeping Mac runs nothing, so
   the nightly run waits for it to wake.

## Switching over

1. Set up the runners and check they show as Idle under Settings > Actions >
   Runners.
2. Under **Settings > Secrets and variables > Actions > Variables**, add
   `RUNNER_WINDOWS` = `musicable-windows`, `RUNNER_LINUX` = `musicable-linux`
   and `RUNNER_MACOS` = `musicable-macos`.
3. Run CI by hand (Actions > CI > Run workflow) and check each job's "Set up
   job" step names our runner.

To go back to GitHub's runners, delete the variable. Nothing else changes.
