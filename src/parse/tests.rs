use super::*;
use crate::wire::{self, WireType};

fn field_bytes(
    name: &str,
    number: u32,
    type_code: i64,
    label: Option<i64>,
    proto3_optional: bool,
) -> Vec<u8> {
    let mut buf = Vec::new();
    wire::write_tag(&mut buf, 1, WireType::Len);
    wire::write_len_delimited(&mut buf, name.as_bytes());
    wire::write_tag(&mut buf, 3, WireType::Varint);
    wire::write_varint(&mut buf, number as u64);
    if let Some(l) = label {
        wire::write_tag(&mut buf, 4, WireType::Varint);
        wire::write_varint(&mut buf, l as u64);
    }
    wire::write_tag(&mut buf, 5, WireType::Varint);
    wire::write_varint(&mut buf, type_code as u64);
    if proto3_optional {
        wire::write_tag(&mut buf, 17, WireType::Varint);
        wire::write_varint(&mut buf, 1);
    }
    buf
}

fn message_bytes(name: &str, fields: &[Vec<u8>]) -> Vec<u8> {
    let mut buf = Vec::new();
    wire::write_tag(&mut buf, 1, WireType::Len);
    wire::write_len_delimited(&mut buf, name.as_bytes());
    for f in fields {
        wire::write_tag(&mut buf, 2, WireType::Len);
        wire::write_len_delimited(&mut buf, f);
    }
    buf
}

/// A message field carrying an explicit `type_name` (tag 6).
fn message_field_bytes(name: &str, number: u32, type_name: &str, label: i64) -> Vec<u8> {
    let mut buf = field_bytes(name, number, 11, Some(label), false); // type 11 = message
    wire::write_tag(&mut buf, 6, WireType::Len);
    wire::write_len_delimited(&mut buf, type_name.as_bytes());
    buf
}

/// A synthetic `map<string, string>` entry `DescriptorProto` (options.map_entry).
fn map_entry_type_bytes(name: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    wire::write_tag(&mut buf, 1, WireType::Len);
    wire::write_len_delimited(&mut buf, name.as_bytes());
    for f in [field_bytes("key", 1, 9, None, false), field_bytes("value", 2, 9, None, false)] {
        wire::write_tag(&mut buf, 2, WireType::Len);
        wire::write_len_delimited(&mut buf, &f);
    }
    let mut opts = Vec::new();
    wire::write_tag(&mut opts, 7, WireType::Varint); // MessageOptions.map_entry
    wire::write_varint(&mut opts, 1);
    wire::write_tag(&mut buf, 7, WireType::Len);
    wire::write_len_delimited(&mut buf, &opts);
    buf
}

#[test]
fn parses_scalar_message() {
    let bytes = message_bytes(
        "Metrics",
        &[
            field_bytes("count", 3, 5, None, false),
            field_bytes("label", 14, 9, None, false),
        ],
    );
    let desc = parse_message(&bytes).unwrap();
    assert_eq!(desc.name, "Metrics");
    assert_eq!(desc.fields[0].kind, FieldKind::Scalar(ScalarType::Int32));
    assert_eq!(desc.fields[0].label, Label::Single);
    assert_eq!(desc.fields[1].kind, FieldKind::Scalar(ScalarType::String));
}

#[test]
fn proto3_optional_becomes_optional_label() {
    let bytes = message_bytes("P", &[field_bytes("nickname", 2, 9, Some(1), true)]);
    let desc = parse_message(&bytes).unwrap();
    assert_eq!(desc.fields[0].label, Label::Optional);
    assert!(desc.fields[0].oneof_index.is_none());
}

#[test]
fn repeated_scalar() {
    let bytes = message_bytes("R", &[field_bytes("tags", 1, 9, Some(LABEL_REPEATED), false)]);
    let desc = parse_message(&bytes).unwrap();
    assert_eq!(desc.fields[0].label, Label::Repeated);
    assert_eq!(desc.fields[0].kind, FieldKind::Scalar(ScalarType::String));
}

#[test]
fn enum_field() {
    // type 14 = enum, type_name ".demo.Role"
    let mut fb = field_bytes("role", 1, 14, None, false);
    wire::write_tag(&mut fb, 6, WireType::Len);
    wire::write_len_delimited(&mut fb, b".demo.Role");
    let bytes = message_bytes("M", &[fb]);
    let desc = parse_message(&bytes).unwrap();
    assert_eq!(desc.fields[0].kind, FieldKind::Enum);
    assert_eq!(desc.fields[0].type_name.as_deref(), Some("demo.Role"));
    assert_eq!(desc.fields[0].label, Label::Single);
}

#[test]
fn message_field_is_optional() {
    let mut fb = field_bytes("addr", 6, 11, None, false);
    wire::write_tag(&mut fb, 6, WireType::Len);
    wire::write_len_delimited(&mut fb, b".demo.Address");
    let bytes = message_bytes("M", &[fb]);
    let desc = parse_message(&bytes).unwrap();
    assert_eq!(desc.fields[0].kind, FieldKind::Message);
    assert_eq!(desc.fields[0].type_name.as_deref(), Some("demo.Address"));
    assert_eq!(desc.fields[0].label, Label::Optional);
}

#[test]
fn out_of_range_oneof_index_is_dropped() {
    // A field claiming oneof_index 3 when the message declares no oneof groups
    // must not keep the index (else encode / oneofs() would panic indexing).
    let mut fb = field_bytes("x", 1, 9, Some(1), false);
    wire::write_tag(&mut fb, 9, WireType::Varint);
    wire::write_varint(&mut fb, 3);
    let desc = parse_message(&message_bytes("M", &[fb])).unwrap();
    assert!(desc.oneofs.is_empty());
    assert!(desc.fields[0].oneof_index.is_none());
}

#[test]
fn sibling_sharing_map_entry_short_name_is_message() {
    // `data` is a real map (nested M.DataEntry); `others` references a real
    // sibling `.pkg.DataEntry` that merely shares the entry's short name and
    // must stay a message field, not be swallowed as a map.
    let data = message_field_bytes("data", 1, ".pkg.M.DataEntry", LABEL_REPEATED);
    let others = message_field_bytes("others", 2, ".pkg.DataEntry", LABEL_REPEATED);
    let mut buf = Vec::new();
    wire::write_tag(&mut buf, 1, WireType::Len);
    wire::write_len_delimited(&mut buf, b"M");
    for f in [&data, &others] {
        wire::write_tag(&mut buf, 2, WireType::Len);
        wire::write_len_delimited(&mut buf, f);
    }
    wire::write_tag(&mut buf, 3, WireType::Len);
    wire::write_len_delimited(&mut buf, &map_entry_type_bytes("DataEntry"));

    let desc = parse_message(&buf).unwrap();
    assert!(matches!(desc.fields[0].kind, FieldKind::Map { .. }));
    assert_eq!(desc.fields[1].kind, FieldKind::Message);
    assert_eq!(desc.fields[1].type_name.as_deref(), Some("pkg.DataEntry"));
}

mod properties {
    use proptest::prelude::*;

    use super::super::parse_message;

    proptest! {
        /// The descriptor parser must never panic — arbitrary bytes are either
        /// a valid `DescriptorProto` (Ok) or a clean error (Err).
        #[test]
        fn parse_message_never_panics(data in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_message(&data);
        }
    }
}
