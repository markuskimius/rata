# rata — notes for Claude

`rata` is one package holding both the library and the CLI, with no
workspace. User-facing docs are in
`README.md`; keep it, `rata help` (`src/cli/mod.rs`), and `CHANGELOG.md` in
sync with any behavior change.

## Layout

- `src/env.rs`: `$RATA` validation and paths (`work/cfg`, `work/log`,
  `work/run`, `work/monitor`).
- `src/config.rs`: `service.toml` parsing and validation. Errors carry
  line and column. Unknown fields are rejected.
- `src/cmdline.rs`: splitting (POSIX on Unix, CommandLineToArgvW rules on
  Windows), `${RATA}` expansion, and **exact** command-line matching.
- `src/logfile.rs`: `<name>_YYYYMMDD_HHMMSS.n.log`, local time (jiff).
- `src/datetime.rs`: the rata datetime format
  (`2026-11-26 14:30:00.000000000 -0400s`; `s` = DST, a space = standard
  time). Use it for every timestamp rata writes (not file names).
- `src/init.rs`: `rata init`; templates are `docs/init.service.toml`,
  `docs/monitor.example.toml`, and `docs/init.archive.toml` (all
  `include_str!`, tested as valid).
- `src/process/`: `mod.rs` holds the cross-platform scan (sysinfo);
  `unix.rs` handles setsid and signals; `windows.rs` handles the new
  process group, the CTRL_BREAK helper (`rata __ctrl-break`, hidden) and
  TerminateProcess.
- `src/service.rs`: start, stop, restart, status and check logic, with
  typed reports. `config.rs` `select_for` applies `explicit_start` /
  `explicit_stop` / `--all`; names on the command line match case-insensitively
  when unique.
- `src/monitor/` (feature `monitor`): `collect.rs` (sysinfo sampling),
  `store.rs` (daily CSV files: append-only, header changes start `.1.csv`,
  tail reads), `sample.rs` (row types and columns), `csv.rs`, `config.rs`
  (`monitor.toml`), `signal.rs` (graceful stop), `mod.rs` (the run loop).
- `src/bin/rata-monitor.rs`: the `RATA_MONITOR` service's binary.
- `src/archive/` (feature `archive`): `config.rs` (`archive.toml`: rules,
  calendar ages `h`/`d`/`w`/`mo`/`y`, `move_to` templates), `glob.rs`
  (extended globs: `[a-z]`, `{a,b}`, `\` escapes; the monitor keeps its
  simpler matcher because Windows mount points contain `\`), `mod.rs` (`decide` is the pure per-file
  decision; `run` scans, then compresses, moves, or deletes). Every action
  is on files in a rule's `path`; nothing under `move_to` is ever touched
  after a move, so `move_after` and `delete_after` are mutually exclusive.
- `src/cli/mon.rs`: `rata mon` (text, `--json`, `--watch`).
- `src/lock.rs`: `work/run/rata.lock` (`File::try_lock`).
  `RATA_LOCK_TIMEOUT` sets the wait. `rata archive` takes `archive.lock`
  without waiting.
- `src/cli/`: clap definitions with custom help templates; `output.rs`
  does all printing.
- `examples/testsvc.rs`: a fake service for the end-to-end tests (not a
  usage example).
- `tests/cli.rs`: end-to-end tests through the real binaries. Monitor tests
  copy `rata-monitor` into each test's `$RATA/bin` so parallel tests do not
  match each other's monitor. `tests/config.rs`
  covers config parsing.

## Rules

- Minimum Rust is 1.89, because of `File::try_lock` (and sysinfo 0.38 needs
  1.88). Do not bump sysinfo past 0.38 without raising the minimum.
- Features: `cli` (the default) implies `service`, `monitor`, and
  `archive`; `archive` implies `monitor` (and adds flate2); `monitor`
  implies `service`; the bare library has no process code. CI checks the
  `--no-default-features`, `--features service`, `--features monitor`,
  and `--features archive` builds.
- Long-running services that ship with rata get their own binary (like
  `rata-monitor`), never a subcommand of `rata`. On Windows, `rata start`
  runs a bare-named program installed next to `rata.exe` from a copy in
  `$RATA\work\run\bin` (`bundled_copy` in `process/windows.rs`), so a
  running service never blocks an upgrade.
- Conventions: service names are ALL CAPS; suite services start with
  `RATA_`. Neither is enforced or special-cased in code.
- The monitor never deletes files (retention is `rata archive`).
  Only today's files are written.
- Exit codes: 0 ok, 1 a service failed, 2 config or environment error, 3
  `status` found a service not running, or `mon` found out-of-date data or
  a service not running.
- Windows: services must not inherit rata's std handles
  (`disinherit_std_handles`), or anything reading rata's output hangs.
  `TerminateProcess` gives "access denied" on a process that is already
  exiting; treat that as stopped.
- The local toolchain is Homebrew Rust with no rustup, so Linux and
  Windows code is checked only in CI (`.github/workflows/ci.yml`, 6
  runners). Verify the windows-sys symbols against the registry source
  before pushing.
- Before committing, run: `cargo fmt`,
  `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`.

## Releasing

1. Bump `version` in `Cargo.toml` (semver; pre-1.0, a breaking API change
   bumps the minor version). Move the `CHANGELOG.md` "Unreleased" entries
   under the new version and update the compare links at the bottom.
2. Commit and push to `main`, then wait for CI to pass.
3. Run `git tag -a vX.Y.Z -m "rata X.Y.Z" && git push origin vX.Y.Z`.
   This starts `release.yml`, which does everything else, in this order:
   - Builds the 6 targets and the shell and PowerShell installers, then
     creates the GitHub Release.
   - Pushes `Formula/rata.rb` to `markuskimius/homebrew-tap`, using the
     `HOMEBREW_TAP_TOKEN` secret (a fine-grained token with write access to
     the tap's contents; if it expires, this job fails).
   - Runs `cargo publish` via `publish-crates.yml`, using crates.io trusted
     publishing with no stored token. The crates.io config names workflow
     `release.yml` (the caller, from the OIDC `workflow_ref` claim) and
     environment `release`, which GitHub restricts to `v*` tags. Do not
     publish from a laptop.
4. Install locally with `brew upgrade rata` or `cargo install rata`.

`release.yml` is generated by `dist` from `dist-workspace.toml`. Run
`scripts/dist-generate.sh`, never plain `dist generate` and never hand
edits. The script patches two bugs in dist 0.34's custom-job output (a
`$/` path, and a missing `contents: read`). Because of that,
`allow-dirty = ["ci"]` is set. When upgrading dist, check whether the
patches are still needed.
