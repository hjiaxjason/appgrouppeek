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

use anyhow::{Context, Result, bail};
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            bail!("`{}` failed with {}", rendered(), output.status);
        }
        bail!("`{}` failed: {detail}", rendered());
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
        let devices = parse_devices(DEVICE_LIST).expect("fixture parses");
        assert_eq!(devices.len(), 2, "the unavailable device is filtered out");
        assert_eq!(devices[0].name, "iPhone 17");
        assert!(devices[0].is_booted());
        assert_eq!(devices[0].runtime, "iOS 26.5");
    }

    #[test]
    fn parse_devices_rejects_malformed_json() {
        assert!(parse_devices("{ not json").is_err());
    }
}
