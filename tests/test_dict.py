"""Native ``to_dict()`` / ``from_dict()`` round-trips, through the public API.

The dict form is for in-process use (native values, not JSON): nested messages
become dicts, ``repeated``/``map`` become list/dict, and ``IntEnum`` /
``datetime`` / ``timedelta`` / ``bytes`` are kept as-is. Every field is present;
unset optional/message/oneof fields are ``None``. These tests assert that shape
and the ``type(x).from_dict(x.to_dict()) == x`` invariant across every field
kind on the committed generated modules.
"""

from datetime import UTC, datetime, timedelta
from typing import cast

import pytest

from fastproto import Message
from fastproto.wellknown import Any, Int32Value, ListValue, NullValue, Struct, Value
from tests.generated.event_pb import Event
from tests.generated.nested_pb import Outer, Sibling
from tests.generated.rich_pb import Address, Role, User
from tests.generated.wkt_pb import Payload


def _full_user() -> User:
    return User(
        id=42,
        name="Ada",
        email="ada@example.com",
        role=Role.ROLE_ADMIN,
        tags=["vip", "beta"],
        scores=[1, 2, 500],
        address=Address(city="London", street="Baker St"),
        past_addresses=[Address(city="C1"), Address(city="C2")],
        counters={"a": 1, "b": 2},
        places={"home": Address(city="London")},
        roles=[Role.ROLE_USER, Role.ROLE_ADMIN],
        phone="+44",
    )


def test_scalar_and_optional_roundtrip() -> None:
    user = _full_user()
    d = user.to_dict()
    # nested messages, maps, and repeated unfold into plain dicts/lists
    assert d["address"] == {"city": "London", "street": "Baker St"}
    assert d["counters"] == {"a": 1, "b": 2}
    assert d["places"] == {"home": {"city": "London", "street": ""}}
    assert d["tags"] == ["vip", "beta"]
    assert User.from_dict(d) == user


def test_all_keys_present_with_none() -> None:
    d = User().to_dict()
    # the key set is the full field set, independent of the data
    assert set(d) == set(_full_user().to_dict())
    assert d["email"] is None
    assert d["address"] is None
    assert d["postal"] is None
    assert d["id"] == 0
    assert d["name"] == ""
    assert d["tags"] == []
    assert d["counters"] == {}
    assert isinstance(d["role"], Role)
    assert d["role"] == Role.ROLE_UNSPECIFIED


def test_optional_empty_string_presence() -> None:
    # an explicitly-set empty string is not the same as unset (None)
    assert User(email="").to_dict()["email"] == ""
    assert User.from_dict({"email": ""}).email == ""


def test_enum_member_kept() -> None:
    d = _full_user().to_dict()
    assert d["role"] is Role.ROLE_ADMIN  # the IntEnum member, not a bare int
    assert d["roles"] == [Role.ROLE_USER, Role.ROLE_ADMIN]


def test_open_enum_raw_int_roundtrips() -> None:
    # value 99 is undefined; the codec keeps it a raw int (proto3 open enum).
    user = User.from_bytes(b"\x20\x63")  # role = 99
    d = user.to_dict()
    assert d["role"] == 99
    assert not isinstance(d["role"], Role)
    assert User.from_dict(d) == user

    # repeated: 1, 99, 2 -> member, raw int, member
    repeated = User.from_bytes(b"\x5a\x03\x01\x63\x02")
    dr = repeated.to_dict()
    assert dr["roles"] == [Role.ROLE_ADMIN, 99, Role.ROLE_USER]
    assert User.from_dict(dr) == repeated


def test_repeated_and_map_of_messages() -> None:
    user = _full_user()
    d = user.to_dict()
    assert d["past_addresses"] == [
        {"city": "C1", "street": ""},
        {"city": "C2", "street": ""},
    ]
    back = User.from_dict(d)
    assert back == user
    assert isinstance(back.places["home"], Address)  # map values rebuild as messages
    assert isinstance(back.past_addresses[0], Address)


def test_nested_and_cyclic() -> None:
    outer = Outer(
        inner=Outer.Inner(x=1, deeper=Outer.Inner(x=2)),
        color=Outer.Color.COLOR_RED,
        mid=Outer.Mid(leaf=Outer.Mid.Leaf(label="lf"), color=Outer.Color.COLOR_GREEN),
        inners=[Outer.Inner(x=3)],
        by_name={"k": Outer.Inner(x=4)},
    )
    d = outer.to_dict()
    assert d["inner"] == {"x": 1, "deeper": {"x": 2, "deeper": None}}
    assert Outer.from_dict(d) == outer
    # cross-message reference to a nested type
    sibling = Sibling(ref=Outer.Inner(x=7), color=Outer.Color.COLOR_RED)
    assert Sibling.from_dict(sibling.to_dict()) == sibling


def test_nested_none_and_defaults() -> None:
    d = Outer().to_dict()
    assert d["inner"] is None
    assert d["mid"] is None
    assert d["inners"] == []
    assert d["by_name"] == {}
    assert isinstance(d["color"], Outer.Color)


def test_oneof() -> None:
    user = User(phone="p")
    d = user.to_dict()
    assert d["phone"] == "p"
    assert d["telegram"] is None
    assert d["postal"] is None
    back = User.from_dict(d)
    assert back == user
    assert back.which_oneof("contact") == "phone"
    # a message-typed oneof member
    postal = User(postal=Address(city="X"))
    assert User.from_dict(postal.to_dict()) == postal


def test_native_wkt_datetime_timedelta() -> None:
    ts = datetime(2021, 6, 1, 12, 0, tzinfo=UTC)
    event = Event(
        name="e",
        created_at=ts,
        ttl=timedelta(seconds=90),
        reminders=[ts],
        checkpoints={"k": ts},
    )
    d = event.to_dict()
    assert d["created_at"] is ts  # datetime kept as-is (not a string)
    assert isinstance(d["ttl"], timedelta)
    assert d["reminders"] == [ts]
    assert d["checkpoints"] == {"k": ts}
    assert Event.from_dict(d) == event
    # unset native WKT -> None
    assert Event().to_dict()["created_at"] is None


def test_structural_wkt_struct_value() -> None:
    value = Value(
        struct_value=Struct(
            fields={
                "n": Value(number_value=2.5),
                "s": Value(string_value="hi"),
                "nul": Value(null_value=NullValue.NULL_VALUE),
                "lst": Value(list_value=ListValue(values=[Value(bool_value=True)])),
            },
        ),
    )
    d = value.to_dict()
    # structural WKTs are ordinary messages -> plain nested dicts
    assert d["struct_value"] == {
        "fields": {
            "n": _value_dict(number_value=2.5),
            "s": _value_dict(string_value="hi"),
            "nul": _value_dict(null_value=NullValue.NULL_VALUE),
            "lst": _value_dict(list_value={"values": [_value_dict(bool_value=True)]}),
        },
    }
    assert Value.from_dict(d) == value


def test_wrappers_any_and_bytes() -> None:
    payload = Payload(
        meta=Struct(fields={"a": Value(bool_value=False)}),
        extra=Any(type_url="t", value=b"\x01\x02\xff"),
        score=Int32Value(value=7),
    )
    d = payload.to_dict()
    assert d["extra"] == {"type_url": "t", "value": b"\x01\x02\xff"}  # bytes as-is
    assert d["score"] == {"value": 7}  # wrapper is a plain message here
    assert Payload.from_dict(d) == payload
    assert Payload().to_dict()["score"] is None


def test_unknown_key_raises() -> None:
    with pytest.raises(ValueError, match="nope"):
        User.from_dict({"nope": 1})


def test_missing_key_uses_default() -> None:
    assert User.from_dict({"id": 5}) == User(id=5)


def test_none_value_roundtrips() -> None:
    assert User.from_dict({"email": None}).email is None
    assert User.from_dict({"address": None}).address is None


def test_to_dict_is_deep_copy() -> None:
    user = _full_user()
    d = user.to_dict()
    cast("list[str]", d["tags"]).append("mutated")
    cast("dict[str, object]", d["address"])["city"] = "Mutated"
    cast("dict[str, int]", d["counters"])["a"] = 999
    # mutating the returned dict must not touch the source message
    assert user.tags == ["vip", "beta"]
    assert user.address is not None
    assert user.address.city == "London"
    assert user.counters == {"a": 1, "b": 2}


def test_unknown_wire_fields_kept_equal() -> None:
    # a message carrying unknown wire bytes: the dict drops them, but the
    # invariant still holds because == ignores the non-field unknown slot.
    original = User.from_bytes(User(id=1).to_bytes() + b"\x98\x06\x2a")
    assert User.from_dict(original.to_dict()) == original


def _value_dict(**set_member: object) -> dict[str, object]:
    """A fully-keyed ``Value`` dict with one member set, the rest ``None``."""
    members = {
        "null_value": None,
        "number_value": None,
        "string_value": None,
        "bool_value": None,
        "struct_value": None,
        "list_value": None,
    }
    return {**members, **set_member}


@pytest.mark.parametrize(
    "msg",
    [
        _full_user(),
        User(),
        Outer(inner=Outer.Inner(x=1), color=Outer.Color.COLOR_RED),
        Sibling(ref=Outer.Inner(x=2)),
        Event(
            name="e",
            created_at=datetime(2020, 1, 1, tzinfo=UTC),
            ttl=timedelta(hours=1),
        ),
        Payload(score=Int32Value(value=3)),
        Value(number_value=1.5),
        Value(list_value=ListValue(values=[Value(string_value="a")])),
    ],
)
def test_invariant(msg: Message) -> None:
    assert type(msg).from_dict(msg.to_dict()) == msg
