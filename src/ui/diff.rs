//! Rendering a change set.
//!
//! Uses the `+` / `-` / `~` vocabulary of a patch, with key-level changes nested
//! under the file they belong to, so the answer to "what did the app just write"
//! reads top to bottom.

use std::fmt::Write as _;

use serde_json::Value;

use crate::diff::{Change, ChangeSet, FileChange, KeyChange};

/// Style for additions.
const ADDED: anstyle::Style = anstyle::Style::new()
    .bold()
    .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Green)));

/// Style for removals.
const REMOVED: anstyle::Style = anstyle::Style::new()
    .bold()
    .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red)));

/// Style for modifications.
const MODIFIED: anstyle::Style = anstyle::Style::new()
    .bold()
    .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow)));

/// Style for de-emphasised detail.
const DIM: anstyle::Style = anstyle::Style::new().dimmed();

impl Change {
    /// The single character marking this kind of change.
    fn marker(self) -> char {
        match self {
            Self::Added => '+',
            Self::Removed => '-',
            Self::Modified => '~',
        }
    }

    /// The style for this kind of change.
    fn style(self) -> anstyle::Style {
        match self {
            Self::Added => ADDED,
            Self::Removed => REMOVED,
            Self::Modified => MODIFIED,
        }
    }
}

/// Renders a change set.
pub fn render(changes: &ChangeSet) -> String {
    if changes.is_empty() {
        return format!(
            "no changes in {} between {} and {}\n",
            changes.group_id, changes.before, changes.after
        );
    }

    let mut out = String::new();
    for file in &changes.files {
        render_file(&mut out, file);
    }
    out
}

/// Renders one changed file and any key detail beneath it.
fn render_file(out: &mut String, file: &FileChange) {
    let style = file.change.style();
    let _ = write!(
        out,
        "{style}{} {}{style:#}",
        file.change.marker(),
        file.path
    );

    match (file.size_before, file.size_after) {
        (Some(before), Some(after)) if before != after => {
            let _ = write!(out, " {DIM}({before} → {after} bytes){DIM:#}");
        }
        (None, Some(size)) | (Some(size), None) => {
            let _ = write!(out, " {DIM}({size} bytes){DIM:#}");
        }
        _ => {}
    }
    out.push('\n');

    match &file.keys {
        Some(keys) if keys.is_empty() => {
            // Bytes differ but the decoded content does not — worth saying so
            // rather than leaving a bare "modified" with nothing under it.
            let _ = writeln!(out, "  {DIM}contents differ but decode identically{DIM:#}");
        }
        Some(keys) => {
            for key in keys {
                render_key(out, key);
            }
        }
        None => {}
    }
}

/// Renders one changed key.
fn render_key(out: &mut String, key: &KeyChange) {
    let style = key.change.style();
    let label = if key.key.is_empty() {
        "(root)"
    } else {
        &key.key
    };
    let _ = write!(out, "  {style}{} {label}{style:#}", key.change.marker());

    match (&key.before, &key.after) {
        (Some(before), Some(after)) => {
            let _ = write!(out, ": {} → {}", scalar(before), scalar(after));
        }
        (None, Some(after)) => {
            let _ = write!(out, ": {}", scalar(after));
        }
        (Some(before), None) => {
            let _ = write!(out, ": {}", scalar(before));
        }
        (None, None) => {}
    }
    out.push('\n');
}

/// Formats a value compactly enough to sit on one line.
///
/// Containers are summarised rather than expanded: a diff line reports *that* a
/// subtree changed, and the keys beneath it are reported on their own lines.
fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => format!("{value:?}"),
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::Object(entries) => format!("{{{} keys}}", entries.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Strips ANSI escapes so assertions can talk about visible text.
    fn plain(styled: &str) -> String {
        let mut out = String::new();
        let mut chars = styled.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                for escape in chars.by_ref() {
                    if escape == 'm' {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn changes(files: Vec<FileChange>) -> ChangeSet {
        ChangeSet {
            group_id: "group.example".into(),
            before: "t0".into(),
            after: "t1".into(),
            files,
        }
    }

    fn modified(path: &str, keys: Option<Vec<KeyChange>>) -> FileChange {
        FileChange {
            path: path.into(),
            change: Change::Modified,
            size_before: Some(100),
            size_after: Some(100),
            keys,
        }
    }

    #[test]
    fn render_reports_a_changed_key_under_its_file() {
        let set = changes(vec![modified(
            "Library/Preferences/group.app.natively.shared.plist",
            Some(vec![KeyChange {
                key: "usageCount".into(),
                change: Change::Modified,
                before: Some(json!(3)),
                after: Some(json!(4)),
            }]),
        )]);

        assert_eq!(
            plain(&render(&set)),
            "~ Library/Preferences/group.app.natively.shared.plist\n  ~ usageCount: 3 → 4\n"
        );
    }

    #[test]
    fn render_shows_nested_key_paths() {
        let set = changes(vec![modified(
            "a.plist",
            Some(vec![KeyChange {
                key: "keyboard_diagnostics.net_apple".into(),
                change: Change::Modified,
                before: Some(json!("ok")),
                after: Some(json!("slow")),
            }]),
        )]);

        assert!(
            plain(&render(&set)).contains("~ keyboard_diagnostics.net_apple: \"ok\" → \"slow\""),
            "got: {}",
            plain(&render(&set))
        );
    }

    #[test]
    fn render_marks_additions_and_removals() {
        let set = changes(vec![modified(
            "a.plist",
            Some(vec![
                KeyChange {
                    key: "fresh".into(),
                    change: Change::Added,
                    before: None,
                    after: Some(json!(1)),
                },
                KeyChange {
                    key: "gone".into(),
                    change: Change::Removed,
                    before: Some(json!(2)),
                    after: None,
                },
            ]),
        )]);

        let rendered = plain(&render(&set));
        assert!(rendered.contains("+ fresh: 1"), "got: {rendered}");
        assert!(rendered.contains("- gone: 2"), "got: {rendered}");
    }

    #[test]
    fn render_notes_a_size_change_on_the_file_line() {
        let mut file = modified("a.plist", None);
        file.size_after = Some(140);
        let rendered = plain(&render(&changes(vec![file])));
        assert!(rendered.contains("(100 → 140 bytes)"), "got: {rendered}");
    }

    #[test]
    fn render_explains_a_hash_only_change() {
        let rendered = plain(&render(&changes(vec![modified("a.plist", Some(vec![]))])));
        assert!(
            rendered.contains("contents differ but decode identically"),
            "got: {rendered}"
        );
    }

    #[test]
    fn render_summarises_containers_rather_than_expanding_them() {
        let set = changes(vec![modified(
            "a.plist",
            Some(vec![KeyChange {
                key: "items".into(),
                change: Change::Added,
                before: None,
                after: Some(json!([1, 2, 3])),
            }]),
        )]);

        assert!(plain(&render(&set)).contains("+ items: [3 items]"));
    }

    #[test]
    fn render_of_no_changes_says_so() {
        let rendered = plain(&render(&changes(vec![])));
        assert!(
            rendered.contains("no changes in group.example"),
            "got: {rendered}"
        );
    }
}
