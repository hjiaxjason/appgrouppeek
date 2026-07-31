//! Capturing the state of a container at a moment in time.
//!
//! A snapshot is a JSON file, sorted by path and stable across runs, so two of
//! them can be compared later without the container still being present.
//!
//! Plist contents are stored inline for small files rather than only a hash. That
//! is what makes a key-level diff possible after the fact — and for the case this
//! tool was built for it is the whole point, because a container holding a single
//! preferences file would otherwise diff as "1 file modified" and nothing more.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decode;
use crate::discover;
use crate::source::sim::{Container, EntryKind, WalkOptions};

/// Version of the snapshot format.
///
/// Present from the first release so the format can change without silently
/// misreading snapshots written by an older build.
pub const FORMAT_VERSION: u32 = 1;

/// Largest plist stored inline, in bytes.
///
/// Above this a snapshot keeps only the hash: a key-level diff of something that
/// big would be unreadable anyway, and snapshots get pasted into issues.
pub const INLINE_LIMIT: u64 = 256 * 1024;

/// A container's contents at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Snapshot format version.
    pub version: u32,
    /// Group identifier of the container.
    pub group_id: String,
    /// UUID of the container directory.
    pub container_uuid: String,
    /// UDID of the device it was taken from.
    pub device_udid: String,
    /// When it was taken, RFC 3339.
    pub taken_at: String,
    /// Every regular file in the container, sorted by path.
    pub files: Vec<FileEntry>,
}

/// One file within a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    /// Path relative to the container root, using forward slashes.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modification time, RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub modified: Option<String>,
    /// Hex-encoded SHA-256 of the file's bytes.
    pub sha256: String,
    /// Decoded plist content, present only for plists within [`INLINE_LIMIT`].
    ///
    /// Stored as already-serialised JSON rather than the decoder's own value type:
    /// this is a projection for comparison, not a claim that the original bytes
    /// could be reconstructed from it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content: Option<serde_json::Value>,
}

impl Snapshot {
    /// Captures the current contents of a container.
    ///
    /// Records regular files only. Directories are implied by the paths of the
    /// files inside them, so an added directory shows up through its contents; an
    /// added *empty* directory does not, which is a deliberate trade for keeping
    /// snapshots to the things that carry data.
    pub fn capture(
        container: &Container,
        resolved: &discover::Container,
        device: &discover::Device,
    ) -> Result<Self> {
        let entries = container.walk(
            container.root(),
            &WalkOptions {
                max_depth: None,
                // Include dotfiles: the container metadata is one, and a change
                // anywhere in the container should be visible.
                all: true,
            },
        )?;

        let mut files = Vec::new();
        for entry in entries {
            if entry.kind != EntryKind::File {
                continue;
            }
            let absolute = container.root().join(&entry.path);
            files.push(FileEntry {
                path: normalise(&entry.path),
                size: entry.size,
                modified: entry.modified.map(format_time),
                sha256: String::new(),
                content: None,
            });
            // Filled in separately so a read failure names the path already known.
            let last = files.len() - 1;
            let (sha256, content) = fingerprint(&absolute, entry.size)
                .with_context(|| format!("could not fingerprint `{}`", entry.path.display()))?;
            if let Some(file) = files.get_mut(last) {
                file.sha256 = sha256;
                file.content = content;
            }
        }

        // Sorted so snapshots are stable across runs and reviewable by eye.
        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self {
            version: FORMAT_VERSION,
            group_id: resolved.id.clone(),
            container_uuid: resolved.uuid.clone(),
            device_udid: device.udid.clone(),
            taken_at: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            files,
        })
    }

    /// Reads a snapshot from disk.
    ///
    /// # Errors
    ///
    /// Fails on unreadable or malformed files, and on snapshots written by a newer
    /// format version than this build understands.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read snapshot `{}`", path.display()))?;
        let snapshot: Self = serde_json::from_str(&text)
            .with_context(|| format!("`{}` is not a valid snapshot", path.display()))?;

        if snapshot.version > FORMAT_VERSION {
            anyhow::bail!(
                "`{}` uses snapshot format {} but this build understands {FORMAT_VERSION}",
                path.display(),
                snapshot.version
            );
        }

        Ok(snapshot)
    }

    /// Looks up a file by its path.
    pub fn file(&self, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|file| file.path == path)
    }
}

/// Hashes a file, decoding it inline when it is a small plist.
///
/// Files within [`INLINE_LIMIT`] are read whole, since the bytes are needed for
/// both the hash and a possible decode. Larger ones are streamed so a snapshot
/// never loads an arbitrarily large file into memory.
fn fingerprint(path: &Path, size: u64) -> Result<(String, Option<serde_json::Value>)> {
    if size > INLINE_LIMIT {
        return Ok((stream_hash(path)?, None));
    }

    let bytes = std::fs::read(path)?;
    let sha256 = hex(&Sha256::digest(&bytes));

    let decoded = decode::decode(&bytes);
    let content = match (decoded.format, &decoded.body) {
        (decode::Format::BinaryPlist | decode::Format::XmlPlist, decode::Body::Value(value)) => {
            Some(serde_json::to_value(value)?)
        }
        _ => None,
    };

    Ok((sha256, content))
}

/// Hashes a file without holding it in memory.
fn stream_hash(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        match buffer.get(..read) {
            Some(chunk) => hasher.update(chunk),
            None => break,
        }
    }

    Ok(hex(&hasher.finalize()))
}

/// Hex-encodes a digest.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Formats a timestamp as RFC 3339.
fn format_time(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Renders a path with forward slashes so snapshots compare across platforms.
fn normalise(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_snapshot_json(files: Vec<FileEntry>) -> String {
        let snapshot = Snapshot {
            version: FORMAT_VERSION,
            group_id: "group.example".into(),
            container_uuid: "UUID".into(),
            device_udid: "DEVICE".into(),
            taken_at: "2026-07-31T12:00:00+00:00".into(),
            files,
        };
        serde_json::to_string_pretty(&snapshot).expect("serialises")
    }

    fn entry(path: &str, sha: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            size: 1,
            modified: None,
            sha256: sha.into(),
            content: None,
        }
    }

    #[test]
    fn hex_encodes_lowercase_fixed_width() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn normalise_uses_forward_slashes() {
        assert_eq!(
            normalise(Path::new("Library/Preferences/x.plist")),
            "Library/Preferences/x.plist"
        );
    }

    #[test]
    fn fingerprint_inlines_a_small_plist() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.plist");
        let xml = include_str!("../tests/fixtures/sample.plist.xml");
        fs::write(&path, xml).expect("write");

        let size = fs::metadata(&path).expect("metadata").len();
        let (sha, content) = fingerprint(&path, size).expect("fingerprints");

        assert_eq!(sha.len(), 64, "sha256 is 32 bytes hex-encoded");
        let content = content.expect("plist content is inlined");
        assert_eq!(content["aString"], serde_json::json!("hello"));
    }

    #[test]
    fn fingerprint_does_not_inline_a_non_plist() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("note.txt");
        fs::write(&path, b"hello").expect("write");

        let (_sha, content) = fingerprint(&path, 5).expect("fingerprints");
        assert!(content.is_none(), "only plists are inlined");
    }

    #[test]
    fn fingerprint_skips_inlining_above_the_limit() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("big.plist");
        fs::write(&path, b"bplist00").expect("write");

        // Claim a size past the cap so the streaming path is taken.
        let (sha, content) = fingerprint(&path, INLINE_LIMIT + 1).expect("fingerprints");
        assert_eq!(sha.len(), 64);
        assert!(content.is_none(), "oversized files are hash-only");
    }

    #[test]
    fn stream_hash_matches_the_in_memory_hash() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("blob");
        let bytes = vec![0xab; 200 * 1024];
        fs::write(&path, &bytes).expect("write");

        assert_eq!(
            stream_hash(&path).expect("hashes"),
            hex(&Sha256::digest(&bytes))
        );
    }

    #[test]
    fn load_round_trips_a_snapshot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("snap.json");
        fs::write(&path, temp_snapshot_json(vec![entry("a.plist", "aa")])).expect("write");

        let snapshot = Snapshot::load(&path).expect("loads");
        assert_eq!(snapshot.group_id, "group.example");
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(
            snapshot.file("a.plist").map(|f| f.sha256.as_str()),
            Some("aa")
        );
    }

    #[test]
    fn load_rejects_a_newer_format_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("snap.json");
        let json = temp_snapshot_json(vec![]).replace("\"version\": 1", "\"version\": 99");
        fs::write(&path, json).expect("write");

        let err = Snapshot::load(&path).unwrap_err().to_string();
        assert!(err.contains("format 99"), "got: {err}");
    }

    #[test]
    fn load_rejects_malformed_json() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("snap.json");
        fs::write(&path, "{ not json").expect("write");

        assert!(Snapshot::load(&path).is_err());
    }

    #[test]
    fn optional_fields_are_omitted_when_absent() {
        let json = temp_snapshot_json(vec![entry("a", "aa")]);
        assert!(!json.contains("\"content\""), "got: {json}");
        assert!(!json.contains("\"modified\""), "got: {json}");
    }
}
