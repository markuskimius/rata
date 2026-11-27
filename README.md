# rata

Start, stop, and monitor long-running services.

`rata` is a command-line tool that manages the services listed in a TOML
config file, and a Rust library for programs that work with rata installs.
Its built-in monitor records host and per-service stats as CSV files you can
open in a spreadsheet. It runs on Linux, macOS, and Windows, on x86_64 and
ARM64.

```console
$ rata start
WEB_API  started           pid 4121  log work/log/WEB_API_20261125_143207.0.log
WORKER   already running   pid 3010
CACHE    FAILED            exited within 500ms (code 1), see work/log/CACHE_20261125_143207.0.log

$ rata status
NAME          STATE    PID   DESCRIPTION
RATA_MONITOR  running  4100  rata Monitor
WEB_API       running  4121  Web API Server
WORKER        running  3010  Background Job Worker
CACHE         stopped  -     Cache Server

$ rata mon WEB_API WORKER
Sampled 2026-11-26 14:30:00.000000000 -0500   12s ago, every 30s

SERVICE  STATE     PID  PROCS   CPU      RSS  UPTIME
WEB_API  running  4121      3  3.1%   82 MiB  2h 01m
WORKER   running  3010      1  0.4%  310 MiB  6d 03:10
```

## Install

Prebuilt binaries for Linux, macOS, and Windows (x86_64 and ARM64) are on
[GitHub Releases](https://github.com/markuskimius/rata/releases). The
installers put `rata` and `rata-monitor` in `~/.cargo/bin`:

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

2. Set it up. `rata init` creates the directory layout, a `service.toml`
   that runs the rata monitor, and a commented `monitor.toml`:

   ```sh
   rata init                        # or: rata init /opt/rata
   ```

3. Add your services to `$RATA/work/cfg/service.toml` (see
   [`docs/service.example.toml`](docs/service.example.toml)):

   ```toml
   [[service]]
   name = "WEB_API"
   description = "Web API Server"
   start = "${RATA}/bin/api --port 8080"
   ```

4. Check the config, start your services, and watch them:

   ```sh
   rata check
   rata start
   rata mon --watch
   ```

## Commands

| Command | What it does |
|---|---|
| `rata start [NAME]... [--all]` | Start services that are not already running |
| `rata stop [NAME]... [--all] [-t SECS]` | Stop running services (reverse config order) |
| `rata restart [NAME]... [--all] [-t SECS]` | Stop, then start services |
| `rata status [NAME]... [-v] [--json]` | Show whether each service is running |
| `rata mon [NAME]... [--json] [-w]` | Show the latest host and service stats from the monitor |
| `rata check` | Validate the config and show what each service matches |
| `rata init [DIR]` | Set up a new install in an empty directory |
| `rata help [COMMAND]` | Show help (also `-h`, `--help`) |

Without names, a command acts on every configured service, except that
`start`, `stop`, and `restart` skip services that set `explicit_start` or
`explicit_stop` (see [Configuration](#configuration)); `--all` includes
them. A name ignores case when only one service matches it, so `rata stop
web_api` stops `WEB_API`. `-q`/`--quiet` prints errors only. `-V`/`--version`
shows the version.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | One or more services failed to start or stop |
| 2 | Config or environment error (e.g. `RATA` not set, invalid `service.toml`) |
| 3 | `status`: one or more services are not running. `mon`: the data is out of date, or a service shown is not running |

## Configuration

`$RATA/work/cfg/service.toml`:

```toml
version = 1                                    # optional; only 1 is accepted

[[service]]
name           = "WEB_API"                     # required
description    = "Web API Server"              # optional
start          = "${RATA}/bin/api --port 8080" # required; string or array
psline         = "..."                         # optional; defaults to start
stop_timeout   = 10                            # optional; seconds (1-3600)
explicit_start = false                         # optional
explicit_stop  = false                         # optional
```

| Field | Rules |
|---|---|
| `name` | Letters, digits, and `_`, not starting with a digit; at most 64 characters; unique |
| `description` | A short name, on a single line |
| `start` | The command `rata start` runs: a string, or an array of arguments |
| `psline` | The command line that identifies the running process, if it differs from `start` |
| `stop_timeout` | Seconds to wait for a graceful stop before force-killing (default 10) |
| `explicit_start` | `true`: `rata start` and `rata restart` without names skip this service |
| `explicit_stop` | `true`: `rata stop` and `rata restart` without names skip this service |

Unknown fields are errors, so typos are caught. Errors include the line and
column.

By convention, service names are ALL CAPS, and services that come with rata
start with `RATA_`. rata does not enforce either convention.

A service with `explicit_start` or `explicit_stop` is included when you name
it (`rata stop RATA_MONITOR`) or pass `--all` (`rata stop --all`). A bare
command lists the services it skipped:

```console
$ rata stop
CACHE         not running
WEB_API       stopped           pid 4121
RATA_MONITOR  skipped           explicit_stop; stop it with: rata stop RATA_MONITOR
```

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
  seconds). `status`, `mon`, and `check` never wait.

## Monitoring

`rata-monitor` records host and per-service stats. `rata init` adds it to
`service.toml` as the `RATA_MONITOR` service:

```toml
[[service]]
name          = "RATA_MONITOR"
description   = "rata Monitor"
start         = "rata-monitor"
explicit_stop = true
```

With `explicit_stop`, a bare `rata stop` leaves the monitor running, so it
records your other services stopping and starting.

### `rata mon`

```console
$ rata mon
Sampled 2026-11-26 14:30:00.000000000 -0500   12s ago, every 30s

HOST     mbp.local   macOS 15.6 Sequoia   up 6d 03:12
CPU      12.4%   load 1.82 1.64 1.55   10 cores
MEMORY   9.0 GiB of 16 GiB (56%)   swap 0 B of 2.0 GiB

DISK             SIZE     USED     FREE  USE%   READ/S  WRITE/S
/             494 GiB  212 GiB  282 GiB   43%  1.2 MiB  340 KiB

NETWORK     RX/S     TX/S  ERRORS  DROPPED
en0      1.6 KiB    412 B       0        0

SERVICE       STATE     PID  PROCS   CPU      RSS     UPTIME
RATA_MONITOR  running  4100      1  0.2%   12 MiB  6d 03:10
WEB_API       running  4121      3  3.1%   82 MiB     2h 01m
CACHE         stopped     -      -     -        -          -
```

| Option | What it does |
|---|---|
| `NAME...` | Show only these services (case ignored), without the host sections |
| `--json` | Raw values: bytes as integers, I/O as bytes during the interval, times in the [rata datetime format](#the-rata-datetime-format) |
| `-w`, `--watch` | Redraw every second until Ctrl-C |
| `-q`, `--quiet` | Print nothing; only set the exit status, for health checks |

`rata mon` reads the newest sample from `$RATA/work/monitor`; it does not
measure anything itself. CPU is a percentage of the whole host in the CPU
line, and of one core in the service table (a service using two full cores
shows 200%). Rates are per second, averaged over the interval. The data is
**out of date** when the newest sample is older than two intervals or the
monitor is not running: `rata mon` then prints a warning saying why and
exits with status 3.

### `monitor.toml`

The optional `$RATA/work/cfg/monitor.toml`
([`docs/monitor.example.toml`](docs/monitor.example.toml)):

```toml
version  = 1          # optional; only 1 is accepted
interval = 30         # seconds between samples, 5-3600
system   = true       # false: record per-service stats only

[disks]
exclude = []          # mount points to leave out (glob patterns: * and ?)

[net]
exclude = []          # network interfaces to leave out (glob patterns)
```

- Samples are taken on the clock: with `interval = 30`, at `:00` and `:30`
  past each minute. The first sample is taken at the first of those times
  at least 5 seconds after the monitor starts. It covers less than a whole
  interval, so its interval amounts (disk and network I/O, and each
  service's disk I/O) are left empty; everything else is recorded as usual.
- Pseudo filesystems (such as `tmpfs`, `devfs`, and macOS's
  `/System/Volumes/*`) and loopback interfaces are always left out, as are
  interfaces that have never sent or received anything.
- **Several installs on one machine** each run a monitor. Set `system =
  false` in all but one of them, so the host is recorded only once.
- `rata check` validates the file. The monitor reads it when it starts, so
  run `rata restart RATA_MONITOR` after a change. It reads `service.toml`
  before every sample, so new services show up without a restart.

### The CSV files

The monitor appends to one file per kind of row per local day, in
`$RATA/work/monitor`:

| File | One row per | Columns after `time` |
|---|---|---|
| `system_YYYYMMDD.csv` | sample | `cpu_pct, load1, load5, load15, cores, mem_total, mem_used, mem_avail, swap_total, swap_used, uptime_s` |
| `disk_YYYYMMDD.csv` | filesystem | `mount, fs, total, used, avail, read, written` |
| `net_YYYYMMDD.csv` | network interface | `iface, rx_bytes, tx_bytes, rx_packets, tx_packets, rx_errors, tx_errors, rx_dropped, tx_dropped` |
| `service_YYYYMMDD.csv` | configured service | `service, state, pid, procs, cpu_pct, rss, disk_read, disk_written, uptime_s` |

```csv
time,service,state,pid,procs,cpu_pct,rss,disk_read,disk_written,uptime_s
2026-11-26 14:30:00.000000000 -0500 ,WEB_API,running,4121,3,3.1,85983232,0,4096,7260
2026-11-26 14:30:00.000000000 -0500 ,CACHE,stopped,,,,,,,
```

- Each file starts with a header row. Values are separated by commas; sizes
  are in bytes. `read`, `written`, `rx_*`, `tx_*`, and `disk_*` are amounts
  during the interval that ended at `time`, so they can be summed or charted
  directly. An empty value means "not available" (for example, the load
  average on Windows, or the interval amounts in the first sample after the
  monitor starts).
- A service row sums the service's whole process tree: the matching
  processes and their children (on Windows, not counting the `conhost.exe`
  console host that rata gives each service). Stopped services get a row too, with
  `state` set to `stopped`.
- `mem_used` is `mem_total - mem_avail`.
- `rx_errors` and `tx_errors` are the interface's error counters: packets
  received damaged or malformed (CRC, framing, length errors, overruns)
  and packets that could not be sent. They do not include dropped packets,
  which are in `rx_dropped` and `tx_dropped`: packets discarded without an
  error, such as when a queue or buffer was full. These come from
  `/sys/class/net/<iface>/statistics` on Linux, the interface's input queue
  drops and send queue drops on macOS, and `InDiscards` and `OutDiscards` on
  Windows. `rata mon` shows each pair summed, as `ERRORS` and `DROPPED`.
- Every sample is written and flushed as it is taken, so a crash loses at
  most the sample in progress.
- **The monitor never deletes files.** It writes only today's files; once a
  day is over, its files never change, so it is safe to archive or delete
  them. If a new rata version changes a file's columns during a day, it
  continues in `<name>_YYYYMMDD.1.csv`.

Spreadsheets read the files directly, but treat the `time` column as text.
To get a date and time, use a formula such as `=--LEFT(A2,19)`.

### The rata datetime format

Timestamps that rata writes, such as the `time` column and the times in
`rata mon` and the monitor's log, use the rata datetime format:

```text
YYYY-MM-DD HH:MM:SS.nnnnnnnnn ±HHMMf        2026-11-26 14:30:00.000000000 -0400s
```

The time is local, with nanoseconds, followed by the UTC offset in effect
and a flag: `s` when daylight saving time is in effect, a space when it is
standard time. The offset makes every value convertible to another time
zone, and unique even when clocks go back.

### Upgrading on Windows

Windows does not let a running program's file be replaced. So that a
running monitor never blocks an upgrade, when a service's `start` program
is a bare name (no directory) of a program installed next to `rata.exe`,
such as `rata-monitor`, `rata start` copies it to `$RATA\work\run\bin` and
runs the copy. After an upgrade, `rata restart RATA_MONITOR` runs the new
version. Linux and macOS need nothing like this.

### Upgrading an install from rata 0.1

`rata init` only sets up empty directories. To add the monitor to an
existing install, add the `RATA_MONITOR` service shown above at the top of
`service.toml`, then run `rata start RATA_MONITOR`. `monitor.toml` is
optional; copy [`docs/monitor.example.toml`](docs/monitor.example.toml) to
change the defaults.

## Directory layout

```text
$RATA/
└── work/
    ├── cfg/service.toml   service configuration
    ├── cfg/monitor.toml   monitor configuration (optional)
    ├── log/               one log file per start
    ├── monitor/           stats recorded by rata-monitor
    └── run/               lock file; on Windows, bin/ with program copies
```

`rata init` creates all of it. rata creates `log/` and `run/` if they are
missing, and the monitor creates `monitor/`.

## Using rata as a library

```toml
[dependencies]
rata = { version = "0.3", default-features = false, features = ["service"] }
```

| Feature | Provides |
|---|---|
| *(none)* | `env`, `config`, `cmdline`, `logfile`, `datetime`, `init`: paths, config parsing, command matching, the rata datetime format |
| `service` | `process`, `service`, `lock`: finding, starting, and stopping processes |
| `monitor` | `monitor` and the `rata-monitor` binary: sampling, the CSV files, `monitor.toml` (implies `service`) |
| `cli` *(default)* | The `rata` command-line interface (implies `service` and `monitor`) |

```rust,no_run
use rata::{Config, Rata};

fn main() -> rata::Result<()> {
    let rata = Rata::from_env()?;
    let config = Config::load(&rata)?;
    let services = config.select(&[])?;
    for s in rata::service::status(&rata, &services) {
        println!("{}: {:?}", s.name, s.state);
    }
    if let Some(sample) = rata::monitor::latest(&rata) {
        println!("latest sample: {}", sample.time);
    }
    Ok(())
}
```

For a tool that cleans up old files, `rata::monitor::live_files` and
`rata::service::current_logs` list the files that may still be written to.

The API may change between minor versions before 1.0.

## Development

```sh
cargo test                       # unit + end-to-end tests (builds examples/testsvc)
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
```

Releases: update `version` in `Cargo.toml` and `CHANGELOG.md`, commit, then
push a `vX.Y.Z` tag. The tag runs `.github/workflows/release.yml`, which
does the following:
- Builds the binaries and creates the GitHub Release.
- Updates the Homebrew formula in
  [markuskimius/homebrew-tap](https://github.com/markuskimius/homebrew-tap).
- Publishes to crates.io using trusted publishing.

The workflow is generated by [`dist`](https://opensource.axo.dev/cargo-dist/).
To change it, edit `dist-workspace.toml` and run `scripts/dist-generate.sh`.

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
