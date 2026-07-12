"""Concurrency smoke test for lazy descriptor linking.

Linking is deferred to the first `to_bytes` / `from_bytes` and mutates the
compiled descriptor through interior mutability (a shared borrow), so a first
call racing across threads must neither error nor corrupt the result. This is a
smoke test: it can't deterministically force the race, but it guards against
deadlocks and gross regressions.
"""

import threading
from dataclasses import dataclass

from google.protobuf.descriptor_pb2 import DescriptorProto, FieldDescriptorProto

from fastproto import Message, message


def _fresh_message_class() -> type[Message]:
    """A brand-new fastproto class, so linking genuinely happens under the race."""
    dp = DescriptorProto(name="Conc")
    dp.field.add(
        name="x",
        number=1,
        type=FieldDescriptorProto.TYPE_INT64,
        label=FieldDescriptorProto.LABEL_OPTIONAL,
    )

    @message(dp.SerializeToString())
    @dataclass(slots=True)
    class Conc(Message):
        x: int = 0

    return Conc


def test_concurrent_first_decode_is_consistent() -> None:
    cls = _fresh_message_class()
    payload = b"\x08\x2a"  # field 1 (int64) = 42
    workers = 16
    barrier = threading.Barrier(workers)
    results: list[int] = []
    lock = threading.Lock()

    def run() -> None:
        barrier.wait()  # release all threads into the first decode at once
        value = cls.from_bytes(payload).x  # ty: ignore[unresolved-attribute]
        with lock:
            results.append(value)

    threads = [threading.Thread(target=run) for _ in range(workers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    assert results == [42] * workers
    assert cls.__fastproto__.is_linked
