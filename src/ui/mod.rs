//! Terminal rendering: colour policy, aligned tables, and error reporting.
//!
//! Colour is emitted unconditionally into the returned strings and stripped at
//! write time by `anstream` when stdout is not a terminal, so callers never have
//! to branch on whether colour is wanted.

pub mod tree;
pub mod value;

use std::fmt::Write as _;
use std::time::SystemTime;

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
/// Each line is trimmed of trailing spaces, so a row whose rightmost cells are
/// empty — a tree's root row, say — does not leave padding behind for copy-paste.
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

    let mut line = String::new();
    for (index, column) in columns.iter().enumerate() {
        let padding = pad(column.header, widths.get(index).copied().unwrap_or(0));
        let _ = write!(line, "{HEADER}{}{HEADER:#}", column.header);
        if index != last {
            let _ = write!(line, "{padding}  ");
        }
    }
    push_line(&mut out, &line);

    for row in rows {
        line.clear();
        for (index, cell) in row.iter().enumerate() {
            let padding = pad(cell, widths.get(index).copied().unwrap_or(0));
            let dim = columns.get(index).is_some_and(|column| column.dim);
            if dim && !cell.is_empty() {
                let _ = write!(line, "{DIM}{cell}{DIM:#}");
            } else {
                line.push_str(cell);
            }
            if index != last {
                let _ = write!(line, "{padding}  ");
            }
        }
        push_line(&mut out, &line);
    }

    out
}

/// Appends a line with its trailing padding removed.
fn push_line(out: &mut String, line: &str) {
    out.push_str(line.trim_end_matches(' '));
    out.push('\n');
}

/// How many terminal columns `text` occupies.
fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Formats a byte count for humans.
///
/// Uses binary multiples, since that is what the filesystem reports, but the
/// familiar `KB`/`MB` labels. Exact byte counts are kept below 1 KiB because at
/// that scale the precise number is usually what you are checking.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    const STEP: f64 = 1024.0;

    if bytes < STEP as u64 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64 / STEP;
    let mut unit = 0;
    while value >= STEP && unit + 1 < UNITS.len() {
        value /= STEP;
        unit += 1;
    }

    // One decimal below 10 keeps 1.5 KB readable without noise at 250 KB.
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Formats a timestamp in local time, to the minute.
///
/// Returns an empty string when the filesystem reported no time, so the column
/// simply stays blank rather than showing a fabricated epoch.
pub fn format_time(time: Option<SystemTime>) -> String {
    time.map_or_else(String::new, |time| {
        chrono::DateTime::<chrono::Local>::from(time)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
}

/// Spaces needed to pad `cell` out to `width` terminal columns.
fn pad(cell: &str, width: usize) -> String {
    " ".repeat(width.saturating_sub(display_width(cell)))
}

/// Prints an error and its full cause chain to stderr.
///
/// Uses `anstream` rather than `std::eprintln!` so the styling is stripped when
/// stderr is redirected to a file or pipe.
pub fn print_error(error: &anyhow::Error) {
    anstream::eprintln!("{ERROR}error:{ERROR:#} {error}");
    for cause in error.chain().skip(1) {
        anstream::eprintln!("  caused by: {cause}");
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

    #[test]
    fn table_leaves_no_padding_when_trailing_cells_are_empty() {
        // A tree's root row has a label but no size or mtime; it must not trail
        // spaces out to the width of the columns it left blank.
        let rendered = table(
            &[
                Column::new("NAME"),
                Column::new("SIZE"),
                Column::dim("MODIFIED"),
            ],
            &[
                vec!["group.example".into(), String::new(), String::new()],
                vec!["child".into(), "266 B".into(), "2026-07-30 21:36".into()],
            ],
        );

        for line in plain(&rendered).lines() {
            assert_eq!(line, line.trim_end(), "line has trailing padding: {line:?}");
        }
    }

    #[test]
    fn human_size_keeps_exact_bytes_below_a_kilobyte() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(266), "266 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn human_size_scales_to_larger_units() {
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(256 * 1024), "256 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn format_time_is_blank_when_unknown() {
        assert_eq!(format_time(None), "");
    }

    #[test]
    fn format_time_renders_to_the_minute() {
        let rendered = format_time(Some(SystemTime::UNIX_EPOCH));
        assert_eq!(rendered.len(), "2026-07-30 21:36".len(), "got: {rendered}");
    }
}
