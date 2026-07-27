use super::*;

fn roundtrip_varint(value: u64) {
    let mut buf = Vec::new();
    write_varint(&mut buf, value);
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_varint().unwrap(), value);
    assert!(reader.is_empty());
}

#[test]
fn varint_roundtrip_edges() {
    for value in [0u64, 1, 127, 128, 300, 16_383, 16_384, u64::MAX] {
        roundtrip_varint(value);
    }
}

#[test]
fn varint_known_encodings() {
    let mut buf = Vec::new();
    write_varint(&mut buf, 300);
    assert_eq!(buf, vec![0xac, 0x02]);

    buf.clear();
    write_varint(&mut buf, 1);
    assert_eq!(buf, vec![0x01]);
}

#[test]
fn negative_int_is_ten_byte_varint() {
    // Protobuf sign-extends negative int32/int64 to 64 bits.
    let mut buf = Vec::new();
    write_varint(&mut buf, (-1i64) as u64);
    assert_eq!(buf.len(), 10);
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_varint().unwrap() as i64, -1);
}

#[test]
fn varint_overflow_is_error() {
    let buf = [0xffu8; 11];
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_varint(), Err(WireError::VarintOverflow));
}

#[test]
fn varint_truncated_is_eof() {
    let buf = [0x80u8]; // continuation bit set but nothing follows
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_varint(), Err(WireError::UnexpectedEof));
}

#[test]
fn tag_roundtrip() {
    let mut buf = Vec::new();
    write_tag(&mut buf, 5, WireType::Len);
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_tag().unwrap(), (5, WireType::Len));
}

#[test]
fn rejects_zero_field_number() {
    // key 0 = field 0, wire type Varint — never a legal protobuf tag.
    let mut reader = Reader::new(&[0x00]);
    assert_eq!(reader.read_tag(), Err(WireError::InvalidFieldNumber(0)));
}

#[test]
fn rejects_oversized_field_number() {
    // One past the 29-bit maximum: must not silently truncate onto a real tag.
    let mut buf = Vec::new();
    write_varint(&mut buf, (MAX_FIELD_NUMBER + 1) << 3);
    let mut reader = Reader::new(&buf);
    assert_eq!(
        reader.read_tag(),
        Err(WireError::InvalidFieldNumber(MAX_FIELD_NUMBER + 1))
    );
}

#[test]
fn fixed_roundtrip() {
    let mut buf = Vec::new();
    write_fixed32(&mut buf, 0xdead_beef);
    write_fixed64(&mut buf, 0x0123_4567_89ab_cdef);
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_fixed32().unwrap(), 0xdead_beef);
    assert_eq!(reader.read_fixed64().unwrap(), 0x0123_4567_89ab_cdef);
}

#[test]
fn len_delimited_roundtrip() {
    let mut buf = Vec::new();
    write_len_delimited(&mut buf, b"hello");
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_len_delimited().unwrap(), b"hello");
}

#[test]
fn len_delimited_overrun_is_error() {
    let mut buf = Vec::new();
    write_varint(&mut buf, 10); // claims 10 bytes
    buf.extend_from_slice(b"abc"); // only 3 present
    let mut reader = Reader::new(&buf);
    assert_eq!(reader.read_len_delimited(), Err(WireError::InvalidLength));
}

#[test]
fn zigzag32() {
    for value in [0i32, -1, 1, -2, 2, i32::MAX, i32::MIN] {
        assert_eq!(zigzag_decode32(zigzag_encode32(value)), value);
    }
    assert_eq!(zigzag_encode32(-1), 1);
    assert_eq!(zigzag_encode32(1), 2);
}

#[test]
fn zigzag64() {
    for value in [0i64, -1, 1, -2, 2, i64::MAX, i64::MIN] {
        assert_eq!(zigzag_decode64(zigzag_encode64(value)), value);
    }
    assert_eq!(zigzag_encode64(-1), 1);
}

#[test]
fn skip_unknown_fields() {
    let mut buf = Vec::new();
    write_tag(&mut buf, 1, WireType::Varint);
    write_varint(&mut buf, 42);
    write_tag(&mut buf, 2, WireType::Len);
    write_len_delimited(&mut buf, b"skip me");
    write_tag(&mut buf, 3, WireType::I32);
    write_fixed32(&mut buf, 7);

    let mut reader = Reader::new(&buf);
    while !reader.is_empty() {
        let (_field, wire) = reader.read_tag().unwrap();
        reader.skip(wire).unwrap();
    }
    assert!(reader.is_empty());
}

#[test]
fn raw_since_captures_skipped_fields() {
    let mut buf = Vec::new();
    write_tag(&mut buf, 1, WireType::Varint);
    write_varint(&mut buf, 42);
    write_tag(&mut buf, 2, WireType::Len);
    write_len_delimited(&mut buf, b"payload");

    let mut reader = Reader::new(&buf);
    // Consume field 1, then capture the raw bytes of field 2.
    reader.read_tag().unwrap();
    reader.skip(WireType::Varint).unwrap();
    let start = reader.pos();
    let (_field, wire) = reader.read_tag().unwrap();
    reader.skip(wire).unwrap();
    assert_eq!(reader.raw_since(start), &buf[start..]);
    assert!(reader.raw_since(start).ends_with(b"payload"));
}

mod properties {
    use proptest::prelude::*;

    use super::super::*;

    proptest! {
        /// Reading any primitive off arbitrary bytes must never panic.
        #[test]
        fn reader_never_panics(data in proptest::collection::vec(any::<u8>(), 0..256)) {
            let mut reader = Reader::new(&data);
            while !reader.is_empty() {
                let Ok((_, wire)) = reader.read_tag() else { break };
                if reader.skip(wire).is_err() {
                    break;
                }
            }
        }

        /// Varint and zigzag encodings round-trip for every value.
        #[test]
        fn varint_roundtrips(value in any::<u64>()) {
            let mut buf = Vec::new();
            write_varint(&mut buf, value);
            prop_assert_eq!(Reader::new(&buf).read_varint().unwrap(), value);
        }

        #[test]
        fn zigzag_roundtrips(value in any::<i64>()) {
            prop_assert_eq!(zigzag_decode64(zigzag_encode64(value)), value);
        }
    }
}

#[test]
fn varint_len_matches_write_varint() {
    for value in [
        0u64,
        1,
        127,
        128,
        16_383,
        16_384,
        300,
        u32::MAX as u64,
        u64::MAX,
    ] {
        let mut buf = Vec::new();
        write_varint(&mut buf, value);
        assert_eq!(varint_len(value), buf.len(), "value {value}");
    }
}

#[test]
fn len_prefix_in_place_matches_write_len_delimited() {
    // Cover the one-byte fast path, both sides of the 128 boundary, and a
    // payload long enough for a three-byte length varint.
    for size in [0usize, 1, 127, 128, 300, 16_383, 16_384, 70_000] {
        let payload: Vec<u8> = (0..size).map(|i| i as u8).collect();

        let mut expected = Vec::new();
        write_len_delimited(&mut expected, &payload);

        let mut got = vec![0xAA]; // leading byte to catch off-by-one patching
        let pos = begin_len_prefix(&mut got);
        got.extend_from_slice(&payload);
        finish_len_prefix(&mut got, pos);
        assert_eq!(got[0], 0xAA);
        assert_eq!(&got[1..], &expected[..], "size {size}");
    }
}
