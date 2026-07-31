//! Rendering decoded values and raw bytes for a terminal.
//!
//! Values render in an indented, YAML-shaped form rather than a flat table: the
//! contents of a shared `UserDefaults` suite nest, and a table would either hide
//! the nesting or need a column per level.

use std::fmt::Write as _;

use crate::decode::Value;

/// Spaces per nesting level.
const INDENT: &str = "  ";

/// How many bytes of a `Data` value to preview inline.
const DATA_PREVIEW: usize = 16;

/// Renders a decoded value.
///
/// Strings are quoted so an empty string, a numeric-looking string, and trailing
/// whitespace are all visible — the distinctions that matter when you are
/// checking what an app actually wrote.
pub fn render(value: &Value) -> String {
    let mut out = String::new();
    render_into(&mut out, value, 0);
    out
}

/// Appends `value` to `out` at the given nesting level.
fn render_into(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Dict(entries) if !entries.is_empty() => {
            for (key, entry) in entries {
                indent(out, depth);
                let _ = write!(out, "{key}:");
                render_child(out, entry, depth);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                indent(out, depth);
                out.push('-');
                render_child(out, item, depth);
            }
        }
        other => {
            indent(out, depth);
            out.push_str(&scalar(other));
            out.push('\n');
        }
    }
}

/// Writes a value that follows a `key:` or `-` marker, inline when it is a scalar
/// and on following lines when it nests.
fn render_child(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Dict(entries) if !entries.is_empty() => {
            out.push('\n');
            render_into(out, value, depth + 1);
        }
        Value::Array(items) if !items.is_empty() => {
            out.push('\n');
            render_into(out, value, depth + 1);
        }
        other => {
            let _ = writeln!(out, " {}", scalar(other));
        }
    }
}

/// Writes one indent level per depth.
fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

/// Formats a value that fits on one line.
fn scalar(value: &Value) -> String {
    match value {
        Value::Bool(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        // An integral real would otherwise print as `2`, indistinguishable from
        // the integer 2 — and which one an app wrote is exactly the kind of
        // detail this tool exists to show.
        Value::Real(value) if value.fract() == 0.0 && value.is_finite() => format!("{value:.1}"),
        Value::Real(value) => value.to_string(),
        // `{:?}` quotes and escapes, making control characters and trailing
        // spaces visible instead of silently changing how a value looks.
        Value::String(value) => format!("{value:?}"),
        Value::Date(value) => value.clone(),
        Value::Uid(value) => format!("<uid {value}>"),
        Value::Data(bytes) => data_preview(bytes),
        Value::Dict(_) => "{}".to_string(),
        Value::Array(_) => "[]".to_string(),
    }
}

/// Summarises a `Data` value as a byte count plus a short hex preview.
fn data_preview(bytes: &[u8]) -> String {
    let shown: String = bytes
        .iter()
        .take(DATA_PREVIEW)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let ellipsis = if bytes.len() > DATA_PREVIEW {
        "…"
    } else {
        ""
    };
    format!("<{} bytes> {shown}{ellipsis}", bytes.len())
}

/// Renders bytes as a classic hexdump.
///
/// `limit` caps the output; `0` means no cap. When bytes are omitted a trailing
/// line says how many, so a truncated dump never looks like the whole file.
pub fn hexdump(bytes: &[u8], limit: usize) -> String {
    let shown = if limit == 0 {
        bytes
    } else {
        bytes.get(..limit).unwrap_or(bytes)
    };

    let mut out = String::new();
    for (index, chunk) in shown.chunks(16).enumerate() {
        let offset = index * 16;
        let _ = write!(out, "{offset:08x}  ");

        for position in 0..16 {
            match chunk.get(position) {
                Some(byte) => {
                    let _ = write!(out, "{byte:02x} ");
                }
                None => out.push_str("   "),
            }
            if position == 7 {
                out.push(' ');
            }
        }

        out.push_str(" |");
        for byte in chunk {
            out.push(if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }

    let omitted = bytes.len().saturating_sub(shown.len());
    if omitted > 0 {
        let _ = writeln!(out, "… {omitted} more bytes (use --limit 0 for all)");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn dict(entries: &[(&str, Value)]) -> Value {
        Value::Dict(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn render_writes_scalars_one_per_line() {
        let value = dict(&[
            ("chineseModeEnabled", Value::Bool(false)),
            ("usageCount", Value::Integer(4)),
            ("usageMonth", Value::String("2026-07".into())),
        ]);

        assert_eq!(
            render(&value),
            "chineseModeEnabled: false\nusageCount: 4\nusageMonth: \"2026-07\"\n"
        );
    }

    #[test]
    fn render_indents_nested_dictionaries() {
        let value = dict(&[
            (
                "keyboard_diagnostics",
                dict(&[
                    ("net_apple", Value::String("ok".into())),
                    ("net_worker", Value::String("slow".into())),
                ]),
            ),
            ("usageCount", Value::Integer(4)),
        ]);

        assert_eq!(
            render(&value),
            "keyboard_diagnostics:\n  net_apple: \"ok\"\n  net_worker: \"slow\"\nusageCount: 4\n"
        );
    }

    #[test]
    fn render_writes_arrays_as_bullets() {
        let value = dict(&[(
            "favourites",
            Value::Array(vec![
                Value::String("one".into()),
                Value::Integer(2),
                Value::Bool(false),
            ]),
        )]);

        assert_eq!(
            render(&value),
            "favourites:\n  - \"one\"\n  - 2\n  - false\n"
        );
    }

    #[test]
    fn render_quotes_strings_so_empties_and_numbers_are_distinguishable() {
        let value = dict(&[
            ("empty", Value::String(String::new())),
            ("numeric", Value::String("4".into())),
            ("trailing", Value::String("x ".into())),
            ("actual", Value::Integer(4)),
        ]);

        let rendered = render(&value);
        assert!(rendered.contains("empty: \"\""), "got: {rendered}");
        assert!(rendered.contains("numeric: \"4\""), "got: {rendered}");
        assert!(rendered.contains("trailing: \"x \""), "got: {rendered}");
        assert!(rendered.contains("actual: 4"), "got: {rendered}");
    }

    #[test]
    fn render_distinguishes_integral_reals_from_integers() {
        let value = dict(&[
            ("real", Value::Real(2.0)),
            ("integer", Value::Integer(2)),
            ("fractional", Value::Real(3.5)),
        ]);

        let rendered = render(&value);
        assert!(rendered.contains("real: 2.0"), "got: {rendered}");
        assert!(rendered.contains("integer: 2\n"), "got: {rendered}");
        assert!(rendered.contains("fractional: 3.5"), "got: {rendered}");
    }

    #[test]
    fn render_shows_non_finite_reals_without_panicking() {
        let value = dict(&[("nan", Value::Real(f64::NAN))]);
        assert!(render(&value).contains("nan: NaN"));
    }

    #[test]
    fn render_shows_empty_containers_inline() {
        let value = dict(&[
            ("emptyDict", Value::Dict(BTreeMap::new())),
            ("emptyArray", Value::Array(Vec::new())),
        ]);

        assert_eq!(render(&value), "emptyArray: []\nemptyDict: {}\n");
    }

    #[test]
    fn render_previews_data_with_a_byte_count() {
        let value = dict(&[("blob", Value::Data(b"hello".to_vec()))]);
        assert_eq!(render(&value), "blob: <5 bytes> 68656c6c6f\n");
    }

    #[test]
    fn render_truncates_a_long_data_preview() {
        let value = dict(&[("blob", Value::Data(vec![0xab; 40]))]);
        let rendered = render(&value);
        assert!(rendered.contains("<40 bytes>"), "got: {rendered}");
        assert!(rendered.ends_with("…\n"), "got: {rendered}");
    }

    #[test]
    fn hexdump_lays_out_offsets_bytes_and_ascii() {
        let dumped = hexdump(b"bplist00\x01\x02", 0);
        assert_eq!(
            dumped,
            "00000000  62 70 6c 69 73 74 30 30  01 02                    |bplist00..|\n"
        );
    }

    #[test]
    fn hexdump_honours_a_limit_and_says_what_it_dropped() {
        let dumped = hexdump(&[0x41; 40], 16);
        assert!(dumped.contains("… 24 more bytes"), "got: {dumped}");
        assert_eq!(dumped.lines().count(), 2);
    }

    #[test]
    fn hexdump_of_nothing_is_empty() {
        assert_eq!(hexdump(b"", 0), "");
    }
}
