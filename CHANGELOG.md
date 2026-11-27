# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/markuskimius/rata/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/markuskimius/rata/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/markuskimius/rata/releases/tag/v0.1.0
