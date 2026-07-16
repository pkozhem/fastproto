"""Scalar wire-format behaviour, exercised through the public API.

Byte-level primitives (varint, zig-zag, fixed widths) are unit-tested in Rust
(``src/wire.rs``); here we verify the Python-facing round-trip, proto3 default
omission, explicit presence, and decoder robustness on the generated classes.
"""

import math
from typing import get_args

from fastproto import Scalar
from tests.generated.scalars_pb import AllScalars, Presence


def test_all_scalars_roundtrip() -> None:
    msg = AllScalars(
        ratio=3.14159,
        temperature=1.5,  # exactly representable in float32
        count=-42,
        total=9_000_000_000,
        ucount=4_000_000_000,
        utotal=18_000_000_000_000_000_000,
        delta=-7,
        delta64=-9_000_000_000,
        f32=0xDEADBEEF,
        f64=0x0123456789ABCDEF,
        sf32=-123456,
        sf64=-9_000_000_000,
        enabled=True,
        label="héllo",
        payload=b"\x00\x01\x02\xff",
    )
    assert AllScalars.from_bytes(msg.to_bytes()) == msg


def test_bytes_field_accepts_bytearray() -> None:
    # The encoder borrows a bytes/bytearray buffer directly (no per-element
    # boxing); bytearray must be accepted and encode identically to bytes.
    raw = b"\x00\x01\x02\xff\xfe"
    assert (
        AllScalars(payload=bytearray(raw)).to_bytes()
        == AllScalars(payload=raw).to_bytes()
    )
    assert (
        AllScalars.from_bytes(AllScalars(payload=bytearray(raw)).to_bytes()).payload
        == raw
    )


def test_large_bytes_roundtrip() -> None:
    # A large payload exercises the bulk-copy encode path (a regression guard
    # against the O(n)-allocations bytes encoder).
    blob = bytes(range(256)) * 4096  # 1 MiB
    assert AllScalars.from_bytes(AllScalars(payload=blob).to_bytes()).payload == blob


def test_defaults_are_omitted() -> None:
    assert AllScalars().to_bytes() == b""  # proto3 defaults are not serialized
    assert AllScalars.from_bytes(b"") == AllScalars()


def test_negative_zero_float_is_preserved() -> None:
    # -0.0 == 0.0, but it is not the proto default: google keeps its sign, so
    # the field must be serialized and the sign must survive the round-trip.
    data = AllScalars(ratio=-0.0).to_bytes()
    assert data != b""
    assert math.copysign(1.0, AllScalars.from_bytes(data).ratio) == -1.0
    # +0.0 is the default and stays omitted.
    assert AllScalars(ratio=0.0).to_bytes() == b""


def test_known_wire_bytes() -> None:
    # count = 1 (int32, field 3): tag = (3<<3)|0 = 0x18, value varint 0x01
    assert AllScalars(count=1).to_bytes() == b"\x18\x01"
    # label = "hi" (string, field 14): tag = (14<<3)|2 = 0x72, len 2, "hi"
    assert AllScalars(label="hi").to_bytes() == b"\x72\x02hi"
    # enabled = True (bool, field 13): tag = (13<<3)|0 = 0x68, value 0x01
    assert AllScalars(enabled=True).to_bytes() == b"\x68\x01"


def test_negative_int32_is_ten_byte_varint() -> None:
    data = AllScalars(count=-1).to_bytes()
    assert data[0] == 0x18  # tag for field 3
    assert len(data) == 1 + 10  # int32 -1 sign-extends to a 10-byte varint


def test_optional_presence() -> None:
    # Optional set to empty string is still serialized (explicit presence).
    assert Presence(nickname="").to_bytes() == b"\x12\x00"  # field 2, len 0
    assert Presence.from_bytes(Presence(nickname="").to_bytes()).nickname == ""

    # Optional left as None is omitted.
    assert Presence(name="x").to_bytes() == b"\x0a\x01x"  # only field 1
    assert Presence.from_bytes(Presence(name="x").to_bytes()).nickname is None


# Unknown fields of every wire type: varint (field 99), length-delimited
# (field 100), fixed64 (field 101), fixed32 (field 102).
UNKNOWN_CHUNK = (
    b"\x98\x06\x2a"  # 99 << 3 | 0, varint 42
    b"\xa2\x06\x03abc"  # 100 << 3 | 2, len 3, b"abc"
    b"\xa9\x06\x01\x02\x03\x04\x05\x06\x07\x08"  # 101 << 3 | 1, fixed64
    b"\xb5\x06\x01\x02\x03\x04"  # 102 << 3 | 5, fixed32
)


def test_unknown_fields_are_preserved() -> None:
    # Fields the schema doesn't know must survive decode -> encode (protobuf
    # forward compatibility), and known fields must still decode correctly.
    data = AllScalars(count=5, label="hi").to_bytes() + UNKNOWN_CHUNK
    msg = AllScalars.from_bytes(data)
    assert msg.count == 5
    assert msg.label == "hi"

    reencoded = msg.to_bytes()
    assert UNKNOWN_CHUNK in reencoded
    # ...and the re-encoded bytes still decode cleanly.
    again = AllScalars.from_bytes(reencoded)
    assert again == msg
    assert UNKNOWN_CHUNK in again.to_bytes()


def test_unknown_fields_do_not_leak_into_fresh_instances() -> None:
    # A hand-constructed message has no unknown bytes to re-emit.
    assert AllScalars(count=5).to_bytes() == b"\x18\x05"


def test_scalar_annotations_are_transparent() -> None:
    # Scalar.Int64 must behave as plain `int` for the type system / runtime.
    base, meta = get_args(Scalar.Int64)
    assert base is int
    assert repr(meta) == "proto:int64"
    base, _ = get_args(Scalar.String)
    assert base is str
