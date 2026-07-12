"""Round-trip tests for composite types on the committed generated module.

Imports ``tests.generated.rich_pb`` (checked in, produced by the plugin) like
any ordinary module and exercises repeated / map / enum / nested-message /
oneof through the native codec. One test cross-checks wire compatibility
against google's reference protobuf runtime.
"""

from pathlib import Path
from typing import Any

import pytest

from tests.generated.rich_pb import Address, Role, User

FIXTURES = Path(__file__).parent / "fixtures"


def test_generated_shape() -> None:
    assert Role.ROLE_ADMIN == 1

    # the generated source reads as clean, idiomatic Python
    src = (Path(__file__).parent / "generated" / "rich_pb.py").read_text()
    assert "from fastproto import Message, Scalar, message" in src
    assert "class User(Message):" in src
    assert "tags: list[Scalar.String]" in src
    assert "counters: dict[Scalar.String, Scalar.Int32]" in src
    assert 'address: "Address | None" = None' in src
    assert "role: Role = Role(0)" in src


def test_full_roundtrip() -> None:
    user = User(
        id=42,
        name="P",
        email="p@example.com",
        role=Role.ROLE_ADMIN,
        tags=["vip", "beta"],
        scores=[10, -20, 30],
        address=Address(city="Prague", street="Karlova"),
        past_addresses=[
            Address(city="Dresden", street="Rosmaringasse"),
            Address(city="New York"),
        ],
        counters={"a": 1, "b": 2},
        places={"home": Address(city="Lisbon", street="Central")},
        roles=[Role.ROLE_ADMIN, Role.ROLE_USER],
        phone="+123",
    )
    back = User.from_bytes(user.to_bytes())
    assert back == user
    # enum/message fields decode back to their real types
    assert isinstance(back.role, Role)
    assert isinstance(back.roles[0], Role)
    assert isinstance(back.address, Address)
    assert isinstance(back.places["home"], Address)


def test_defaults_and_presence() -> None:
    empty = User()
    assert empty.to_bytes() == b""  # all proto3 defaults omitted
    assert User.from_bytes(b"") == empty
    assert empty.email is None  # optional unset
    assert empty.address is None  # message unset

    # optional set to empty string survives (explicit presence)
    assert User.from_bytes(User(email="").to_bytes()).email == ""


def test_open_enum_unknown_value_survives() -> None:
    # proto3 enums are open: value 99 is not a Role member but is valid on the
    # wire. Field `role` = 4 (varint): tag = (4<<3)|0 = 0x20, value 99 = 0x63.
    data = b"\x20\x63"
    user = User.from_bytes(data)
    assert user.role == 99
    assert not isinstance(user.role, Role)  # raw int fallback, like google

    # The unknown value survives a re-encode round-trip.
    assert User.from_bytes(user.to_bytes()).role == 99


def test_open_enum_in_repeated() -> None:
    # `roles` = 11, packed: tag = (11<<3)|2 = 0x5a, len 3, values 1, 99, 2.
    data = b"\x5a\x03\x01\x63\x02"
    user = User.from_bytes(data)
    assert user.roles == [Role.ROLE_ADMIN, 99, Role.ROLE_USER]
    assert isinstance(user.roles[0], Role)
    assert not isinstance(user.roles[1], Role)
    assert User.from_bytes(user.to_bytes()).roles == [1, 99, 2]


def test_wrong_wire_type_on_known_field_is_preserved_not_misdecoded() -> None:
    # `role` (field 4, enum) expects a varint but arrives length-delimited.
    # tag = (4<<3)|2 = 0x22, len 1, byte 0x63. The decoder must NOT read it as
    # an enum (that would mis-decode); it skips it and preserves it as unknown.
    data = b"\x22\x01\x63"
    user = User.from_bytes(data)
    assert user.role == Role.ROLE_UNSPECIFIED  # untouched, not mis-decoded
    assert user.to_bytes() == data  # preserved verbatim as an unknown field


def test_oneof_enforced() -> None:
    with pytest.raises(ValueError, match="oneof"):
        User(phone="a", telegram="b").to_bytes()
    # a single member is fine
    assert User.from_bytes(User(telegram="t").to_bytes()).telegram == "t"


def test_which_oneof() -> None:
    # nothing set -> no member
    assert User(name="x").which_oneof("contact") is None
    # a scalar member
    assert User(phone="p").which_oneof("contact") == "phone"
    # a message member, surviving a round-trip
    decoded = User.from_bytes(User(postal=Address(city="C")).to_bytes())
    assert decoded.which_oneof("contact") == "postal"
    assert decoded.postal == Address(city="C")


def test_which_oneof_unknown_group() -> None:
    with pytest.raises(ValueError, match="no oneof group 'nope'"):
        User(name="x").which_oneof("nope")


def test_oneof_last_member_on_wire_wins() -> None:
    # Two members of the same oneof on the wire is valid protobuf ("last wins").
    # The decoder must keep only the last, so the result re-encodes cleanly.
    # phone = field 12 ("a"), telegram = field 13 ("b").
    data = bytes([0x62, 0x01, ord("a"), 0x6A, 0x01, ord("b")])
    u = User.from_bytes(data)
    assert u.phone is None
    assert u.telegram == "b"
    assert u.to_bytes() == bytes([0x6A, 0x01, ord("b")])


def test_repeated_wrong_wire_type_is_preserved() -> None:
    # `tags` (field 5) is a repeated string; sending it as a varint is a wire
    # mismatch. Like a singular mismatch, the bytes must be preserved as unknown
    # (not silently dropped) so a decode -> encode round-trip is lossless.
    data = bytes([0x28, 0x01])  # field 5, wire type Varint, value 1
    u = User.from_bytes(data)
    assert u.tags == []
    assert u.to_bytes() == data


def test_field_number_zero_is_rejected() -> None:
    with pytest.raises(ValueError, match="malformed"):
        User.from_bytes(b"\x00")


def test_wire_compatible_with_reference() -> None:
    """Our bytes must be readable by google's protobuf, and vice versa."""
    pytest.importorskip("google.protobuf")
    from google.protobuf import descriptor_pb2, descriptor_pool, message_factory

    fileset = descriptor_pb2.FileDescriptorSet.FromString(
        (FIXTURES / "rich.fds").read_bytes(),
    )
    pool = descriptor_pool.DescriptorPool()
    for file in fileset.file:
        pool.Add(file)
    ref_cls = message_factory.GetMessageClass(pool.FindMessageTypeByName("rich.User"))

    user = User(
        id=7,
        name="x",
        role=Role.ROLE_USER,
        tags=["a", "b"],
        scores=[1, 2, 3],
        address=Address(city="C", street="S"),
        counters={"k": 5},
        places={"h": Address(city="HC")},
        roles=[Role.ROLE_ADMIN],
        telegram="tg",
    )

    # our bytes -> reference decodes them faithfully (reflective message is
    # dynamically typed, so `ref` is deliberately Any)
    ref: Any = ref_cls()
    ref.ParseFromString(user.to_bytes())
    assert ref.id == 7
    assert list(ref.tags) == ["a", "b"]
    assert list(ref.scores) == [1, 2, 3]
    assert ref.address.city == "C"
    assert dict(ref.counters) == {"k": 5}
    assert ref.places["h"].city == "HC"
    assert ref.telegram == "tg"
    assert ref.role == Role.ROLE_USER

    # reference bytes -> our decoder decodes them faithfully
    assert User.from_bytes(ref.SerializeToString()) == user

    # unknown fields we preserved on a decode -> encode round-trip must still
    # parse cleanly in the reference implementation
    unknown_chunk = b"\x98\x06\x2a"  # field 99, varint 42
    roundtripped = User.from_bytes(user.to_bytes() + unknown_chunk).to_bytes()
    ref.ParseFromString(roundtripped)
    assert ref.id == 7  # known fields intact alongside the unknown one
