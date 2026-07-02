use super::*;

#[test]
fn scalar_wire_types() {
    assert_eq!(ScalarType::Int64.wire_type(), WireType::Varint);
    assert_eq!(ScalarType::Double.wire_type(), WireType::I64);
    assert_eq!(ScalarType::Float.wire_type(), WireType::I32);
    assert_eq!(ScalarType::String.wire_type(), WireType::Len);
}

#[test]
fn packable_excludes_len_delimited() {
    assert!(ScalarType::Int32.is_packable());
    assert!(!ScalarType::String.is_packable());
    assert!(!ScalarType::Bytes.is_packable());
}

#[test]
fn field_lookup_by_number() {
    let msg = MessageDescriptor {
        name: "Metrics".to_string(),
        fields: vec![
            FieldDescriptor::scalar(1, "count", ScalarType::Int32, Label::Single),
            FieldDescriptor::scalar(7, "label", ScalarType::String, Label::Single),
        ],
        oneofs: vec![],
    };
    assert_eq!(msg.field_by_number(7).unwrap().name, "label");
    assert!(msg.field_by_number(2).is_none());
}
