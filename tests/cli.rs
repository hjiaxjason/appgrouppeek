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
fn errors_carry_no_ansi_escapes_when_stderr_is_not_a_terminal() {
    // assert_cmd captures stderr through a pipe, so anstream must strip styling
    // even without --no-color. Colour leaking into redirected output is a bug.
    let output = agpeek()
        .args(["groups", "com.example.app", "--device", "no-such-simulator"])
        .output()
        .expect("runs");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains('\u{1b}'),
        "stderr contained ANSI escapes: {stderr:?}"
    );
}

#[test]
fn help_lists_the_ls_subcommand() {
    agpeek()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("ls"));
}

#[test]
fn ls_requires_a_group_id() {
    agpeek()
        .arg("ls")
        .assert()
        .failure()
        .code(2)
        .stderr(contains("<GROUP_ID>"));
}

#[test]
fn ls_rejects_a_path_that_escapes_the_container() {
    // Resolution happens before the walk, so this fails on the path argument
    // rather than needing a container that actually exists.
    agpeek()
        .args(["ls", "group.does.not.exist", "../../etc", "--no-color"])
        .assert()
        .failure()
        .code(1)
        .stderr(contains("error:"));
}

#[test]
fn help_lists_the_decoding_subcommands() {
    agpeek()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("cat"))
        .stdout(contains("defaults"));
}

#[test]
fn cat_requires_a_group_id_and_a_path() {
    agpeek()
        .args(["cat", "group.example"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("<PATH>"));
}

#[test]
fn defaults_requires_a_group_id() {
    agpeek()
        .arg("defaults")
        .assert()
        .failure()
        .code(2)
        .stderr(contains("<GROUP_ID>"));
}

#[test]
fn cat_rejects_a_path_that_escapes_the_container() {
    agpeek()
        .args([
            "cat",
            "group.does.not.exist",
            "../../etc/passwd",
            "--no-color",
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(contains("error:"));
}

#[test]
fn help_lists_the_snapshot_subcommands() {
    agpeek()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("snapshot"))
        .stdout(contains("diff"));
}

#[test]
fn diff_requires_two_snapshots() {
    agpeek()
        .args(["diff", "only-one.json"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("<AFTER>"));
}

/// `diff` reads snapshot files, so the whole comparison is exercisable end to
/// end without a simulator — unlike `ls` and `cat`, which reach their container
/// only through discovery.
#[test]
fn diff_reports_a_changed_key_between_two_snapshot_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let before = dir.path().join("before.json");
    let after = dir.path().join("after.json");

    std::fs::write(&before, snapshot_json("t0", 3)).expect("write");
    std::fs::write(&after, snapshot_json("t1", 4)).expect("write");

    agpeek()
        .args([
            "diff",
            before.to_str().expect("utf8"),
            after.to_str().expect("utf8"),
            "--no-color",
        ])
        .assert()
        .success()
        .stdout(contains("~ usageCount: 3 → 4"));
}

#[test]
fn diff_refuses_snapshots_of_different_groups() {
    let dir = tempfile::tempdir().expect("temp dir");
    let before = dir.path().join("before.json");
    let after = dir.path().join("after.json");

    std::fs::write(&before, snapshot_json("t0", 3)).expect("write");
    std::fs::write(
        &after,
        snapshot_json("t1", 4).replace("group.example", "group.other"),
    )
    .expect("write");

    agpeek()
        .args([
            "diff",
            before.to_str().expect("utf8"),
            after.to_str().expect("utf8"),
            "--no-color",
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(contains("different groups"));
}

/// A minimal snapshot holding one plist with one key.
fn snapshot_json(taken_at: &str, usage_count: u32) -> String {
    format!(
        r#"{{
  "version": 1,
  "group_id": "group.example",
  "container_uuid": "UUID",
  "device_udid": "DEVICE",
  "taken_at": "{taken_at}",
  "files": [
    {{
      "path": "Library/Preferences/group.example.plist",
      "size": 100,
      "sha256": "{usage_count:064}",
      "content": {{ "usageCount": {usage_count} }}
    }}
  ]
}}"#
    )
}

#[test]
fn version_is_reported() {
    agpeek()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}
