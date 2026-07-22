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

#[test]
fn field_index_dense_and_sparse() {
    let fields = vec![
        FieldDescriptor::scalar(1, "a", ScalarType::Int32, Label::Single),
        FieldDescriptor::scalar(7, "b", ScalarType::Int32, Label::Single),
        FieldDescriptor::scalar(3, "c", ScalarType::Int32, Label::Single),
    ];
    let dense = FieldIndex::build(&fields);
    assert!(matches!(dense, FieldIndex::Dense(_)));
    assert_eq!(dense.get(1), Some(0));
    assert_eq!(dense.get(7), Some(1));
    assert_eq!(dense.get(3), Some(2));
    assert_eq!(dense.get(2), None);
    assert_eq!(dense.get(1000), None);

    let sparse_fields = vec![
        FieldDescriptor::scalar(1, "a", ScalarType::Int32, Label::Single),
        FieldDescriptor::scalar(500_000, "b", ScalarType::Int32, Label::Single),
    ];
    let sparse = FieldIndex::build(&sparse_fields);
    assert!(matches!(sparse, FieldIndex::Sparse(_)));
    assert_eq!(sparse.get(1), Some(0));
    assert_eq!(sparse.get(500_000), Some(1));
    assert_eq!(sparse.get(2), None);
}
