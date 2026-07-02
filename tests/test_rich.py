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
    assert "address: Address | None = None" in src
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


def test_oneof_enforced() -> None:
    with pytest.raises(ValueError, match="oneof"):
        User(phone="a", telegram="b").to_bytes()
    # a single member is fine
    assert User.from_bytes(User(telegram="t").to_bytes()).telegram == "t"


def test_wire_compatible_with_reference() -> None:
    """Our bytes must be readable by google's protobuf, and vice versa."""
    google = pytest.importorskip("google.protobuf")
    descriptor_pb2 = google.descriptor_pb2
    from google.protobuf import descriptor_pool, message_factory

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
