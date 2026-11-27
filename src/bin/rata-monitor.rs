//! `rata-monitor`: records host and per-service stats for a rata install.
//!
//! Run it as the `RATA_MONITOR` service (`rata init` sets this up); see
//! `rata::monitor`.

use std::process::ExitCode;

const USAGE: &str = "\
rata-monitor records host and per-service stats for the rata install in
$RATA, every 30 seconds by default, as CSV files in $RATA/work/monitor.

Run it as a rata service rather than by hand:

  [[service]]
  name          = \"RATA_MONITOR\"
  start         = \"rata-monitor\"
  explicit_stop = true

Settings are in $RATA/work/cfg/monitor.toml. View the stats with 'rata mon'.

Usage: rata-monitor

Options:
  -h, --help     Show help
  -V, --version  Show version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("rata-monitor {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("error: unexpected argument '{other}'\nRun 'rata-monitor --help' for usage.");
            return ExitCode::from(2);
        }
    }
    let result = rata::Rata::from_env().and_then(|rata| rata::monitor::run(&rata));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
