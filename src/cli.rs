//! Command-line surface for `agpeek`.
//!
//! This module holds the clap definitions and nothing else — no I/O, no logic.
//! Subcommand help text comes from the `///` doc comments on each variant.

use clap::{Parser, Subcommand};

/// Inspect iOS App Group containers.
#[derive(Debug, Parser)]
#[command(name = "agpeek", version, about, long_about = None)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,

    /// Simulator to inspect, by UDID or name (defaults to the only booted device)
    #[arg(long, global = true, value_name = "UDID|NAME")]
    pub device: Option<String>,

    /// Emit machine-readable JSON instead of a formatted table
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable coloured output
    #[arg(long, global = true)]
    pub no_color: bool,
}

/// Subcommands exposed by the binary.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List simulators that can be inspected
    Devices,

    /// List the App Groups an installed app declares
    Groups {
        /// Bundle identifier of the app, e.g. `app.natively`
        bundle_id: String,
    },
}
