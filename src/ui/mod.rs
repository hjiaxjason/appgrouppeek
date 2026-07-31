//! Terminal rendering: colour policy, aligned tables, and error reporting.
//!
//! Colour is emitted unconditionally into the returned strings and stripped at
//! write time by `anstream` when stdout is not a terminal, so callers never have
//! to branch on whether colour is wanted.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthStr;

/// Style for table headers.
const HEADER: anstyle::Style = anstyle::Style::new().bold();

/// Style for de-emphasised cells such as identifiers and paths.
const DIM: anstyle::Style = anstyle::Style::new().dimmed();

/// Style for the `error:` prefix.
const ERROR: anstyle::Style = anstyle::Style::new()
    .bold()
    .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red)));

/// Applies the global colour policy.
///
/// `anstream` already honours `NO_COLOR` and non-terminal stdout; this only adds
/// the explicit `--no-color` override.
pub fn init_color(no_color: bool) {
    if no_color {
        anstream::ColorChoice::Never.write_global();
    }
}

/// A column in a rendered table.
pub struct Column<'a> {
    /// Header text.
    pub header: &'a str,
    /// Whether the column's cells should be de-emphasised.
    pub dim: bool,
}

impl<'a> Column<'a> {
    /// A normally-styled column.
    pub fn new(header: &'a str) -> Self {
        Self { header, dim: false }
    }

    /// A de-emphasised column, for identifiers and paths.
    pub fn dim(header: &'a str) -> Self {
        Self { header, dim: true }
    }
}

/// Renders rows as a left-aligned table with a styled header.
///
/// Widths are measured in terminal columns rather than bytes or `char`s: a CJK
/// glyph is a single `char` but occupies two columns, so counting `char`s would
/// visibly misalign the table for non-ASCII paths and device names.
///
/// The final column is not padded, keeping trailing whitespace out of copy-paste.
pub fn table(columns: &[Column<'_>], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = columns.iter().map(|c| display_width(c.header)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(display_width(cell));
            }
        }
    }

    let mut out = String::new();
    let last = columns.len().saturating_sub(1);

    for (index, column) in columns.iter().enumerate() {
        let padding = pad(column.header, widths.get(index).copied().unwrap_or(0));
        let _ = write!(out, "{HEADER}{}{HEADER:#}", column.header);
        if index != last {
            let _ = write!(out, "{padding}  ");
        }
    }
    out.push('\n');

    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            let padding = pad(cell, widths.get(index).copied().unwrap_or(0));
            let dim = columns.get(index).is_some_and(|column| column.dim);
            if dim {
                let _ = write!(out, "{DIM}{cell}{DIM:#}");
            } else {
                out.push_str(cell);
            }
            if index != last {
                let _ = write!(out, "{padding}  ");
            }
        }
        out.push('\n');
    }

    out
}

/// How many terminal columns `text` occupies.
fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Spaces needed to pad `cell` out to `width` terminal columns.
fn pad(cell: &str, width: usize) -> String {
    " ".repeat(width.saturating_sub(display_width(cell)))
}

/// Prints an error and its full cause chain to stderr.
pub fn print_error(error: &anyhow::Error) {
    eprintln!("{ERROR}error:{ERROR:#} {error}");
    for cause in error.chain().skip(1) {
        eprintln!("  caused by: {cause}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn table_aligns_columns_and_omits_trailing_padding() {
        let rendered = table(
            &[Column::new("NAME"), Column::new("STATE")],
            &[
                vec!["iPhone 17".into(), "Booted".into()],
                vec!["iPad".into(), "Shutdown".into()],
            ],
        );

        let lines: Vec<String> = plain(&rendered).lines().map(str::to_string).collect();
        assert_eq!(lines[0], "NAME       STATE");
        assert_eq!(lines[1], "iPhone 17  Booted");
        assert_eq!(lines[2], "iPad       Shutdown");
        assert!(
            lines.iter().all(|line| line == line.trim_end()),
            "no trailing whitespace"
        );
    }

    #[test]
    fn table_measures_width_in_terminal_columns() {
        let rendered = table(
            &[Column::new("NAME"), Column::new("X")],
            &[
                vec!["日本語".into(), "a".into()],
                vec!["ab".into(), "b".into()],
            ],
        );

        let lines: Vec<String> = plain(&rendered).lines().map(str::to_string).collect();
        // "日本語" is 3 chars and 9 bytes, but occupies 6 terminal columns — so it
        // sets the column width, and "ab" is padded out to match it.
        assert_eq!(lines[1], "日本語  a");
        assert_eq!(lines[2], "ab      b");
    }

    #[test]
    fn table_renders_header_only_when_there_are_no_rows() {
        let rendered = table(&[Column::new("NAME")], &[]);
        assert_eq!(plain(&rendered), "NAME\n");
    }
}
