# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/markuskimius/rata/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/markuskimius/rata/releases/tag/v0.1.0
