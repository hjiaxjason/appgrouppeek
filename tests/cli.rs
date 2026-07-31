//! Integration tests that drive the built binary.
//!
//! These cover argument handling and error surfacing only — anything that needs a
//! real simulator is verified by hand against the acceptance criteria in `PRD.md`,
//! since CI cannot boot one.

// `clippy.toml` exempts `#[test]` functions from the no-panic rule, but not the
// helpers beside them. Failing to build the binary should abort the suite.
#![allow(clippy::expect_used)]

use assert_cmd::Command;
use predicates::str::contains;

/// Builds a `Command` for the binary under test.
fn agpeek() -> Command {
    Command::cargo_bin("agpeek").expect("binary builds")
}

#[test]
fn help_lists_both_subcommands() {
    agpeek()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("devices"))
        .stdout(contains("groups"));
}

#[test]
fn global_flags_are_accepted_after_the_subcommand() {
    // `--json` is declared global, so this must parse even though it trails the
    // subcommand and its positional argument.
    agpeek()
        .args(["groups", "com.example.app", "--json", "--no-color"])
        .assert()
        .code(predicates::ord::ne(2));
}

#[test]
fn groups_requires_a_bundle_id() {
    agpeek()
        .arg("groups")
        .assert()
        .failure()
        .code(2)
        .stderr(contains("<BUNDLE_ID>"));
}

#[test]
fn unknown_subcommand_fails_cleanly() {
    agpeek()
        .arg("nope")
        .assert()
        .failure()
        .code(2)
        .stderr(contains("unrecognized subcommand"));
}

#[test]
fn unknown_device_is_reported_with_the_error_prefix() {
    agpeek()
        .args([
            "groups",
            "com.example.app",
            "--device",
            "no-such-simulator",
            "--no-color",
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(contains("error:"))
        .stderr(contains("no simulator matches `no-such-simulator`"));
}

#[test]
fn version_is_reported() {
    agpeek()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}
