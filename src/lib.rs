//! # rata
//!
//! Start, stop, and monitor long-running services.
//!
//! This crate is both the `rata` command-line tool and a library for
//! programs that work with rata installs.
//!
//! Services are defined in `$RATA/work/cfg/service.toml`. A service is
//! running when a process owned by the current user has a command line that
//! exactly matches the service's `psline` (or its `start` command). See
//! [`cmdline`] for the exact matching rules.
//!
//! ## Features
//!
//! | Feature | Default | Provides |
//! |---|---|---|
//! | *(none)* | | [`env`](mod@env), [`config`], [`cmdline`], [`logfile`], [`Error`] |
//! | `service` | | `process`, `service`, `lock`: finding, starting, and stopping processes |
//! | `cli` | yes | `cli` and the `rata` binary (implies `service`) |
//!
//! Library users that do not need the CLI can depend on
//! `rata = { version = "0.1", default-features = false, features = ["service"] }`.

#![warn(missing_docs)]

pub mod cmdline;
pub mod config;
pub mod datetime;
pub mod env;
pub mod error;
pub mod logfile;

#[cfg(feature = "service")]
pub mod lock;
#[cfg(feature = "service")]
pub mod process;
#[cfg(feature = "service")]
pub mod service;

#[cfg(feature = "cli")]
pub mod cli;

pub use config::{CommandSpec, Config, Service};
pub use env::Rata;
pub use error::{Error, Result};
