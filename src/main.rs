//! `agpeek` — inspect iOS App Group containers.
//!
//! See `PRD.md` for the product shape. `main` does routing only; discovery lives in
//! [`discover`] and all rendering in [`ui`].

mod cli;
mod decode;
mod diff;
mod discover;
mod snapshot;
mod source;
mod ui;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::source::sim::{Container, EntryKind, WalkOptions};
use crate::ui::Column;
use crate::ui::tree::Node;

/// Bytes of a hexdump shown when falling back from a failed decode.
const DEFAULT_HEX_LIMIT: usize = 2048;

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
        Command::Cat {
            group_id,
            path,
            raw,
            limit,
        } => cat(cli, group_id, path, *raw, *limit),
        Command::Defaults { group_id, raw } => defaults(cli, group_id, *raw),
        Command::Snapshot { group_id, output } => snapshot(cli, group_id, output.as_deref()),
        Command::Diff { before, after } => diff(cli, before, after),
    }
}

/// Records the current contents of a container.
fn snapshot(cli: &Cli, group_id: &str, output: Option<&Path>) -> Result<()> {
    let device = discover::select_device(discover::devices()?, cli.device.as_deref())?;
    let resolved = discover::resolve_container(&device, group_id)?;
    let container = Container::new(resolved.path.clone());

    let snapshot = snapshot::Snapshot::capture(&container, &resolved, &device)?;
    let json = serde_json::to_string_pretty(&snapshot)?;

    match output {
        Some(path) => {
            std::fs::write(path, format!("{json}\n"))
                .with_context(|| format!("could not write `{}`", path.display()))?;
            // To stderr so `-o` output stays scriptable when stdout is piped.
            anstream::eprintln!(
                "wrote {} ({} files) to {}",
                resolved.id,
                snapshot.files.len(),
                path.display()
            );
        }
        None => anstream::println!("{json}"),
    }
    Ok(())
}

/// Compares two snapshots.
fn diff(cli: &Cli, before: &Path, after: &Path) -> Result<()> {
    let before = snapshot::Snapshot::load(before)?;
    let after = snapshot::Snapshot::load(after)?;
    let changes = diff::compare(&before, &after)?;

    if cli.json {
        return print_json(&changes);
    }

    anstream::print!("{}", ui::diff::render(&changes));
    Ok(())
}

/// Shows a file from a container, decoded where possible.
fn cat(cli: &Cli, group_id: &str, path: &Path, raw: bool, limit: usize) -> Result<()> {
    let (container, resolved) = open_container(cli, group_id)?;
    let file = container.resolve(Some(path))?;
    let bytes = container.read(&file)?;
    show(
        cli,
        &bytes,
        raw,
        limit,
        &format!("{} ▸ {}", resolved.id, path.display()),
    )
}

/// Shows the shared `UserDefaults` suite for a group.
fn defaults(cli: &Cli, group_id: &str, raw: bool) -> Result<()> {
    let (container, resolved) = open_container(cli, group_id)?;

    // The suite is always stored under the group's own identifier, which is not
    // necessarily what the user typed — they may have passed a container UUID.
    let relative = PathBuf::from("Library/Preferences").join(format!("{}.plist", resolved.id));
    let file = container.resolve(Some(&relative))?;

    if !file.exists() {
        bail!(
            "`{}` has no shared UserDefaults yet\n\nexpected {}\nnothing has written to the suite on this device",
            resolved.id,
            relative.display()
        );
    }

    let bytes = container.read(&file)?;
    show(cli, &bytes, raw, 0, &resolved.id)
}

/// Resolves a group identifier to a container ready to read.
fn open_container(cli: &Cli, group_id: &str) -> Result<(Container, discover::Container)> {
    let device = discover::select_device(discover::devices()?, cli.device.as_deref())?;
    let resolved = discover::resolve_container(&device, group_id)?;
    Ok((Container::new(resolved.path.clone()), resolved))
}

/// Renders file bytes, decoded unless `raw` was asked for.
fn show(cli: &Cli, bytes: &[u8], raw: bool, limit: usize, label: &str) -> Result<()> {
    if raw {
        return show_raw(cli, bytes, limit, label);
    }

    let decoded = decode::decode(bytes);

    if cli.json {
        let body = match &decoded.body {
            decode::Body::Value(value) => serde_json::to_value(value)?,
            decode::Body::Text(text) => serde_json::Value::String(text.clone()),
            decode::Body::Opaque => serde_json::Value::Null,
        };
        return print_json(&serde_json::json!({
            "path": label,
            "format": decoded.format,
            "bytes": bytes.len(),
            "note": decoded.note,
            "value": body,
        }));
    }

    match &decoded.body {
        decode::Body::Value(value) => anstream::print!("{}", ui::value::render(value)),
        decode::Body::Text(text) => anstream::print!("{text}"),
        decode::Body::Opaque => {
            // Nothing decodable, so say what it is and fall back to the bytes.
            match &decoded.note {
                Some(note) => anstream::println!("{} — {note}", decoded.format),
                None => anstream::println!("{}, {} bytes", decoded.format, bytes.len()),
            }
            if !bytes.is_empty() {
                anstream::print!("{}", ui::value::hexdump(bytes, DEFAULT_HEX_LIMIT));
            }
        }
    }
    Ok(())
}

/// Renders bytes without decoding them.
///
/// Text passes through verbatim so `--raw` on a config file stays readable;
/// anything else is hexdumped rather than spraying control bytes at the terminal.
fn show_raw(cli: &Cli, bytes: &[u8], limit: usize, label: &str) -> Result<()> {
    if cli.json {
        return print_json(&serde_json::json!({
            "path": label,
            "bytes": bytes.len(),
            "base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
        }));
    }

    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => anstream::print!("{text}"),
        _ => anstream::print!("{}", ui::value::hexdump(bytes, limit)),
    }
    Ok(())
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
