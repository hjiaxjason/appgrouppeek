//! Comparing two snapshots.
//!
//! File-level comparison is not enough for the case this tool exists to serve.
//! Natively's container holds a single preferences plist, so a file-level diff
//! would report "1 file modified" every time and answer nothing. When both sides
//! of a modified file were decoded as plists, the comparison descends into their
//! keys; it falls back to file-level reporting only when it cannot.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::snapshot::{FileEntry, Snapshot};

/// What happened to a file or key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    /// Present only in the later snapshot.
    Added,
    /// Present only in the earlier snapshot.
    Removed,
    /// Present in both, with different contents.
    Modified,
}

/// A key whose value differs between two snapshots.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyChange {
    /// Dotted path to the key, e.g. `keyboard_diagnostics.net_apple`.
    pub key: String,
    /// What happened to it.
    pub change: Change,
    /// Value in the earlier snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    /// Value in the later snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}

/// A file that differs between two snapshots.
#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    /// Path relative to the container root.
    pub path: String,
    /// What happened to it.
    pub change: Change,
    /// Size in the earlier snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_before: Option<u64>,
    /// Size in the later snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_after: Option<u64>,
    /// Key-level changes, when both sides decoded as plists.
    ///
    /// `None` means the comparison could not descend — the file is not a plist,
    /// or was too large to inline — and only the file-level fact is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<KeyChange>>,
}

/// Everything that changed between two snapshots.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeSet {
    /// Group the snapshots describe.
    pub group_id: String,
    /// When the earlier snapshot was taken.
    pub before: String,
    /// When the later snapshot was taken.
    pub after: String,
    /// Files that changed, sorted by path.
    pub files: Vec<FileChange>,
}

impl ChangeSet {
    /// Whether anything changed at all.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Compares two snapshots.
///
/// # Errors
///
/// Refuses snapshots of different containers. Diffing unrelated groups would
/// report every file as both added and removed, which is never what was meant.
pub fn compare(before: &Snapshot, after: &Snapshot) -> Result<ChangeSet> {
    if before.group_id != after.group_id {
        bail!(
            "snapshots describe different groups: `{}` and `{}`",
            before.group_id,
            after.group_id
        );
    }

    let paths: BTreeSet<&str> = before
        .files
        .iter()
        .chain(&after.files)
        .map(|file| file.path.as_str())
        .collect();

    let mut files = Vec::new();
    for path in paths {
        match (before.file(path), after.file(path)) {
            (None, Some(entry)) => files.push(FileChange {
                path: path.to_string(),
                change: Change::Added,
                size_before: None,
                size_after: Some(entry.size),
                keys: None,
            }),
            (Some(entry), None) => files.push(FileChange {
                path: path.to_string(),
                change: Change::Removed,
                size_before: Some(entry.size),
                size_after: None,
                keys: None,
            }),
            (Some(old), Some(new)) => {
                if let Some(change) = compare_file(path, old, new) {
                    files.push(change);
                }
            }
            (None, None) => {}
        }
    }

    Ok(ChangeSet {
        group_id: before.group_id.clone(),
        before: before.taken_at.clone(),
        after: after.taken_at.clone(),
        files,
    })
}

/// Compares one file present in both snapshots, returning `None` if identical.
fn compare_file(path: &str, before: &FileEntry, after: &FileEntry) -> Option<FileChange> {
    if before.sha256 == after.sha256 {
        return None;
    }

    // Descend only when both sides were decoded; a hash-only side means the
    // file-level fact is all that is actually known.
    let keys = match (&before.content, &after.content) {
        (Some(old), Some(new)) => {
            let mut changes = Vec::new();
            compare_values(String::new(), old, new, &mut changes);
            Some(changes)
        }
        _ => None,
    };

    Some(FileChange {
        path: path.to_string(),
        change: Change::Modified,
        size_before: Some(before.size),
        size_after: Some(after.size),
        keys,
    })
}

/// Walks two decoded values in step, recording differences by key path.
fn compare_values(prefix: String, before: &Value, after: &Value, out: &mut Vec<KeyChange>) {
    match (before, after) {
        (Value::Object(old), Value::Object(new)) => {
            let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
            for key in keys {
                let path = join(&prefix, key);
                match (old.get(key), new.get(key)) {
                    (Some(old), Some(new)) => compare_values(path, old, new, out),
                    (None, Some(new)) => out.push(added(path, new)),
                    (Some(old), None) => out.push(removed(path, old)),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(old), Value::Array(new)) => {
            for index in 0..old.len().max(new.len()) {
                let path = format!("{prefix}[{index}]");
                match (old.get(index), new.get(index)) {
                    (Some(old), Some(new)) => compare_values(path, old, new, out),
                    (None, Some(new)) => out.push(added(path, new)),
                    (Some(old), None) => out.push(removed(path, old)),
                    (None, None) => {}
                }
            }
        }
        _ if before != after => out.push(KeyChange {
            key: prefix,
            change: Change::Modified,
            before: Some(before.clone()),
            after: Some(after.clone()),
        }),
        _ => {}
    }
}

/// Joins a key onto a dotted path.
fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Builds an "added" key change.
fn added(key: String, value: &Value) -> KeyChange {
    KeyChange {
        key,
        change: Change::Added,
        before: None,
        after: Some(value.clone()),
    }
}

/// Builds a "removed" key change.
fn removed(key: String, value: &Value) -> KeyChange {
    KeyChange {
        key,
        change: Change::Removed,
        before: Some(value.clone()),
        after: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::FORMAT_VERSION;
    use serde_json::json;

    fn snapshot(group: &str, taken_at: &str, files: Vec<FileEntry>) -> Snapshot {
        Snapshot {
            version: FORMAT_VERSION,
            group_id: group.into(),
            container_uuid: "UUID".into(),
            device_udid: "DEVICE".into(),
            taken_at: taken_at.into(),
            files,
        }
    }

    fn plist_file(path: &str, sha: &str, content: Value) -> FileEntry {
        FileEntry {
            path: path.into(),
            size: 100,
            modified: None,
            sha256: sha.into(),
            content: Some(content),
        }
    }

    fn opaque_file(path: &str, sha: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            size: 100,
            modified: None,
            sha256: sha.into(),
            content: None,
        }
    }

    /// The acceptance case: one plist, one key, one changed number.
    #[test]
    fn compare_reports_the_changed_key_not_just_the_file() {
        let path = "Library/Preferences/group.app.natively.shared.plist";
        let before = snapshot(
            "group.app.natively.shared",
            "t0",
            vec![plist_file(path, "aaa", json!({ "usageCount": 3 }))],
        );
        let after = snapshot(
            "group.app.natively.shared",
            "t1",
            vec![plist_file(path, "bbb", json!({ "usageCount": 4 }))],
        );

        let changes = compare(&before, &after).expect("compares");
        assert_eq!(changes.files.len(), 1);

        let keys = changes.files[0].keys.as_ref().expect("descended into keys");
        assert_eq!(
            keys,
            &vec![KeyChange {
                key: "usageCount".into(),
                change: Change::Modified,
                before: Some(json!(3)),
                after: Some(json!(4)),
            }]
        );
    }

    #[test]
    fn compare_reports_changes_inside_nested_dictionaries() {
        let path = "a.plist";
        let before = snapshot(
            "g",
            "t0",
            vec![plist_file(
                path,
                "aaa",
                json!({ "keyboard_diagnostics": { "net_apple": "ok", "net_dns": "ok" } }),
            )],
        );
        let after = snapshot(
            "g",
            "t1",
            vec![plist_file(
                path,
                "bbb",
                json!({ "keyboard_diagnostics": { "net_apple": "slow", "net_dns": "ok" } }),
            )],
        );

        let changes = compare(&before, &after).expect("compares");
        let keys = changes.files[0].keys.as_ref().expect("keys");

        assert_eq!(keys.len(), 1, "only the key that changed is reported");
        assert_eq!(keys[0].key, "keyboard_diagnostics.net_apple");
        assert_eq!(keys[0].after, Some(json!("slow")));
    }

    #[test]
    fn compare_reports_added_and_removed_keys() {
        let before = snapshot(
            "g",
            "t0",
            vec![plist_file(
                "a.plist",
                "aaa",
                json!({ "gone": 1, "kept": 2 }),
            )],
        );
        let after = snapshot(
            "g",
            "t1",
            vec![plist_file(
                "a.plist",
                "bbb",
                json!({ "kept": 2, "fresh": 3 }),
            )],
        );

        let changes = compare(&before, &after).expect("compares");
        let keys = changes.files[0].keys.as_ref().expect("keys");

        let by_key: Vec<(&str, Change)> = keys
            .iter()
            .map(|change| (change.key.as_str(), change.change))
            .collect();
        assert_eq!(
            by_key,
            vec![("fresh", Change::Added), ("gone", Change::Removed)]
        );
    }

    #[test]
    fn compare_descends_into_arrays_by_index() {
        let before = snapshot(
            "g",
            "t0",
            vec![plist_file("a.plist", "aaa", json!({ "items": ["a", "b"] }))],
        );
        let after = snapshot(
            "g",
            "t1",
            vec![plist_file(
                "a.plist",
                "bbb",
                json!({ "items": ["a", "c", "d"] }),
            )],
        );

        let changes = compare(&before, &after).expect("compares");
        let keys = changes.files[0].keys.as_ref().expect("keys");

        assert_eq!(keys[0].key, "items[1]");
        assert_eq!(keys[0].change, Change::Modified);
        assert_eq!(keys[1].key, "items[2]");
        assert_eq!(keys[1].change, Change::Added);
    }

    #[test]
    fn compare_falls_back_to_file_level_without_decoded_content() {
        let before = snapshot("g", "t0", vec![opaque_file("blob.bin", "aaa")]);
        let after = snapshot("g", "t1", vec![opaque_file("blob.bin", "bbb")]);

        let changes = compare(&before, &after).expect("compares");
        assert_eq!(changes.files[0].change, Change::Modified);
        assert!(
            changes.files[0].keys.is_none(),
            "no key detail is claimed for content that was never decoded"
        );
    }

    #[test]
    fn compare_reports_added_and_removed_files() {
        let before = snapshot("g", "t0", vec![opaque_file("gone.txt", "aaa")]);
        let after = snapshot("g", "t1", vec![opaque_file("fresh.txt", "bbb")]);

        let changes = compare(&before, &after).expect("compares");
        let summary: Vec<(&str, Change)> = changes
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.change))
            .collect();
        assert_eq!(
            summary,
            vec![("fresh.txt", Change::Added), ("gone.txt", Change::Removed)]
        );
    }

    #[test]
    fn compare_of_identical_snapshots_is_empty() {
        let files = vec![plist_file("a.plist", "aaa", json!({ "k": 1 }))];
        let before = snapshot("g", "t0", files.clone());
        let after = snapshot("g", "t1", files);

        assert!(compare(&before, &after).expect("compares").is_empty());
    }

    #[test]
    fn compare_refuses_snapshots_of_different_groups() {
        let before = snapshot("group.one", "t0", vec![]);
        let after = snapshot("group.two", "t1", vec![]);

        let err = compare(&before, &after).unwrap_err().to_string();
        assert!(err.contains("different groups"), "got: {err}");
    }

    #[test]
    fn compare_notices_a_hash_change_with_identical_decoded_content() {
        // Same keys, different bytes — trailing padding, say. The file is
        // reported as modified with an empty key list rather than dropped.
        let before = snapshot("g", "t0", vec![plist_file("a.plist", "aaa", json!({}))]);
        let after = snapshot("g", "t1", vec![plist_file("a.plist", "bbb", json!({}))]);

        let changes = compare(&before, &after).expect("compares");
        assert_eq!(changes.files.len(), 1);
        assert_eq!(changes.files[0].keys.as_deref(), Some(&[][..]));
    }
}
