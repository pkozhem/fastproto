"""Scalar wire-format behaviour, exercised through the public API.

Byte-level primitives (varint, zig-zag, fixed widths) are unit-tested in Rust
(``src/wire.rs``); here we verify the Python-facing round-trip, proto3 default
omission, explicit presence, and decoder robustness on the generated classes.
"""

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


def test_defaults_are_omitted() -> None:
    assert AllScalars().to_bytes() == b""  # proto3 defaults are not serialized
    assert AllScalars.from_bytes(b"") == AllScalars()


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


def test_unknown_fields_are_skipped() -> None:
    # Append an unknown field (number 99, varint 1); the decoder must ignore it.
    data = AllScalars(count=5).to_bytes() + b"\x98\x06\x01"
    assert AllScalars.from_bytes(data).count == 5


def test_scalar_annotations_are_transparent() -> None:
    # Scalar.Int64 must behave as plain `int` for the type system / runtime.
    base, meta = get_args(Scalar.Int64)
    assert base is int
    assert repr(meta) == "proto:int64"
    base, _ = get_args(Scalar.String)
    assert base is str
