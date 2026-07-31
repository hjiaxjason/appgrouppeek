//! Locating simulators and their App Group containers.
//!
//! This is deliberately the only module that knows simulators exist. It shells out
//! to `xcrun simctl` and hands back plain paths; everything above it operates on a
//! directory tree and can be tested without a simulator booted.
//!
//! Each subprocess call is paired with a pure parsing function so the parsing can
//! be unit-tested against captured fixture output.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// A simulator that can be inspected.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    /// Stable unique identifier, e.g. `B943F0CB-ED32-487F-9D2D-F1977C064AE7`.
    pub udid: String,
    /// Display name, e.g. `iPhone 17`.
    pub name: String,
    /// Simulator lifecycle state, e.g. `Booted` or `Shutdown`.
    pub state: String,
    /// Human-readable runtime, e.g. `iOS 26.5`.
    pub runtime: String,
    /// Root of the device's data volume, the parent of `Containers/`.
    pub data_path: PathBuf,
}

impl Device {
    /// Whether the simulator is currently running.
    pub fn is_booted(&self) -> bool {
        self.state.eq_ignore_ascii_case("Booted")
    }
}

/// An App Group container declared by an app.
#[derive(Debug, Clone, Serialize)]
pub struct AppGroup {
    /// The group identifier as declared in the app's entitlements.
    ///
    /// Note this does **not** reliably begin with `group.` — `systemgroup.…` and
    /// bare identifiers such as `com.apple.CoreODI` both occur in practice.
    pub id: String,
    /// Absolute path to the container on the host filesystem.
    pub path: PathBuf,
}

/// Which of the two shared-container roots a container lives under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerKind {
    /// `Containers/Shared/AppGroup` — declared by an app's entitlements.
    App,
    /// `Containers/Shared/SystemGroup` — created by system daemons.
    System,
}

impl ContainerKind {
    /// Directory name under `Containers/Shared`.
    fn dir_name(self) -> &'static str {
        match self {
            Self::App => "AppGroup",
            Self::System => "SystemGroup",
        }
    }
}

impl std::fmt::Display for ContainerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::App => "app",
            Self::System => "system",
        })
    }
}

/// A shared container found on disk.
#[derive(Debug, Clone, Serialize)]
pub struct Container {
    /// Group identifier read from the container's metadata.
    pub id: String,
    /// Which root the container lives under.
    pub kind: ContainerKind,
    /// The container directory's own UUID, which is unique where `id` is not.
    pub uuid: String,
    /// Absolute path to the container.
    pub path: PathBuf,
}

/// Filename of the per-container metadata plist written by the container manager.
const METADATA_PLIST: &str = ".com.apple.mobile_container_manager.metadata.plist";

/// Key inside that plist holding the group identifier.
const METADATA_ID_KEY: &str = "MCMMetadataIdentifier";

/// Reads the group identifier out of a container's metadata plist.
///
/// Returns `None` rather than an error for anything unreadable or unexpected: the
/// scan walks every container on the device, and one malformed neighbour must not
/// prevent finding the container actually being looked for.
fn read_container_id(container: &std::path::Path) -> Option<String> {
    let value = plist::Value::from_file(container.join(METADATA_PLIST)).ok()?;
    Some(
        value
            .as_dictionary()?
            .get(METADATA_ID_KEY)?
            .as_string()?
            .to_string(),
    )
}

/// Lists every shared container on a device, across both roots.
///
/// Ordering is by kind then UUID so output and error messages are deterministic.
pub fn containers(device: &Device) -> Result<Vec<Container>> {
    let mut found = Vec::new();

    for kind in [ContainerKind::App, ContainerKind::System] {
        let root = device
            .data_path
            .join("Containers/Shared")
            .join(kind.dir_name());
        let Ok(entries) = std::fs::read_dir(&root) else {
            // A device that has never booted has no container roots at all.
            continue;
        };

        let mut in_root: Vec<Container> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let path = entry.path();
                Some(Container {
                    id: read_container_id(&path)?,
                    kind,
                    uuid: path.file_name()?.to_string_lossy().into_owned(),
                    path,
                })
            })
            .collect();

        in_root.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        found.append(&mut in_root);
    }

    Ok(found)
}

/// Resolves a group identifier to a single container.
///
/// Accepts either a group identifier or a container UUID. Group identifiers are
/// **not** unique — `systemgroup.com.apple.accessorysetupkit` exists under both
/// roots on a stock simulator, with different contents — so an ambiguous match is
/// an error listing the candidates, and the UUID is the way to disambiguate.
pub fn resolve_container(device: &Device, query: &str) -> Result<Container> {
    let available = containers(device)?;

    if let Some(container) = available
        .iter()
        .find(|container| container.uuid.eq_ignore_ascii_case(query))
    {
        return Ok(container.clone());
    }

    let matches: Vec<&Container> = available
        .iter()
        .filter(|container| container.id == query)
        .collect();

    match matches.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(anyhow!(
            "no container for `{query}` on {}\n\nrun `agpeek groups <bundle-id>` to see what an app declares",
            device.name
        )),
        several => Err(anyhow!(
            "`{query}` matches {} containers — pass the UUID instead:\n{}",
            several.len(),
            several
                .iter()
                .map(|container| format!("  {} ({})", container.uuid, container.kind))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// Runs `xcrun simctl` with the given arguments and returns its stdout.
///
/// # Errors
///
/// Fails if the process cannot be spawned, or if it exits non-zero — in which case
/// the error carries simctl's own stderr, which is usually the actionable part.
fn simctl(args: &[&str]) -> Result<String> {
    let rendered = || format!("xcrun simctl {}", args.join(" "));

    let output = Command::new("xcrun")
        .arg("simctl")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{}`", rendered()))?;

    if !output.status.success() {
        let detail = condense(&String::from_utf8_lossy(&output.stderr));
        if detail.is_empty() {
            bail!("`{}` failed with {}", rendered(), output.status);
        }
        bail!("`{}` failed: {detail}", rendered());
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Flattens multi-line tool output into a single line.
///
/// simctl reports failures across three lines, repeating the underlying strerror
/// text. Collapsing it keeps the cause chain readable, since the actionable part
/// is already in the context message wrapping it.
fn condense(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Shape of `xcrun simctl list devices -j`, keyed by runtime identifier.
#[derive(Debug, Deserialize)]
struct DeviceList {
    devices: BTreeMap<String, Vec<RawDevice>>,
}

/// A single entry in simctl's device list.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDevice {
    udid: String,
    name: String,
    state: String,
    is_available: bool,
    /// Absent for runtimes that are not installed.
    data_path: Option<PathBuf>,
}

/// Turns a runtime identifier into something readable.
///
/// `com.apple.CoreSimulator.SimRuntime.iOS-26-5` becomes `iOS 26.5`. Unrecognised
/// identifiers are passed through unchanged rather than mangled.
fn friendly_runtime(identifier: &str) -> String {
    let Some(tail) = identifier.strip_prefix("com.apple.CoreSimulator.SimRuntime.") else {
        return identifier.to_string();
    };
    match tail.split_once('-') {
        Some((platform, version)) => format!("{platform} {}", version.replace('-', ".")),
        None => tail.to_string(),
    }
}

/// Parses the output of `xcrun simctl list devices -j`.
///
/// Devices whose runtime is unavailable are skipped, as are entries with no data
/// path — neither can be inspected.
pub fn parse_devices(json: &str) -> Result<Vec<Device>> {
    let list: DeviceList =
        serde_json::from_str(json).context("could not parse the device list from simctl")?;

    let mut devices: Vec<Device> = list
        .devices
        .into_iter()
        .flat_map(|(runtime, raws)| {
            let runtime = friendly_runtime(&runtime);
            raws.into_iter().filter_map(move |raw| {
                if !raw.is_available {
                    return None;
                }
                Some(Device {
                    udid: raw.udid,
                    name: raw.name,
                    state: raw.state,
                    runtime: runtime.clone(),
                    data_path: raw.data_path?,
                })
            })
        })
        .collect();

    // Booted devices first, then by name, so the useful ones are at the top.
    devices.sort_by(|a, b| {
        b.is_booted()
            .cmp(&a.is_booted())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.udid.cmp(&b.udid))
    });

    Ok(devices)
}

/// Lists every inspectable simulator on this host.
pub fn devices() -> Result<Vec<Device>> {
    parse_devices(&simctl(&["list", "devices", "-j"])?)
}

/// Picks the device to operate on.
///
/// With no request, the single booted device is used. Ambiguity is always an error
/// rather than an arbitrary choice, because picking the wrong simulator produces a
/// confusingly empty container rather than an obvious failure.
pub fn select_device(available: Vec<Device>, requested: Option<&str>) -> Result<Device> {
    match requested {
        Some(query) => select_by_query(available, query),
        None => select_booted(available),
    }
}

/// Resolves an explicit `--device` request against UDID first, then name.
fn select_by_query(available: Vec<Device>, query: &str) -> Result<Device> {
    if let Some(device) = available
        .iter()
        .find(|device| device.udid.eq_ignore_ascii_case(query))
    {
        return Ok(device.clone());
    }

    let mut by_name: Vec<Device> = available
        .iter()
        .filter(|device| device.name.eq_ignore_ascii_case(query))
        .cloned()
        .collect();

    match by_name.len() {
        1 => Ok(by_name.remove(0)),
        0 => Err(anyhow!(
            "no simulator matches `{query}`\n\navailable:\n{}",
            bullet_list(&available)
        )),
        _ => Err(anyhow!(
            "`{query}` matches {} simulators — pass a UDID instead:\n{}",
            by_name.len(),
            bullet_list(&by_name)
        )),
    }
}

/// Resolves the implicit case: exactly one booted simulator.
fn select_booted(available: Vec<Device>) -> Result<Device> {
    let mut booted: Vec<Device> = available
        .iter()
        .filter(|device| device.is_booted())
        .cloned()
        .collect();

    match booted.len() {
        1 => Ok(booted.remove(0)),
        0 => Err(anyhow!(
            "no booted simulator — boot one, or pass --device\n\navailable:\n{}",
            bullet_list(&available)
        )),
        _ => Err(anyhow!(
            "{} simulators are booted — pass --device to choose:\n{}",
            booted.len(),
            bullet_list(&booted)
        )),
    }
}

/// Renders devices as an indented list for use inside error messages.
fn bullet_list(devices: &[Device]) -> String {
    if devices.is_empty() {
        return "  (none)".to_string();
    }
    devices
        .iter()
        .map(|device| format!("  {} ({}, {})", device.name, device.runtime, device.udid))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parses the tab-separated output of `simctl get_app_container … groups`.
///
/// # Layout
///
/// One line per group, `identifier<TAB>absolute-path`. Paths may contain spaces,
/// so the split is on the first tab only.
///
/// # Edge cases
///
/// Blank lines are skipped. A line with no tab is malformed and is an error rather
/// than silently dropped, since silently losing a group would be worse than failing.
pub fn parse_groups(stdout: &str) -> Result<Vec<AppGroup>> {
    stdout
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, path) = line
                .split_once('\t')
                .ok_or_else(|| anyhow!("unexpected line from simctl: {line:?}"))?;
            Ok(AppGroup {
                id: id.trim().to_string(),
                path: PathBuf::from(path.trim()),
            })
        })
        .collect()
}

/// Lists the App Groups declared by an installed app.
///
/// # Errors
///
/// Distinguishes the two failure modes that look identical in simctl's own output:
/// the app not being installed, and the app declaring no groups at all. Both are
/// errors here — an empty success would leave the user unsure which happened.
pub fn app_groups(device: &Device, bundle_id: &str) -> Result<Vec<AppGroup>> {
    let stdout =
        simctl(&["get_app_container", &device.udid, bundle_id, "groups"]).with_context(|| {
            format!(
                "could not read App Groups for `{bundle_id}` on {} ({}) — is the app installed?",
                device.name, device.udid
            )
        })?;

    let groups = parse_groups(&stdout)?;
    if groups.is_empty() {
        bail!(
            "`{bundle_id}` is installed on {} but declares no App Groups",
            device.name
        );
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from real `simctl list devices -j` output on this host.
    const DEVICE_LIST: &str = r#"{
      "devices": {
        "com.apple.CoreSimulator.SimRuntime.iOS-26-5": [
          {
            "udid": "B943F0CB-ED32-487F-9D2D-F1977C064AE7",
            "name": "iPhone 17",
            "state": "Booted",
            "isAvailable": true,
            "dataPath": "/tmp/devices/booted/data"
          },
          {
            "udid": "11C81E92-46FB-436D-AEC4-357C075E6DCB",
            "name": "iPhone 17 Pro",
            "state": "Shutdown",
            "isAvailable": true,
            "dataPath": "/tmp/devices/shutdown/data"
          },
          {
            "udid": "00000000-0000-0000-0000-000000000000",
            "name": "Unavailable Phone",
            "state": "Shutdown",
            "isAvailable": false,
            "dataPath": "/tmp/devices/unavailable/data"
          }
        ]
      }
    }"#;

    fn devices() -> Vec<Device> {
        parse_devices(DEVICE_LIST).expect("fixture parses")
    }

    #[test]
    fn friendly_runtime_formats_ios_versions() {
        assert_eq!(
            friendly_runtime("com.apple.CoreSimulator.SimRuntime.iOS-26-5"),
            "iOS 26.5"
        );
        assert_eq!(
            friendly_runtime("com.apple.CoreSimulator.SimRuntime.watchOS-11-0"),
            "watchOS 11.0"
        );
    }

    #[test]
    fn friendly_runtime_passes_through_unknown_identifiers() {
        assert_eq!(friendly_runtime("something-else"), "something-else");
    }

    #[test]
    fn parse_devices_skips_unavailable_and_sorts_booted_first() {
        let devices = devices();
        assert_eq!(devices.len(), 2, "the unavailable device is filtered out");
        assert_eq!(devices[0].name, "iPhone 17");
        assert!(devices[0].is_booted());
        assert_eq!(devices[0].runtime, "iOS 26.5");
    }

    #[test]
    fn parse_devices_rejects_malformed_json() {
        assert!(parse_devices("{ not json").is_err());
    }

    #[test]
    fn select_device_defaults_to_the_only_booted_device() {
        let device = select_device(devices(), None).expect("one booted device");
        assert_eq!(device.name, "iPhone 17");
    }

    #[test]
    fn select_device_errors_when_nothing_is_booted() {
        let shutdown: Vec<Device> = devices()
            .into_iter()
            .map(|mut device| {
                device.state = "Shutdown".into();
                device
            })
            .collect();

        let err = select_device(shutdown, None).unwrap_err().to_string();
        assert!(err.contains("no booted simulator"), "got: {err}");
        assert!(err.contains("iPhone 17 Pro"), "lists candidates: {err}");
    }

    #[test]
    fn select_device_errors_when_several_are_booted() {
        let booted: Vec<Device> = devices()
            .into_iter()
            .map(|mut device| {
                device.state = "Booted".into();
                device
            })
            .collect();

        let err = select_device(booted, None).unwrap_err().to_string();
        assert!(err.contains("2 simulators are booted"), "got: {err}");
    }

    #[test]
    fn select_device_matches_udid_case_insensitively() {
        let device = select_device(devices(), Some("b943f0cb-ed32-487f-9d2d-f1977c064ae7"))
            .expect("matches by udid");
        assert_eq!(device.name, "iPhone 17");
    }

    #[test]
    fn select_device_matches_by_name() {
        let device = select_device(devices(), Some("iPhone 17 Pro")).expect("matches by name");
        assert!(!device.is_booted());
    }

    #[test]
    fn select_device_reports_unknown_names() {
        let err = select_device(devices(), Some("iPad"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no simulator matches `iPad`"), "got: {err}");
    }

    #[test]
    fn select_device_reports_ambiguous_names() {
        let mut duplicated = devices();
        duplicated[1].name = "iPhone 17".into();

        let err = select_device(duplicated, Some("iPhone 17"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("matches 2 simulators"), "got: {err}");
    }

    #[test]
    fn condense_flattens_simctls_multi_line_stderr() {
        let stderr = "An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=2):\n\
                      The operation couldn't be completed. No such file or directory\n\
                      No such file or directory\n";
        let condensed = condense(stderr);
        assert!(!condensed.contains('\n'), "got: {condensed}");
        assert!(condensed.starts_with("An error was encountered"));
    }

    #[test]
    fn condense_of_blank_output_is_empty() {
        assert_eq!(condense("  \n\n "), "");
    }

    #[test]
    fn parse_groups_splits_on_the_first_tab() {
        let groups = parse_groups(
            "group.com.apple.weather\t/tmp/App Group/one\n\
             systemgroup.com.apple.accessorysetupkit\t/tmp/two\n",
        )
        .expect("parses");

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].id, "group.com.apple.weather");
        assert_eq!(groups[0].path, PathBuf::from("/tmp/App Group/one"));
        assert_eq!(groups[1].id, "systemgroup.com.apple.accessorysetupkit");
    }

    #[test]
    fn parse_groups_accepts_identifiers_without_the_group_prefix() {
        let groups = parse_groups("com.apple.CoreODI\t/tmp/odi").expect("parses");
        assert_eq!(groups[0].id, "com.apple.CoreODI");
    }

    #[test]
    fn parse_groups_ignores_blank_lines() {
        let groups = parse_groups("\n\ngroup.a\t/tmp/a\n\n").expect("parses");
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn parse_groups_rejects_lines_without_a_tab() {
        assert!(parse_groups("group.a /tmp/a").is_err());
    }

    #[test]
    fn parse_groups_returns_empty_for_empty_output() {
        assert!(parse_groups("").expect("parses").is_empty());
    }
}
