//! Filesystem-backed container reading.
//!
//! A [`Container`] is just a root directory. Discovery is what produces that root;
//! nothing in this module knows how it was found.

use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Result, bail};
use serde::Serialize;
use walkdir::WalkDir;

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symlink, which is reported but never followed.
    Symlink,
    /// A socket, fifo, or device node.
    Other,
    /// An entry that could not be read; see [`Entry::error`].
    Unreadable,
}

/// One entry in a container's tree.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// Path relative to the walk's starting directory.
    pub path: PathBuf,
    /// Depth below the starting directory; its immediate children are 1.
    pub depth: usize,
    /// What this entry is.
    pub kind: EntryKind,
    /// Size in bytes. Always 0 for directories.
    pub size: u64,
    /// Last modification time, absent when the filesystem did not report one.
    #[serde(serialize_with = "serialize_timestamp")]
    pub modified: Option<SystemTime>,
    /// Why this entry could not be read, when `kind` is [`EntryKind::Unreadable`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Entry {
    /// The final path component, as shown in a tree.
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// How to traverse a container.
#[derive(Debug, Clone, Copy, Default)]
pub struct WalkOptions {
    /// Maximum depth below the starting directory; `None` walks the whole tree.
    pub max_depth: Option<usize>,
    /// Include entries whose name begins with a dot.
    pub all: bool,
}

/// A container on the local filesystem.
#[derive(Debug, Clone)]
pub struct Container {
    root: PathBuf,
}

impl Container {
    /// Wraps a container root directory.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The container's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a caller-supplied relative path against the container root.
    ///
    /// # Errors
    ///
    /// Rejects absolute paths and any `..` component, so a path argument cannot
    /// walk out of the container it names.
    pub fn resolve(&self, relative: Option<&Path>) -> Result<PathBuf> {
        let Some(relative) = relative else {
            return Ok(self.root.clone());
        };

        for component in relative.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir => {
                    bail!("path `{}` escapes the container", relative.display())
                }
                Component::RootDir | Component::Prefix(_) => {
                    bail!(
                        "path `{}` must be relative to the container",
                        relative.display()
                    )
                }
            }
        }

        Ok(self.root.join(relative))
    }

    /// Walks the tree beneath `start`, in depth-first order sorted by name.
    ///
    /// Symlinks are reported but never followed, so a container cannot send the
    /// walk somewhere else on the host. Entries that cannot be read become
    /// [`EntryKind::Unreadable`] rows rather than aborting the walk — a single
    /// permission-denied directory should not cost you the rest of the listing.
    pub fn walk(&self, start: &Path, options: &WalkOptions) -> Result<Vec<Entry>> {
        if !start.exists() {
            bail!("`{}` does not exist in this container", self.display(start));
        }

        let mut walker = WalkDir::new(start)
            .min_depth(1)
            .follow_links(false)
            .sort_by_file_name();
        if let Some(max_depth) = options.max_depth {
            walker = walker.max_depth(max_depth);
        }

        let all = options.all;
        let entries = walker
            .into_iter()
            .filter_entry(move |entry| all || !is_hidden(entry.file_name()))
            .map(|result| match result {
                Ok(entry) => Self::describe(start, &entry),
                Err(error) => Entry {
                    path: error
                        .path()
                        .and_then(|path| path.strip_prefix(start).ok())
                        .map(Path::to_path_buf)
                        .unwrap_or_default(),
                    depth: error.depth(),
                    kind: EntryKind::Unreadable,
                    size: 0,
                    modified: None,
                    error: Some(error.to_string()),
                },
            })
            .collect();

        Ok(entries)
    }

    /// Renders a path for messages, relative to the container root where possible.
    fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// Turns a walkdir entry into an [`Entry`], recording metadata failures inline.
    fn describe(start: &Path, entry: &walkdir::DirEntry) -> Entry {
        let path = entry
            .path()
            .strip_prefix(start)
            .unwrap_or(entry.path())
            .to_path_buf();

        let file_type = entry.file_type();
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Dir
        } else if file_type.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };

        // `entry.metadata()` does not traverse symlinks, matching follow_links(false).
        match entry.metadata() {
            Ok(metadata) => Entry {
                path,
                depth: entry.depth(),
                kind,
                size: if kind == EntryKind::Dir {
                    0
                } else {
                    metadata.len()
                },
                modified: metadata.modified().ok(),
                error: None,
            },
            Err(error) => Entry {
                path,
                depth: entry.depth(),
                kind: EntryKind::Unreadable,
                size: 0,
                modified: None,
                error: Some(error.to_string()),
            },
        }
    }
}

/// Whether a filename begins with a dot.
fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// Serialises a timestamp as RFC 3339 rather than serde's default for
/// `SystemTime`, which is a `{secs_since_epoch, nanos_since_epoch}` pair that no
/// consumer of `--json` would want to handle.
fn serialize_timestamp<S: serde::Serializer>(
    time: &Option<SystemTime>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match time {
        Some(time) => serializer.serialize_str(
            &chrono::DateTime::<chrono::Local>::from(*time)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds a container fixture on disk and returns it with its temp dir.
    fn fixture() -> (tempfile::TempDir, Container) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();

        fs::create_dir_all(root.join("Library/Preferences")).expect("dirs");
        fs::create_dir_all(root.join("Library/Caches/Empty")).expect("dirs");
        fs::write(
            root.join("Library/Preferences/group.example.plist"),
            b"bplist00xx",
        )
        .expect("file");
        fs::write(root.join("note.txt"), b"hello").expect("file");
        fs::write(root.join(".hidden"), b"x").expect("file");

        let container = Container::new(root);
        (dir, container)
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(Entry::name).collect()
    }

    #[test]
    fn walk_lists_the_whole_tree_in_sorted_order() {
        let (_dir, container) = fixture();
        let entries = container
            .walk(container.root(), &WalkOptions::default())
            .expect("walks");

        assert_eq!(
            names(&entries),
            vec![
                "Library",
                "Caches",
                "Empty",
                "Preferences",
                "group.example.plist",
                "note.txt",
            ]
        );
    }

    #[test]
    fn walk_hides_dotfiles_unless_all_is_set() {
        let (_dir, container) = fixture();

        let hidden = container
            .walk(container.root(), &WalkOptions::default())
            .expect("walks");
        assert!(!names(&hidden).contains(&".hidden".to_string()));

        let shown = container
            .walk(
                container.root(),
                &WalkOptions {
                    all: true,
                    ..WalkOptions::default()
                },
            )
            .expect("walks");
        assert!(names(&shown).contains(&".hidden".to_string()));
    }

    #[test]
    fn walk_honours_max_depth() {
        let (_dir, container) = fixture();
        let entries = container
            .walk(
                container.root(),
                &WalkOptions {
                    max_depth: Some(1),
                    ..WalkOptions::default()
                },
            )
            .expect("walks");

        assert_eq!(names(&entries), vec!["Library", "note.txt"]);
    }

    #[test]
    fn walk_records_sizes_and_kinds() {
        let (_dir, container) = fixture();
        let entries = container
            .walk(container.root(), &WalkOptions::default())
            .expect("walks");

        let note = entries
            .iter()
            .find(|entry| entry.name() == "note.txt")
            .expect("note.txt is listed");
        assert_eq!(note.kind, EntryKind::File);
        assert_eq!(note.size, 5);
        assert!(note.modified.is_some());

        let library = entries
            .iter()
            .find(|entry| entry.name() == "Library")
            .expect("Library is listed");
        assert_eq!(library.kind, EntryKind::Dir);
        assert_eq!(library.size, 0, "directory sizes are not meaningful");
    }

    #[test]
    fn walk_reports_symlinks_without_following_them() {
        let (dir, container) = fixture();
        let outside = dir.path().parent().expect("parent");
        std::os::unix::fs::symlink(outside, container.root().join("escape")).expect("symlink");

        let entries = container
            .walk(container.root(), &WalkOptions::default())
            .expect("walks");

        let link = entries
            .iter()
            .find(|entry| entry.name() == "escape")
            .expect("symlink is listed");
        assert_eq!(link.kind, EntryKind::Symlink);

        // Following it would have pulled in the whole parent directory.
        assert!(
            entries.iter().all(|entry| entry.depth <= 3),
            "walk stayed inside the container"
        );
    }

    #[test]
    fn entries_serialise_timestamps_as_rfc_3339() {
        let (_dir, container) = fixture();
        let entries = container
            .walk(container.root(), &WalkOptions::default())
            .expect("walks");
        let note = entries
            .iter()
            .find(|entry| entry.name() == "note.txt")
            .expect("note.txt is listed");

        let json = serde_json::to_value(note).expect("serialises");
        let modified = json["modified"].as_str().expect("a string, not a struct");
        // e.g. 2026-07-30T21:36:02+01:00 — parseable by any consumer.
        assert!(
            chrono::DateTime::parse_from_rfc3339(modified).is_ok(),
            "got: {modified}"
        );
    }

    #[test]
    fn walk_reports_unreadable_directories_without_aborting() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, container) = fixture();
        let locked = container.root().join("Library/Locked");
        fs::create_dir(&locked).expect("dir");
        fs::write(locked.join("secret.txt"), b"x").expect("file");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");

        let entries = container.walk(container.root(), &WalkOptions::default());

        // Restore before asserting so the temp dir can always be cleaned up.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod back");

        let entries = entries.expect("walk succeeds despite the locked directory");
        assert!(
            entries
                .iter()
                .any(|entry| entry.kind == EntryKind::Unreadable && entry.error.is_some()),
            "the locked directory is reported inline"
        );
        assert!(
            names(&entries).contains(&"note.txt".to_string()),
            "the rest of the tree is still listed"
        );
    }

    #[test]
    fn walk_rejects_a_missing_start_path() {
        let (_dir, container) = fixture();
        let missing = container.root().join("nope");
        assert!(container.walk(&missing, &WalkOptions::default()).is_err());
    }

    #[test]
    fn resolve_defaults_to_the_container_root() {
        let (_dir, container) = fixture();
        assert_eq!(container.resolve(None).expect("resolves"), container.root());
    }

    #[test]
    fn resolve_joins_a_relative_path() {
        let (_dir, container) = fixture();
        let resolved = container
            .resolve(Some(Path::new("Library/Preferences")))
            .expect("resolves");
        assert_eq!(resolved, container.root().join("Library/Preferences"));
    }

    #[test]
    fn resolve_rejects_paths_that_escape_the_container() {
        let (_dir, container) = fixture();
        assert!(container.resolve(Some(Path::new("../elsewhere"))).is_err());
        assert!(
            container
                .resolve(Some(Path::new("Library/../../elsewhere")))
                .is_err()
        );
    }

    #[test]
    fn resolve_rejects_absolute_paths() {
        let (_dir, container) = fixture();
        assert!(container.resolve(Some(Path::new("/etc/passwd"))).is_err());
    }
}
