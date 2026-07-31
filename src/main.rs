//! `agpeek` — inspect iOS App Group containers.
//!
//! See `PRD.md` for the product shape. `main` does routing only; discovery lives in
//! [`discover`] and all rendering in [`ui`].

mod cli;
mod discover;
mod source;
mod ui;

use std::path::Path;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::source::sim::{Container, EntryKind, WalkOptions};
use crate::ui::Column;
use crate::ui::tree::Node;

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
        Command::Ls {
            group_id,
            path,
            depth,
            all,
        } => ls(cli, group_id, path.as_deref(), *depth, *all),
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

/// Shows the file tree of a container.
fn ls(
    cli: &Cli,
    group_id: &str,
    path: Option<&Path>,
    depth: Option<usize>,
    all: bool,
) -> Result<()> {
    let device = discover::select_device(discover::devices()?, cli.device.as_deref())?;
    let resolved = discover::resolve_container(&device, group_id)?;

    let container = Container::new(resolved.path.clone());
    let start = container.resolve(path)?;
    let entries = container.walk(
        &start,
        &WalkOptions {
            max_depth: depth,
            all,
        },
    )?;

    if cli.json {
        return print_json(&serde_json::json!({
            "group_id": resolved.id,
            "kind": resolved.kind,
            "uuid": resolved.uuid,
            "root": container.root(),
            "path": path,
            "entries": entries,
        }));
    }

    let names: Vec<String> = entries
        .iter()
        .map(crate::source::sim::Entry::name)
        .collect();
    let nodes: Vec<Node<'_>> = entries
        .iter()
        .zip(&names)
        .map(|(entry, name)| Node {
            depth: entry.depth,
            name,
            detail: match entry.kind {
                // A directory's own size is an allocation detail, not information.
                EntryKind::Dir => String::new(),
                EntryKind::Unreadable => entry.error.clone().unwrap_or_default(),
                _ => ui::human_size(entry.size),
            },
            modified: ui::format_time(entry.modified),
        })
        .collect();

    let label = match path {
        Some(path) => format!("{} ▸ {}", resolved.id, path.display()),
        None => resolved.id.clone(),
    };

    anstream::print!("{}", ui::tree::render(&label, &nodes));

    if entries.is_empty() {
        anstream::println!("(empty)");
    }
    Ok(())
}

/// Writes a value to stdout as pretty-printed JSON.
fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    anstream::println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
