//! Property-list decoding.
//!
//! Wraps the `plist` crate behind one narrow function so the implementation stays
//! swappable — a hand-written parser could replace it without touching a caller.
//!
//! # Edge cases
//!
//! The input is a binary format written by an app that may be buggy, so the
//! conversion is bounded: nesting deeper than [`MAX_DEPTH`] is replaced with a
//! marker string rather than recursing until the stack runs out. The `plist`
//! crate itself returns errors instead of panicking on malformed bytes, which is
//! the reason it is used here rather than hand-rolled.

use std::collections::BTreeMap;

use super::Value;

/// How deep the converter will follow nested containers.
///
/// Real preference files nest a handful of levels; anything approaching this is a
/// bug or an attack, and either way a marker is a better outcome than a crash.
const MAX_DEPTH: usize = 128;

/// Text substituted for structure below [`MAX_DEPTH`].
const TOO_DEEP: &str = "<nesting too deep>";

/// Decodes a property list, binary or XML.
///
/// # Errors
///
/// Returns the `plist` crate's error for input that is not a well-formed property
/// list, including truncated files.
pub fn decode(bytes: &[u8]) -> Result<Value, ::plist::Error> {
    let value = ::plist::Value::from_reader(std::io::Cursor::new(bytes))?;
    Ok(convert(&value, 0))
}

/// Converts a `plist` value into our own, bounded by depth.
fn convert(value: &::plist::Value, depth: usize) -> Value {
    if depth >= MAX_DEPTH {
        return Value::String(TOO_DEEP.to_string());
    }

    match value {
        ::plist::Value::Boolean(value) => Value::Bool(*value),
        ::plist::Value::String(value) => Value::String(value.clone()),
        ::plist::Value::Real(value) => Value::Real(*value),
        ::plist::Value::Data(bytes) => Value::Data(bytes.clone()),
        ::plist::Value::Uid(uid) => Value::Uid(uid.get()),
        // Plist integers may be signed or unsigned 64-bit; `i128` holds either.
        ::plist::Value::Integer(value) => value
            .as_signed()
            .map(i128::from)
            .or_else(|| value.as_unsigned().map(i128::from))
            .map_or_else(|| Value::String(value.to_string()), Value::Integer),
        ::plist::Value::Date(date) => Value::Date(format_date(*date)),
        ::plist::Value::Array(values) => {
            Value::Array(values.iter().map(|item| convert(item, depth + 1)).collect())
        }
        ::plist::Value::Dictionary(entries) => Value::Dict(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), convert(item, depth + 1)))
                .collect::<BTreeMap<_, _>>(),
        ),
        // `plist::Value` is non-exhaustive; anything new degrades to a label
        // rather than failing the whole decode.
        other => Value::String(format!("<unsupported plist value: {other:?}>")),
    }
}

/// Formats a plist date as RFC 3339 in local time.
fn format_date(date: ::plist::Date) -> String {
    let time: std::time::SystemTime = date.into();
    chrono::DateTime::<chrono::Local>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An XML property list exercising every type worth converting.
    const SAMPLE: &str = include_str!("../../tests/fixtures/sample.plist.xml");

    /// Re-encodes the XML fixture as a binary plist, which is the form found in
    /// containers. Keeping the fixture as XML in the repo keeps it reviewable.
    fn sample_binary() -> Vec<u8> {
        let value =
            ::plist::Value::from_reader_xml(std::io::Cursor::new(SAMPLE)).expect("fixture parses");
        let mut out = Vec::new();
        ::plist::to_writer_binary(&mut out, &value).expect("re-encodes");
        out
    }

    fn dict(value: &Value) -> &BTreeMap<String, Value> {
        match value {
            Value::Dict(entries) => entries,
            other => panic!("expected a dict, got {other:?}"),
        }
    }

    #[test]
    fn decode_reads_the_xml_fixture() {
        let value = decode(SAMPLE.as_bytes()).expect("decodes");
        let entries = dict(&value);
        assert_eq!(entries.get("aString"), Some(&Value::String("hello".into())));
        assert_eq!(entries.get("aBool"), Some(&Value::Bool(true)));
        assert_eq!(entries.get("anInteger"), Some(&Value::Integer(42)));
    }

    #[test]
    fn decode_reads_the_same_content_from_binary() {
        let from_xml = decode(SAMPLE.as_bytes()).expect("decodes");
        let from_binary = decode(&sample_binary()).expect("decodes");
        assert_eq!(from_xml, from_binary);
    }

    #[test]
    fn decode_preserves_data_dates_and_nesting() {
        let value = decode(&sample_binary()).expect("decodes");
        let entries = dict(&value);

        assert_eq!(
            entries.get("someData"),
            Some(&Value::Data(b"hello".to_vec()))
        );
        assert!(matches!(entries.get("aDate"), Some(Value::Date(_))));

        let nested = dict(entries.get("nested").expect("nested key"));
        assert_eq!(nested.get("inner"), Some(&Value::String("value".into())));

        match entries.get("anArray") {
            Some(Value::Array(items)) => assert_eq!(items.len(), 3),
            other => panic!("expected an array, got {other:?}"),
        }
    }

    #[test]
    fn dictionary_keys_are_sorted() {
        let value = decode(&sample_binary()).expect("decodes");
        let keys: Vec<&String> = dict(&value).keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "keys come back in a deterministic order");
    }

    #[test]
    fn decode_rejects_a_truncated_binary_plist_without_panicking() {
        let binary = sample_binary();
        // The trailer lives in the last 32 bytes, so this removes it entirely.
        assert!(decode(&binary[..20]).is_err());
    }

    /// The core promise of this module: malformed input errors, never panics.
    #[test]
    fn decode_never_panics_on_truncation_at_any_offset() {
        let binary = sample_binary();
        for length in 0..binary.len() {
            let _ = decode(&binary[..length]);
        }
    }

    #[test]
    fn decode_never_panics_on_a_corrupted_byte_at_any_offset() {
        let binary = sample_binary();
        for index in 0..binary.len() {
            let mut corrupted = binary.clone();
            // Flipping the high bit tends to produce absurd lengths and offsets.
            corrupted[index] ^= 0x80;
            let _ = decode(&corrupted);
        }
    }

    #[test]
    fn decode_never_panics_on_arbitrary_bytes() {
        for seed in 0u16..=u16::MAX {
            let bytes: Vec<u8> = seed
                .to_le_bytes()
                .iter()
                .cycle()
                .take(64)
                .copied()
                .collect();
            let _ = decode(&bytes);
        }
    }

    #[test]
    fn convert_caps_runaway_nesting() {
        // Build a plist nested well beyond the cap.
        let mut value = ::plist::Value::String("bottom".into());
        for _ in 0..(MAX_DEPTH + 10) {
            value = ::plist::Value::Array(vec![value]);
        }

        let converted = convert(&value, 0);

        // Walk down and confirm it terminates in the marker rather than blowing up.
        let mut cursor = &converted;
        let mut depth = 0;
        while let Value::Array(items) = cursor {
            cursor = items.first().expect("one item");
            depth += 1;
        }
        assert_eq!(cursor, &Value::String(TOO_DEEP.into()));
        assert_eq!(depth, MAX_DEPTH);
    }
}
