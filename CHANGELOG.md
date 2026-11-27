# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

To add archiving to an existing install, copy `docs/init.archive.toml` to
`$RATA/work/cfg/archive.toml`.

### Added

- `rata archive` compresses (gzip), moves, or deletes old files, following
  rules in the new `$RATA/work/cfg/archive.toml`. Each rule names a
  directory and file name patterns, with `compress_after`, and either
  `delete_after` or `move_after` with `move_to`. `move_to` takes `{yyyy}`,
  `{mm}`, and `{dd}` from the file's modified date. Leaving out
  `compress_after` moves or deletes files as they are. `exclude` leaves out
  files that `match` would take. Patterns support `*`, `?`, `[a-z]`,
  `[!a-z]`, `{a,b}`, and `\` escapes. Ages (`12h`, `3d`, `2w`, `6mo`, `1y`,
  or spelled out, like `90 days`) count from each file's last-modified time,
  which is kept; all but hours go by the calendar. Every action
  applies only to files in the rule's directory, and rata never touches
  what it moved. The current log of a running service and today's monitor
  files are never touched. `-n`/`--dry-run` shows what would happen. Runs
  are safe to interrupt and never overlap (`work/run/archive.lock`), so
  `rata archive -q` can run from cron. Exit code 1 means a file could not be
  handled.
- `rata init` writes an `archive.toml` that compresses logs and monitor
  files after 3 days and deletes them after 90.
- `rata check` validates `archive.toml` and lists its rules.
- Library: the `archive` feature and module (included in `cli`), with
  `Lock::acquire_named`, `Rata::archive_config_path`,
  `init::ARCHIVE_TEMPLATE`, and `Error::ArchiveBusy`.

## [0.3.0] - 2026-11-27

### Added

- Network stats record dropped packets: new `rx_dropped` and `tx_dropped`
  columns in `net_YYYYMMDD.csv` (packets discarded without an error, per
  interval), on Linux, macOS, and Windows. `rata mon` shows them summed in a
  new `DROPPED` column, and `--json` includes both. Files written by an
  older version during the same day continue in `net_YYYYMMDD.1.csv`.

### Changed

- Library: `monitor::sample::NetRow` has two new fields, `rx_dropped` and
  `tx_dropped` (a breaking change for code that builds `NetRow` values).

## [0.2.1] - 2026-11-26

### Changed

- `rata-monitor` takes its first sample at the first clock boundary at least
  5 seconds after it starts (within 5 to 35 seconds with the default
  30-second interval), instead of waiting a whole interval (30 to 60
  seconds). That first sample covers less than an interval, so its interval
  amounts (disk and network I/O, and per-service disk I/O) are left empty.

## [0.2.0] - 2026-11-26

To add the monitor to an install set up with 0.1, add the `RATA_MONITOR`
service to the top of `service.toml` (see "Upgrading an install from rata
0.1" in the README) and run `rata start RATA_MONITOR`.

### Added

- `rata init [DIR]` sets up a new install in an empty directory (hidden
  files do not count): the `work/` tree, a `service.toml` that runs the rata
  monitor (`RATA_MONITOR`, with `explicit_stop = true`), and a commented
  `monitor.toml`. Without `DIR` it uses `$RATA`.
- The rata monitor: `rata-monitor`, a second binary, records host stats
  (CPU, load, memory, swap, disks, network) and per-service stats (state,
  process count, CPU, memory, disk I/O, uptime) every 30 seconds, as daily
  CSV files in `$RATA/work/monitor`. Run it as the `RATA_MONITOR` service.
  Settings are in the optional `$RATA/work/cfg/monitor.toml` (`interval`,
  `system`, `[disks] exclude`, `[net] exclude`). It never deletes files;
  older days' files never change.
- `rata mon [NAME]...` shows the newest sample: host CPU, load, memory,
  disks, and network, and each service's state, PID, process count, CPU,
  memory, and uptime. `--json` prints raw values, `-w`/`--watch` redraws
  every second, and `-q` only sets the exit status. Naming services shows
  only those. Exit status 3 means the data is out of date (older than two
  intervals, or the monitor is not running) or a service shown is not
  running.
- `monitor` library feature (`rata::monitor`), included in `cli`.
- `rata check` also validates `monitor.toml`.
- Windows: a service whose program is a bare name of a program installed
  next to `rata.exe` (such as `rata-monitor`) runs from a copy in
  `$RATA\work\run\bin`, so a running service never blocks an upgrade.
- `service::current_logs` lists the log files that running services may
  still write to.
- `explicit_start` and `explicit_stop` service options: `start`, `stop`, and
  `restart` without names skip such services. Name them, or pass the new
  `--all` option, to include them. Skipped services are listed in the output.
- `rata check` shows which services set `explicit_start` or `explicit_stop`.
- `rata::datetime`: the rata datetime format
  (`2026-11-26 14:30:00.000000000 -0400s`), used by default for timestamps
  that rata writes.

### Changed

- Service names on the command line ignore case when exactly one service
  matches (an exact match always wins). A name that matches several services
  is an error.
- Docs and examples use ALL CAPS service names, the new convention.

## [0.1.1] - 2026-11-26

### Added

- Homebrew formula: `brew install markuskimius/tap/rata`.

## [0.1.0] - 2026-11-26

### Added

- `rata start`, `stop`, `restart`, `status`, `check`, and `help`.
- Service config in `$RATA/work/cfg/service.toml` (`name`, `description`,
  `start`, `psline`, `stop_timeout`).
- Exact command-line matching, including processes started without rata.
- A new log file per start: `$RATA/work/log/<name>_YYYYMMDD_HHMMSS.n.log`.
- `status --json` and `status -v` (matched processes and near misses).
- Library API (`config`, `cmdline`, `process`, `service`) for other rata packages.
- Prebuilt binaries for Linux, macOS, and Windows (x86_64 and ARM64) on
  GitHub Releases, with shell and PowerShell installers.

[Unreleased]: https://github.com/markuskimius/rata/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/markuskimius/rata/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/markuskimius/rata/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/markuskimius/rata/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/markuskimius/rata/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/markuskimius/rata/releases/tag/v0.1.0
