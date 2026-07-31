//! Making container bytes human-readable.
//!
//! App Group contents are mostly opaque binary, and this module is where that is
//! dealt with. Two rules govern everything here:
//!
//! * **Decoding never fails the command.** [`decode`] returns a best-effort result
//!   and cannot return an error — anything it does not understand, or understands
//!   but cannot parse, comes back as [`Body::Opaque`] with a note explaining why,
//!   for the caller to hexdump. A debugging tool that panics on the thing you were
//!   trying to debug is worse than useless.
//! * **Content is sniffed, never inferred from the filename.** A `.plist`
//!   extension on a JPEG proves nothing about what an app actually wrote.

pub mod plist;

use std::collections::BTreeMap;

use base64::Engine as _;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

/// A decoded value, covering the property-list type system.
///
/// This exists rather than decoding straight to [`serde_json::Value`] because
/// plists carry three types JSON has no equivalent for — raw data, dates, and
/// `CF$UID` references — and collapsing them early would lose exactly the detail
/// worth seeing.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// An integer. Widened to `i128` so both `i64` and `u64` plist integers fit.
    Integer(i128),
    /// A floating-point number.
    Real(f64),
    /// A string.
    String(String),
    /// Raw bytes.
    Data(Vec<u8>),
    /// A timestamp, already formatted as RFC 3339.
    Date(String),
    /// An `NSKeyedArchiver` object reference.
    Uid(u64),
    /// An ordered list.
    Array(Vec<Value>),
    /// A mapping, sorted by key so output is deterministic.
    Dict(BTreeMap<String, Value>),
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Integer(value) => serialize_integer(*value, serializer),
            Self::Real(value) => {
                // JSON has no NaN or infinity; a string keeps the fact visible.
                if value.is_finite() {
                    serializer.serialize_f64(*value)
                } else {
                    serializer.serialize_str(&value.to_string())
                }
            }
            Self::String(value) => serializer.serialize_str(value),
            Self::Data(bytes) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry(
                    "base64",
                    &base64::engine::general_purpose::STANDARD.encode(bytes),
                )?;
                map.serialize_entry("bytes", &bytes.len())?;
                map.end()
            }
            Self::Date(value) => serializer.serialize_str(value),
            Self::Uid(value) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("uid", value)?;
                map.end()
            }
            Self::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(value)?;
                }
                seq.end()
            }
            Self::Dict(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

/// Serialises an integer, falling back to a string beyond JSON's number range.
fn serialize_integer<S: Serializer>(value: i128, serializer: S) -> Result<S::Ok, S::Error> {
    if let Ok(value) = i64::try_from(value) {
        serializer.serialize_i64(value)
    } else if let Ok(value) = u64::try_from(value) {
        serializer.serialize_u64(value)
    } else {
        serializer.serialize_str(&value.to_string())
    }
}

/// What a byte slice turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// A binary property list.
    BinaryPlist,
    /// An XML property list.
    XmlPlist,
    /// A SQLite database, recognised but not decoded.
    Sqlite,
    /// A PNG image.
    Png,
    /// Valid UTF-8 text.
    Text,
    /// Anything else.
    Binary,
    /// A zero-length file.
    Empty,
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BinaryPlist => "binary plist",
            Self::XmlPlist => "XML plist",
            Self::Sqlite => "SQLite database",
            Self::Png => "PNG image",
            Self::Text => "text",
            Self::Binary => "binary",
            Self::Empty => "empty",
        })
    }
}

/// The decoded content, if there is any.
#[derive(Debug, Clone)]
pub enum Body {
    /// Structured data.
    Value(Value),
    /// Plain text.
    Text(String),
    /// Bytes the caller should hexdump.
    Opaque,
}

/// The result of decoding a byte slice.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// What the content was sniffed as.
    pub format: Format,
    /// The decoded content.
    pub body: Body,
    /// Why the content was not decoded further, when it was not.
    pub note: Option<String>,
}

/// Magic bytes for a binary property list.
const BINARY_PLIST_MAGIC: &[u8] = b"bplist00";
/// Magic bytes for a SQLite database.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
/// Magic bytes for a PNG image.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Decodes a byte slice, as far as it can be understood.
///
/// This never returns an error. Content that cannot be parsed comes back as
/// [`Body::Opaque`] with an explanatory note.
pub fn decode(bytes: &[u8]) -> Decoded {
    if bytes.is_empty() {
        return Decoded {
            format: Format::Empty,
            body: Body::Opaque,
            note: None,
        };
    }

    if bytes.starts_with(BINARY_PLIST_MAGIC) {
        return decode_plist(bytes, Format::BinaryPlist);
    }

    if bytes.starts_with(SQLITE_MAGIC) {
        return Decoded {
            format: Format::Sqlite,
            body: Body::Opaque,
            note: Some("SQLite databases are not decoded; use --raw for the bytes".into()),
        };
    }

    if bytes.starts_with(PNG_MAGIC) {
        let note = match png_dimensions(bytes) {
            Some((width, height)) => format!("{width}x{height} PNG, {} bytes", bytes.len()),
            None => format!("PNG, {} bytes", bytes.len()),
        };
        return Decoded {
            format: Format::Png,
            body: Body::Opaque,
            note: Some(note),
        };
    }

    if looks_like_xml_plist(bytes) {
        return decode_plist(bytes, Format::XmlPlist);
    }

    match std::str::from_utf8(bytes) {
        Ok(text) if is_printable(text) => Decoded {
            format: Format::Text,
            body: Body::Text(text.to_string()),
            note: None,
        },
        _ => Decoded {
            format: Format::Binary,
            body: Body::Opaque,
            note: None,
        },
    }
}

/// Runs the plist decoder, degrading to opaque bytes when it refuses the input.
fn decode_plist(bytes: &[u8], format: Format) -> Decoded {
    match plist::decode(bytes) {
        Ok(value) => Decoded {
            format,
            body: Body::Value(value),
            note: None,
        },
        Err(error) => Decoded {
            format,
            body: Body::Opaque,
            note: Some(format!("could not be parsed as a {format}: {error}")),
        },
    }
}

/// Whether the bytes open an XML property list, ignoring leading whitespace.
fn looks_like_xml_plist(bytes: &[u8]) -> bool {
    let trimmed = bytes
        .strip_prefix(b"\xef\xbb\xbf".as_slice())
        .unwrap_or(bytes);
    let start = trimmed
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(trimmed.len());
    let Some(rest) = trimmed.get(start..) else {
        return false;
    };

    rest.starts_with(b"<?xml")
        || rest.starts_with(b"<!DOCTYPE plist")
        || rest.starts_with(b"<plist")
}

/// Whether text is plausibly meant to be read, rather than binary that happens to
/// decode as UTF-8.
///
/// Control characters other than tab, newline, and carriage return are the tell.
fn is_printable(text: &str) -> bool {
    !text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\n' | '\r'))
}

/// Reads a PNG's pixel dimensions from its IHDR chunk.
///
/// Returns `None` for anything shorter or differently shaped than expected rather
/// than indexing past the end.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let width = bytes.get(16..20)?;
    let height = bytes.get(20..24)?;
    Some((
        u32::from_be_bytes(width.try_into().ok()?),
        u32::from_be_bytes(height.try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_recognises_an_empty_file() {
        let decoded = decode(b"");
        assert_eq!(decoded.format, Format::Empty);
        assert!(matches!(decoded.body, Body::Opaque));
    }

    #[test]
    fn decode_recognises_sqlite_and_points_at_raw() {
        let decoded = decode(b"SQLite format 3\0rest of the header");
        assert_eq!(decoded.format, Format::Sqlite);
        assert!(decoded.note.expect("a note").contains("--raw"));
    }

    #[test]
    fn decode_reads_png_dimensions() {
        let mut png = Vec::from(PNG_MAGIC);
        png.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&240u32.to_be_bytes());

        let decoded = decode(&png);
        assert_eq!(decoded.format, Format::Png);
        assert!(decoded.note.expect("a note").contains("320x240"));
    }

    #[test]
    fn decode_does_not_index_past_a_truncated_png() {
        let decoded = decode(PNG_MAGIC);
        assert_eq!(decoded.format, Format::Png);
        assert!(!decoded.note.expect("a note").contains('x'));
    }

    #[test]
    fn decode_treats_utf8_as_text() {
        let decoded = decode("hello\nworld\n".as_bytes());
        assert_eq!(decoded.format, Format::Text);
        match decoded.body {
            Body::Text(text) => assert_eq!(text, "hello\nworld\n"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn decode_treats_utf8_with_control_bytes_as_binary() {
        // Decodes as UTF-8 but is plainly not text.
        let decoded = decode(b"\x00\x01\x02valid ascii");
        assert_eq!(decoded.format, Format::Binary);
    }

    #[test]
    fn decode_recognises_an_xml_plist_with_leading_whitespace() {
        let xml = b"  \n<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>";
        assert_eq!(decode(xml).format, Format::XmlPlist);
    }

    #[test]
    fn looks_like_xml_plist_ignores_a_byte_order_mark() {
        assert!(looks_like_xml_plist(b"\xef\xbb\xbf<?xml version=\"1.0\"?>"));
    }

    #[test]
    fn looks_like_xml_plist_rejects_other_xml_shaped_input() {
        assert!(!looks_like_xml_plist(b"<html><body>hi</body></html>"));
    }

    #[test]
    fn integers_beyond_json_range_serialise_as_strings() {
        let json = serde_json::to_value(Value::Integer(i128::MAX)).expect("serialises");
        assert!(json.is_string(), "got: {json}");

        let json = serde_json::to_value(Value::Integer(-5)).expect("serialises");
        assert_eq!(json, serde_json::json!(-5));

        let json = serde_json::to_value(Value::Integer(u64::MAX.into())).expect("serialises");
        assert_eq!(json, serde_json::json!(u64::MAX));
    }

    #[test]
    fn data_serialises_as_base64_with_a_byte_count() {
        let json = serde_json::to_value(Value::Data(b"hello".to_vec())).expect("serialises");
        assert_eq!(
            json,
            serde_json::json!({ "base64": "aGVsbG8=", "bytes": 5 })
        );
    }

    #[test]
    fn non_finite_reals_serialise_as_strings() {
        let json = serde_json::to_value(Value::Real(f64::NAN)).expect("serialises");
        assert!(json.is_string(), "got: {json}");
    }
}
