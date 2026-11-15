//! Blackbox core: a flight recorder for gaming PCs.
//!
//! * `collectors` read the system state (drivers, updates, settings, game builds).
//! * `store` journals every difference between two scans.
//! * `monitor` records play sessions, `frames` turns captures into metrics.
//! * `analysis` flags regressions and ranks the changes that may explain them.

pub mod analysis;
pub mod collectors;
pub mod config;
pub mod error;
pub mod frames;
pub mod i18n;
pub mod launchers;
pub mod load;
pub mod model;
pub mod monitor;
pub mod notify;
pub mod report;
pub mod service;
pub mod steam;
pub mod store;
pub mod system;
pub mod vault;
pub mod vdf;

pub use error::{Error, Result};

/// Current time as Unix seconds.
pub fn now() -> model::Timestamp {
    chrono::Utc::now().timestamp()
}
