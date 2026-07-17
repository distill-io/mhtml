//! Library backing the `mhtml` CLI binary. Holds the extraction-support
//! modules (naming, reference resolution) and the subcommand implementations so
//! that `main.rs` stays a thin clap adapter and integration tests can drive
//! the real logic.

pub mod extract;
pub mod list;
pub mod naming;
pub mod resolve;
