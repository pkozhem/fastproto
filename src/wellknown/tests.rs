use super::*;

#[test]
fn wire_roundtrip() {
    for (secs, nanos) in [
        (0_i64, 0_i32),
        (1_720_620_000, 500),
        (-1, 999_999_999),   // Timestamp just before the epoch
        (-5, -500_000_000),  // negative Duration: same-sign parts
    ] {
        let mut buf = Vec::new();
        encode_parts(&mut buf, secs, nanos);
        assert_eq!(decode_parts(&buf).unwrap(), (secs, nanos));
    }
}

#[test]
fn zero_parts_encode_to_nothing() {
    let mut buf = Vec::new();
    encode_parts(&mut buf, 0, 0);
    assert!(buf.is_empty());
}

#[test]
fn unknown_fields_inside_are_skipped() {
    let mut buf = Vec::new();
    encode_parts(&mut buf, 7, 0);
    wire::write_tag(&mut buf, 9, WireType::Len);
    wire::write_len_delimited(&mut buf, b"junk");
    assert_eq!(decode_parts(&buf).unwrap(), (7, 0));
}
