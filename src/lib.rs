//! DiskAnalyzer: a read-only inventory of what is using space on a drive.
//!
//! The crate never deletes, moves, renames or modifies anything. The only
//! files it creates are the reports, in the output directory you chose
//! (see `reports::sink`).

pub mod categories;
pub mod cli;
pub mod exclusions;
pub mod insights;
pub mod platform;
pub mod projects;
pub mod reports;
pub mod scanner;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
