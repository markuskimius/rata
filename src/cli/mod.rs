//! The `rata` command-line interface.
//!
//! [`run`] parses arguments, calls into [`crate::service`], and formats the
//! results. Exit codes:
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | Success |
//! | 1 | One or more services failed to start or stop |
//! | 2 | Config or environment error |
//! | 3 | (`status` only) One or more services are not running |

use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgAction, ColorChoice, CommandFactory, Parser, Subcommand};

use crate::config::{Action, Config, MAX_STOP_TIMEOUT};
use crate::env::Rata;
use crate::error::Error;
use crate::lock::Lock;
use crate::service;

mod mon;
mod output;

use output::Printer;

/// Exit code: success.
pub const EXIT_OK: u8 = 0;
/// Exit code: a service failed to start or stop.
pub const EXIT_FAILED: u8 = 1;
/// Exit code: config or environment error.
pub const EXIT_ERROR: u8 = 2;
/// Exit code: `status` found a service that is not running.
pub const EXIT_NOT_RUNNING: u8 = 3;

const TOP_TEMPLATE: &str = "\
{name} {version}
{about}

{usage-heading} {usage}

{all-args}{after-help}";

const SUB_TEMPLATE: &str = "\
{about}

{usage-heading} {usage}

{all-args}{after-help}";

const TOP_AFTER_HELP: &str = "\
Services:
  Commands act on all configured services, in config order (stop uses
  reverse order). Name one or more services to act on only those; a name
  ignores case when only one service matches it:

    rata start WEB_API WORKER

  Without names, start, stop, and restart skip services that set
  explicit_start or explicit_stop. Name them, or pass --all, to include them.

Environment:
  RATA  Required. Absolute path to the root directory of this rata install.
        Config is read from $RATA/work/cfg/service.toml
        Logs are written to $RATA/work/log/<name>_YYYYMMDD_HHMMSS.n.log

Exit codes:
  0  Success
  1  One or more services failed to start or stop
  2  Config or environment error (e.g. RATA not set, invalid service.toml)
  3  (status only) One or more services are not running

Run 'rata help <command>' for more on a specific command.";

const START_AFTER_HELP: &str = "\
Starts each service whose start command (or psline) does not match a running
process. Each start writes a new log file in $RATA/work/log. A service that
exits within 500ms, or whose process does not match its psline, is reported
as FAILED. Without names, services that set explicit_start are skipped.";

const STOP_AFTER_HELP: &str = "\
Stops every process whose command line exactly matches the service's
psline (or start, if psline is not set), including processes that were
started without rata. Without names, services that set explicit_stop are
skipped.";

const RESTART_AFTER_HELP: &str = "\
Stops the services in reverse config order, then starts the ones that
stopped in config order. A service that fails to stop is not started.
Without names, services that set explicit_start or explicit_stop are
skipped.";

const STATUS_AFTER_HELP: &str = "\
Exit status is 0 if all listed services are running, 3 if any are stopped.";

const MON_AFTER_HELP: &str = "\
Shows the newest sample that rata-monitor (the RATA_MONITOR service)
recorded in $RATA/work/monitor: host CPU, memory, disks, and network, and
each service's state, CPU, memory (RSS), and uptime. Rates are per second,
averaged over the sample interval. Naming services shows only those, without
host stats. The data is out of date when it is older than two intervals.

Exit status is 0 if the data is current and every service shown is running,
3 if not.";

const INIT_AFTER_HELP: &str = "\
Creates the work/ tree, a service.toml that runs the rata monitor
(RATA_MONITOR), and a commented monitor.toml. The directory must not exist
or must be empty; hidden files (names starting with a dot) do not count.
rata init never changes an existing install.";

const CHECK_AFTER_HELP: &str = "\
Validates $RATA/work/cfg/service.toml and monitor.toml, then shows each
service's start command and psline as rata splits them, the processes
matching right now, and near misses: processes running the same program with
different arguments. Exit status is 0 if the config is valid, 2 if not.";

#[derive(Parser, Debug)]
#[command(
    name = "rata",
    version,
    about = "Start, stop, and monitor long-running services",
    override_usage = "rata <COMMAND> [OPTIONS]",
    help_template = TOP_TEMPLATE,
    after_help = TOP_AFTER_HELP,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    color = ColorChoice::Never,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Only print errors
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Show help
    #[arg(short = 'h', long, global = true, action = ArgAction::Help)]
    help: Option<bool>,

    /// Show version
    #[arg(short = 'V', long, action = ArgAction::Version)]
    version: Option<bool>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start services that are not already running
    #[command(help_template = SUB_TEMPLATE, after_help = START_AFTER_HELP)]
    Start {
        /// Services to start (default: all except explicit_start ones, in
        /// config order)
        #[arg(value_name = "NAME")]
        names: Vec<String>,

        /// Also start services that set explicit_start
        #[arg(short, long, conflicts_with = "names")]
        all: bool,
    },

    /// Stop running services
    #[command(help_template = SUB_TEMPLATE, after_help = STOP_AFTER_HELP)]
    Stop {
        /// Services to stop (default: all except explicit_stop ones, in
        /// reverse config order)
        #[arg(value_name = "NAME")]
        names: Vec<String>,

        /// Also stop services that set explicit_stop
        #[arg(short, long, conflicts_with = "names")]
        all: bool,

        /// Seconds to wait after a graceful stop before force-killing
        /// [default: 10, or the service's stop_timeout]
        #[arg(short, long, value_name = "SECS", value_parser = parse_timeout, verbatim_doc_comment)]
        timeout: Option<u64>,
    },

    /// Stop, then start services
    #[command(help_template = SUB_TEMPLATE, after_help = RESTART_AFTER_HELP)]
    Restart {
        /// Services to restart (default: all except explicit_start and
        /// explicit_stop ones)
        #[arg(value_name = "NAME")]
        names: Vec<String>,

        /// Also restart services that set explicit_start or explicit_stop
        #[arg(short, long, conflicts_with = "names")]
        all: bool,

        /// Seconds to wait after a graceful stop before force-killing
        /// [default: 10, or the service's stop_timeout]
        #[arg(short, long, value_name = "SECS", value_parser = parse_timeout, verbatim_doc_comment)]
        timeout: Option<u64>,
    },

    /// Show whether each service is running
    #[command(help_template = SUB_TEMPLATE, after_help = STATUS_AFTER_HELP)]
    Status {
        /// Services to show (default: all)
        #[arg(value_name = "NAME")]
        names: Vec<String>,

        /// Show matched PIDs, full command lines, and near misses
        #[arg(short, long)]
        verbose: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show the latest host and service stats from the rata monitor
    #[command(help_template = SUB_TEMPLATE, after_help = MON_AFTER_HELP)]
    Mon {
        /// Services to show (default: all, with host stats)
        #[arg(value_name = "NAME")]
        names: Vec<String>,

        /// Output as JSON
        #[arg(long, conflicts_with = "watch")]
        json: bool,

        /// Redraw every second until interrupted (Ctrl-C)
        #[arg(short, long)]
        watch: bool,
    },

    /// Set up a new rata install in an empty directory
    #[command(help_template = SUB_TEMPLATE, after_help = INIT_AFTER_HELP)]
    Init {
        /// Directory to set up (default: $RATA). Must be absolute; it is
        /// created if missing
        #[arg(value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },

    /// Validate the config and show what each service matches
    #[command(help_template = SUB_TEMPLATE, after_help = CHECK_AFTER_HELP)]
    Check,

    /// Show this help, or help for a command
    #[command(help_template = SUB_TEMPLATE)]
    Help {
        /// Command to show help for
        #[arg(value_name = "COMMAND")]
        command: Option<String>,
    },

    /// Windows graceful-stop helper (internal)
    #[command(name = "__ctrl-break", hide = true)]
    CtrlBreak { pid: u32, allowed: Vec<u32> },
}

fn parse_timeout(s: &str) -> Result<u64, String> {
    let n: u64 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a number of seconds"))?;
    if (1..=MAX_STOP_TIMEOUT).contains(&n) {
        Ok(n)
    } else {
        Err(format!("must be between 1 and {MAX_STOP_TIMEOUT}"))
    }
}

/// Run the `rata` CLI with the process's arguments and return its exit code.
pub fn run() -> ExitCode {
    run_from(std::env::args_os())
}

/// Run the `rata` CLI with the given arguments (program name first).
pub fn run_from<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        crate::process::set_ctrl_break_helper(exe);
    }

    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(e.exit_code() as u8);
        }
    };
    ExitCode::from(match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_ERROR
        }
    })
}

fn load() -> Result<(Rata, Config), Error> {
    let rata = Rata::from_env()?;
    let config = Config::load(&rata)?;
    Ok((rata, config))
}

fn dispatch(cli: Cli) -> Result<u8, Error> {
    let quiet = cli.quiet;
    let Some(command) = cli.command else {
        eprintln!("error: no command given\nRun 'rata help' for usage.");
        return Ok(EXIT_ERROR);
    };
    match command {
        Command::Help { command } => Ok(help(command.as_deref())),
        Command::CtrlBreak { pid, allowed } => Ok(ctrl_break(pid, &allowed)),
        Command::Start { names, all } => {
            let (rata, config) = load()?;
            let sel = config.select_for(Action::Start, &names, all)?;
            let p = Printer::for_selection(&rata, &sel, quiet);
            if sel.services.is_empty() && sel.skipped.is_empty() {
                p.note("no services configured");
                return Ok(EXIT_OK);
            }
            let _lock = Lock::acquire(&rata, Lock::wait_limit())?;
            let reports = service::start(&rata, &sel.services)?;
            reports.iter().for_each(|r| p.start(r));
            p.skipped(Action::Start, &sel.skipped);
            Ok(if reports.iter().all(|r| r.ok()) {
                EXIT_OK
            } else {
                EXIT_FAILED
            })
        }
        Command::Stop {
            names,
            all,
            timeout,
        } => {
            let (rata, config) = load()?;
            let sel = config.select_for(Action::Stop, &names, all)?;
            let p = Printer::for_selection(&rata, &sel, quiet);
            if sel.services.is_empty() && sel.skipped.is_empty() {
                p.note("no services configured");
                return Ok(EXIT_OK);
            }
            let _lock = Lock::acquire(&rata, Lock::wait_limit())?;
            let reports = service::stop(
                &rata,
                &sel.services,
                timeout.map(Duration::from_secs),
                |r| p.stop(r),
            );
            p.skipped(Action::Stop, &sel.skipped);
            Ok(if reports.iter().all(|r| r.ok()) {
                EXIT_OK
            } else {
                EXIT_FAILED
            })
        }
        Command::Restart {
            names,
            all,
            timeout,
        } => {
            let (rata, config) = load()?;
            let sel = config.select_for(Action::Restart, &names, all)?;
            let p = Printer::for_selection(&rata, &sel, quiet);
            if sel.services.is_empty() && sel.skipped.is_empty() {
                p.note("no services configured");
                return Ok(EXIT_OK);
            }
            let _lock = Lock::acquire(&rata, Lock::wait_limit())?;
            let report = service::restart(
                &rata,
                &sel.services,
                timeout.map(Duration::from_secs),
                |r| p.stop(r),
            )?;
            report.started.iter().for_each(|r| p.start(r));
            p.skipped(Action::Restart, &sel.skipped);
            Ok(if report.ok() { EXIT_OK } else { EXIT_FAILED })
        }
        Command::Status {
            names,
            verbose,
            json,
        } => {
            let (rata, config) = load()?;
            let services = config.select(&names)?;
            let p = Printer::new(&rata, &services, quiet);
            let reports = service::status(&rata, &services);
            if json {
                p.status_json(&reports);
            } else if services.is_empty() {
                p.note("no services configured");
            } else {
                p.status(&reports, verbose);
            }
            Ok(
                if reports.iter().all(|r| r.state == service::State::Running) {
                    EXIT_OK
                } else {
                    EXIT_NOT_RUNNING
                },
            )
        }
        Command::Mon { names, json, watch } => mon::run(&mon::Options {
            names,
            json,
            watch,
            quiet,
        }),
        Command::Init { dir } => {
            let root = crate::init::target_dir(dir)?;
            let report = crate::init::init(&root)?;
            output::init(&report, quiet);
            Ok(EXIT_OK)
        }
        Command::Check => {
            let (rata, config) = load()?;
            let all: Vec<_> = config.services.iter().collect();
            let monitor = crate::monitor::MonitorConfig::load(&rata)?;
            let p = Printer::new(&rata, &all, quiet);
            p.check(&config, &service::check(&rata, &config));
            p.check_monitor(&monitor);
            Ok(EXIT_OK)
        }
    }
}

fn help(command: Option<&str>) -> u8 {
    let mut cmd = Cli::command();
    cmd.build();
    match command {
        None => {
            let _ = cmd.print_help();
            EXIT_OK
        }
        Some(name) => match cmd.find_subcommand_mut(name).filter(|c| !c.is_hide_set()) {
            Some(sub) => {
                let _ = sub.print_help();
                EXIT_OK
            }
            None => {
                eprintln!("error: unknown command '{name}'\nRun 'rata help' for usage.");
                EXIT_ERROR
            }
        },
    }
}

#[cfg(windows)]
fn ctrl_break(pid: u32, allowed: &[u32]) -> u8 {
    crate::process::ctrl_break_helper_main(pid, allowed) as u8
}

#[cfg(not(windows))]
fn ctrl_break(_pid: u32, _allowed: &[u32]) -> u8 {
    eprintln!("error: __ctrl-break is only used on Windows");
    EXIT_ERROR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
