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
        Command::Groups { bundle_id } => groups(cli, bundle_id),
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

/// Lists the App Groups declared by an installed app.
fn groups(cli: &Cli, bundle_id: &str) -> Result<()> {
    let device = discover::select_device(discover::devices()?, cli.device.as_deref())?;
    let groups = discover::app_groups(&device, bundle_id)?;

    if cli.json {
        return print_json(&groups);
    }

    let rows: Vec<Vec<String>> = groups
        .iter()
        .map(|group| vec![group.id.clone(), group.path.display().to_string()])
        .collect();

    anstream::print!(
        "{}",
        ui::table(&[Column::new("GROUP"), Column::dim("PATH")], &rows)
    );
    Ok(())
}

/// Writes a value to stdout as pretty-printed JSON.
fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    anstream::println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
