//! `agpeek` — inspect iOS App Group containers.
//!
//! See `PRD.md` for the product shape. `main` does routing only; discovery lives in
//! [`discover`] and all rendering in [`ui`].

mod cli;
mod discover;
mod ui;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::ui::Column;

fn main() {
    let cli = Cli::parse();
    ui::init_color(cli.no_color);

    if let Err(error) = run(&cli) {
        ui::print_error(&error);
        std::process::exit(1);
    }
}

/// Routes to the requested subcommand.
fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Devices => devices(cli),
    }
}

/// Lists every inspectable simulator.
fn devices(cli: &Cli) -> Result<()> {
    let devices = discover::devices()?;

    if cli.json {
        return print_json(&devices);
    }

    if devices.is_empty() {
        anstream::println!("No simulators available.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = devices
        .iter()
        .map(|device| {
            vec![
                device.name.clone(),
                device.runtime.clone(),
                device.state.clone(),
                device.udid.clone(),
            ]
        })
        .collect();

    anstream::print!(
        "{}",
        ui::table(
            &[
                Column::new("NAME"),
                Column::new("RUNTIME"),
                Column::new("STATE"),
                Column::dim("UDID"),
            ],
            &rows,
        )
    );
    Ok(())
}

/// Writes a value to stdout as pretty-printed JSON.
fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
