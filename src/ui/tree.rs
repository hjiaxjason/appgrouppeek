//! Tree rendering.
//!
//! Kept independent of how entries were discovered: callers hand over a flat list
//! of [`Node`]s in depth-first order and this works out the connector prefixes.
//! Alignment is delegated to [`table`](super::table), so the detail columns line
//! up the same way they do everywhere else in the tool.

use super::{Column, table};

/// One line of a tree.
pub struct Node<'a> {
    /// Depth below the root; the root's immediate children are 1.
    pub depth: usize,
    /// Text to show for this entry, normally a file name.
    pub name: &'a str,
    /// Right-hand detail column, normally a size.
    pub detail: String,
    /// Second detail column, normally a modification time.
    pub modified: String,
}

/// Connector drawn for an entry that has siblings after it.
const BRANCH: &str = "├── ";
/// Connector drawn for the last entry at its level.
const LAST_BRANCH: &str = "└── ";
/// Filler drawn under an ancestor that has siblings after it.
const VERTICAL: &str = "│   ";
/// Filler drawn under an ancestor that was the last at its level.
const BLANK: &str = "    ";

/// Whether each node is the last among its siblings.
///
/// A node is last when no later node shares its depth before one shallower than it
/// appears — that shallower node is where the parent's sibling list ends.
fn last_flags(nodes: &[Node<'_>]) -> Vec<bool> {
    let mut flags = vec![true; nodes.len()];

    for (index, node) in nodes.iter().enumerate() {
        for later in &nodes[index + 1..] {
            if later.depth < node.depth {
                break;
            }
            if later.depth == node.depth {
                flags[index] = false;
                break;
            }
        }
    }

    flags
}

/// Builds the connector prefix for each node from the ancestors' last-flags.
fn prefixes(nodes: &[Node<'_>]) -> Vec<String> {
    let flags = last_flags(nodes);
    // ancestors[d] is whether the node at depth d+1 on the current path was last.
    let mut ancestors: Vec<bool> = Vec::new();
    let mut out = Vec::with_capacity(nodes.len());

    for (index, node) in nodes.iter().enumerate() {
        let depth = node.depth.max(1);
        ancestors.truncate(depth - 1);

        let mut prefix = String::new();
        for &ancestor_was_last in &ancestors {
            prefix.push_str(if ancestor_was_last { BLANK } else { VERTICAL });
        }
        prefix.push_str(if flags[index] { LAST_BRANCH } else { BRANCH });

        out.push(prefix);
        ancestors.push(flags[index]);
    }

    out
}

/// Renders a tree with aligned size and modified columns.
///
/// `root_label` is shown above the tree as its own row, so the listing carries the
/// name of what is being listed.
pub fn render(root_label: &str, nodes: &[Node<'_>]) -> String {
    let prefixes = prefixes(nodes);

    let mut rows = Vec::with_capacity(nodes.len() + 1);
    rows.push(vec![root_label.to_string(), String::new(), String::new()]);

    for (node, prefix) in nodes.iter().zip(prefixes) {
        rows.push(vec![
            format!("{prefix}{}", node.name),
            node.detail.clone(),
            node.modified.clone(),
        ]);
    }

    table(
        &[
            Column::new("NAME"),
            Column::new("SIZE"),
            Column::dim("MODIFIED"),
        ],
        &rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(depth: usize, name: &str) -> Node<'_> {
        Node {
            depth,
            name,
            detail: String::new(),
            modified: String::new(),
        }
    }

    /// Strips ANSI escapes and the trailing detail columns, leaving the tree.
    fn shape(rendered: &str) -> Vec<String> {
        rendered
            .lines()
            .skip(1) // header
            .map(|line| {
                let plain: String = {
                    let mut out = String::new();
                    let mut chars = line.chars();
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
                };
                plain.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn render_draws_connectors_for_a_nested_tree() {
        let nodes = vec![
            node(1, "Library"),
            node(2, "Caches"),
            node(3, "AppConfiguration"),
            node(2, "Preferences"),
            node(3, "group.example.plist"),
            node(1, "note.txt"),
        ];

        assert_eq!(
            shape(&render("group.example", &nodes)),
            vec![
                "group.example",
                "├── Library",
                "│   ├── Caches",
                "│   │   └── AppConfiguration",
                "│   └── Preferences",
                "│       └── group.example.plist",
                "└── note.txt",
            ]
        );
    }

    #[test]
    fn render_marks_the_only_child_as_last() {
        let nodes = vec![node(1, "Library"), node(2, "Preferences")];

        assert_eq!(
            shape(&render("root", &nodes)),
            vec!["root", "└── Library", "    └── Preferences"]
        );
    }

    #[test]
    fn render_handles_siblings_after_a_deep_subtree() {
        let nodes = vec![node(1, "a"), node(2, "a1"), node(3, "a1x"), node(1, "b")];

        assert_eq!(
            shape(&render("root", &nodes)),
            vec!["root", "├── a", "│   └── a1", "│       └── a1x", "└── b"]
        );
    }

    #[test]
    fn render_of_an_empty_tree_is_just_the_root() {
        assert_eq!(shape(&render("root", &[])), vec!["root"]);
    }

    #[test]
    fn render_aligns_detail_columns() {
        let nodes = vec![
            Node {
                depth: 1,
                name: "short.txt",
                detail: "5 B".into(),
                modified: "2026-07-30 21:36".into(),
            },
            Node {
                depth: 1,
                name: "a-much-longer-name.plist",
                detail: "266 B".into(),
                modified: "2026-07-30 21:37".into(),
            },
        ];

        let lines = shape(&render("root", &nodes));
        // Both size values start at the same column despite differing name widths.
        let first = lines[1].find("5 B").expect("size present");
        let second = lines[2].find("266 B").expect("size present");
        assert_eq!(first, second);
    }
}
