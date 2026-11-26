# rata

Start, stop, and monitor long-running services.

`rata` is the first package of the rata suite. It is a command-line tool
that manages the services listed in a TOML config file, and a Rust library
that the suite's other packages build on. It runs on Linux, macOS, and
Windows, on x86_64 and ARM64.

```console
$ rata start
web_api  started           pid 4121  log work/log/web_api_20261125_143207.0.log
worker   already running   pid 3010
cache    FAILED            exited within 500ms (code 1), see work/log/cache_20261125_143207.0.log

$ rata status
NAME     STATE    PID   DESCRIPTION
web_api  running  4121  Web API Server
worker   running  3010  Background Job Worker
cache    stopped  -     Cache Server
```

## Install

Prebuilt binaries for Linux, macOS, and Windows (x86_64 and ARM64) are on
[GitHub Releases](https://github.com/markuskimius/rata/releases). The
installers put `rata` in `~/.cargo/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/markuskimius/rata/releases/latest/download/rata-installer.sh | sh
```
```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/markuskimius/rata/releases/latest/download/rata-installer.ps1 | iex"
```

With [Homebrew](https://brew.sh) (macOS and Linux):

```sh
brew install markuskimius/tap/rata
```

Or build from source with Rust 1.89 or newer:

```sh
cargo install rata
```

## Quick start

1. Choose a root directory for your install and point `RATA` at it. It must
   be an absolute path.

   ```sh
   export RATA=/opt/rata            # bash/zsh
   ```
   ```powershell
   $env:RATA = "C:\rata"            # PowerShell
   ```

2. Create `$RATA/work/cfg/service.toml`. Start from
   [`docs/service.example.toml`](docs/service.example.toml):

   ```toml
   [[service]]
   name = "web_api"
   description = "Web API Server"
   start = "${RATA}/bin/api --port 8080"
   ```

3. Check it, then start your services:

   ```sh
   rata check
   rata start
   ```

## Commands

| Command | What it does |
|---|---|
| `rata start [NAME]...` | Start services that are not already running |
| `rata stop [NAME]... [-t SECS]` | Stop running services (reverse config order) |
| `rata restart [NAME]... [-t SECS]` | Stop, then start services |
| `rata status [NAME]... [-v] [--json]` | Show whether each service is running |
| `rata check` | Validate the config and show what each service matches |
| `rata help [COMMAND]` | Show help (also `-h`, `--help`) |

Without names, a command acts on every configured service. `-q`/`--quiet`
prints errors only. `-V`/`--version` shows the version.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | One or more services failed to start or stop |
| 2 | Config or environment error (e.g. `RATA` not set, invalid `service.toml`) |
| 3 | `status` only: one or more services are not running |

## Configuration

`$RATA/work/cfg/service.toml`:

```toml
version = 1                                  # optional; only 1 is accepted

[[service]]
name         = "web_api"                     # required
description  = "Web API Server"              # optional
start        = "${RATA}/bin/api --port 8080" # required; string or array
psline       = "..."                         # optional; defaults to start
stop_timeout = 10                            # optional; seconds (1-3600)
```

| Field | Rules |
|---|---|
| `name` | Letters, digits, and `_`, not starting with a digit; at most 64 characters; unique |
| `description` | A short name, on a single line |
| `start` | The command `rata start` runs: a string, or an array of arguments |
| `psline` | The command line that identifies the running process, if it differs from `start` |
| `stop_timeout` | Seconds to wait for a graceful stop before force-killing (default 10) |

Unknown fields are errors, so typos are caught. Errors include the line and
column.

### How commands run

- `${RATA}` is replaced with the `RATA` directory. No other variables are
  expanded.
- A string is split into arguments using Unix shell quoting on Linux and
  macOS, and Windows quoting on Windows (a backslash is literal there). An
  array is used exactly as written.
- **No shell is involved.** For pipes, `&&`, or redirection, run a shell
  explicitly, such as `["sh", "-c", "..."]`, and set `psline`.
- Services run in `$RATA`, inherit rata's environment, have stdin closed, and
  write stdout and stderr to a new log file for every start:
  `$RATA/work/log/<name>_YYYYMMDD_HHMMSS.n.log` (local time; `n` starts at 0
  and increases if the name is taken).

### How rata finds a service's process

A process belongs to a service when its command line **exactly** matches the
service's `psline` (or `start`, if there is no `psline`). This works for
processes started by hand, too: rata detects them, and `rata stop` stops them.

- Only processes owned by the current user are considered.
- The arguments after the program must match exactly, with case mattering.
  Extra or missing arguments do not match.
- A program given as a path (`/opt/rata/bin/api`) must match that path exactly.
  A bare program name (`java`) matches any directory.
- On Windows, the program name ignores case and an optional `.exe` extension.
- For programs that rewrite their own process title (such as nginx), put the
  title in `psline`.

Writing `start` with a full path, such as `${RATA}/bin/api`, makes each
install's services distinct even when several installs share a machine.

When something is not detected, `rata status -v` and `rata check` list
**near misses**: processes running the same program with different
arguments, with their full command lines.

### Starting and stopping

- **Starting:** a service already running is skipped. Each new process is
  detached from the terminal. After 500ms, rata reports `FAILED` if the
  process has exited or if no process matches its `psline`.
- **Stopping:** rata sends SIGTERM (Windows: CTRL_BREAK), waits
  `stop_timeout`, then force-kills (SIGKILL / `TerminateProcess`). Services
  rata started are stopped together with their child processes. On Windows,
  a process started by hand in a shared console window is force-stopped
  directly, because a graceful signal would also reach the shell.
- Only one `start`, `stop`, or `restart` runs at a time per `RATA` directory.
  Others wait up to 30 seconds (`RATA_LOCK_TIMEOUT` overrides this, in
  seconds). `status` and `check` never wait.

## Directory layout

```text
$RATA/
└── work/
    ├── cfg/service.toml   service configuration (you create this)
    ├── log/               one log file per start (created by rata)
    └── run/               lock file (created by rata)
```

## Using rata as a library

```toml
[dependencies]
rata = { version = "0.1", default-features = false, features = ["service"] }
```

| Feature | Provides |
|---|---|
| *(none)* | `env`, `config`, `cmdline`, `logfile`: paths, config parsing, command matching |
| `service` | `process`, `service`, `lock`: finding, starting, and stopping processes |
| `cli` *(default)* | The `rata` command-line interface (implies `service`) |

```rust,no_run
use rata::{Config, Rata};

fn main() -> rata::Result<()> {
    let rata = Rata::from_env()?;
    let config = Config::load(&rata)?;
    let services = config.select(&[])?;
    for s in rata::service::status(&rata, &services) {
        println!("{}: {:?}", s.name, s.state);
    }
    Ok(())
}
```

The API may change between minor versions before 1.0.

## Development

```sh
cargo test                       # unit + end-to-end tests (builds examples/testsvc)
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
```

Releases: update `version` in `Cargo.toml` and `CHANGELOG.md`, commit, run
`cargo publish`, then push a `vX.Y.Z` tag. The tag runs
`.github/workflows/release.yml` (generated by [`dist`](https://opensource.axo.dev/cargo-dist/);
edit `dist-workspace.toml` and run `dist generate`, not the workflow), which
builds the binaries, creates the GitHub Release, and updates the Homebrew
formula in [markuskimius/homebrew-tap](https://github.com/markuskimius/homebrew-tap).

`examples/testsvc.rs` is a fake service used by the end-to-end tests, not a
usage example.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
