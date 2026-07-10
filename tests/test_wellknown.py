"""Native well-known types: Timestamp <-> datetime, Duration <-> timedelta.

The codec converts on the wire (exact integer math on days/seconds/micros);
protobuf's sub-microsecond precision is truncated by design. Naive datetimes
are read as UTC; decoded datetimes are always aware UTC.
"""

from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from tests.generated.event_pb import Event
from tests.generated.wkt_pb import Payload

FIXED = datetime(2026, 7, 10, 12, 30, 45, 123456, tzinfo=UTC)


def test_timestamp_roundtrip() -> None:
    event = Event(name="release", created_at=FIXED)
    back = Event.from_bytes(event.to_bytes())
    assert back.created_at == FIXED
    assert back.created_at is not None
    assert back.created_at.tzinfo is not None  # always aware UTC


def test_naive_datetime_is_read_as_utc() -> None:
    naive = FIXED.replace(tzinfo=None)
    back = Event.from_bytes(Event(created_at=naive).to_bytes())
    assert back.created_at == FIXED


def test_pre_epoch_timestamp() -> None:
    old = datetime(1969, 12, 31, 23, 59, 59, 500000, tzinfo=UTC)
    back = Event.from_bytes(Event(created_at=old).to_bytes())
    assert back.created_at == old


def test_duration_roundtrip_including_negative() -> None:
    for delta in [
        timedelta(days=2, hours=3, microseconds=7),
        timedelta(0),
        -timedelta(hours=5, microseconds=250),
    ]:
        back = Event.from_bytes(Event(ttl=delta).to_bytes())
        assert back.ttl == delta


def test_repeated_and_map_timestamps() -> None:
    times = [FIXED, FIXED + timedelta(days=1)]
    event = Event(reminders=times, checkpoints={"start": FIXED})
    back = Event.from_bytes(event.to_bytes())
    assert back.reminders == times
    assert back.checkpoints == {"start": FIXED}


def test_unset_fields_stay_none() -> None:
    empty = Event.from_bytes(Event().to_bytes())
    assert empty.created_at is None
    assert empty.ttl is None


def test_wire_compatible_with_reference() -> None:
    """Bytes must interoperate with google's runtime, including nanos."""
    pytest.importorskip("google.protobuf")
    from google.protobuf import descriptor_pb2, descriptor_pool, message_factory

    data = Event(name="x", created_at=FIXED).to_bytes()
    fileset = descriptor_pb2.FileDescriptorSet.FromString(
        (Path(__file__).parent / "fixtures" / "event.fds").read_bytes(),
    )
    pool = descriptor_pool.DescriptorPool()
    for file in fileset.file:
        pool.Add(file)
    ref_cls = message_factory.GetMessageClass(pool.FindMessageTypeByName("event.Event"))
    ref: Any = ref_cls()  # reflective google message is dynamically typed
    ref.ParseFromString(data)
    assert ref.created_at.ToDatetime(tzinfo=UTC) == FIXED

    # google bytes with sub-microsecond nanos -> our decode truncates to µs
    ref2: Any = ref_cls(name="y")
    ref2.created_at.seconds = 1_720_620_000
    ref2.created_at.nanos = 1_999
    ours = Event.from_bytes(ref2.SerializeToString())
    expected = datetime.fromtimestamp(1_720_620_000, tz=UTC) + timedelta(microseconds=1)
    assert ours.created_at == expected  # 1999ns -> 1µs, remainder truncated


def test_struct_value_roundtrip() -> None:
    from fastproto.wellknown import ListValue, NullValue, Struct, Value

    payload = Payload(
        meta=Struct(
            fields={
                "name": Value(string_value="fastproto"),
                "count": Value(number_value=3.0),
                "ok": Value(bool_value=True),
                "nothing": Value(null_value=NullValue.NULL_VALUE),
                "nested": Value(
                    list_value=ListValue(values=[Value(string_value="x")]),
                ),
            },
        ),
    )
    back = Payload.from_bytes(payload.to_bytes())
    assert back == payload
    assert back.meta is not None
    assert back.meta.fields["nested"].list_value is not None


def test_wrappers_and_any_roundtrip() -> None:
    from fastproto.wellknown import Any, Int32Value

    payload = Payload(extra=Any(type_url="x/y", value=b"\x01"), score=Int32Value())
    back = Payload.from_bytes(payload.to_bytes())
    assert back == payload
    assert back.score is not None
    assert back.score.value == 0  # explicitly-set wrapper survives (presence)


def test_struct_wire_compatible_with_reference() -> None:
    pytest.importorskip("google.protobuf")
    from google.protobuf import struct_pb2

    from fastproto.wellknown import Struct, Value

    ours = Payload(meta=Struct(fields={"k": Value(number_value=2.5)}))
    ref = struct_pb2.Struct()
    # Payload.meta is field 1: strip its tag+len to get the bare Struct bytes.
    raw = ours.to_bytes()
    assert raw[0] == 0x0A
    ref.ParseFromString(raw[2:])
    assert ref["k"] == 2.5
