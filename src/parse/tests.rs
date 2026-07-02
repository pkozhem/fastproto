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
    assert_eq!(desc.fields[0].type_name.as_deref(), Some("Role"));
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
    assert_eq!(desc.fields[0].type_name.as_deref(), Some("Address"));
    assert_eq!(desc.fields[0].label, Label::Optional);
}
