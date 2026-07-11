"""Round-trip tests for nested message/enum definitions.

Exercises types declared *inside* another message: nested messages, nested
enums, multi-level nesting, self-references, sibling-scope enum references, and
references to a nested type from another top-level message. One test cross-checks
wire compatibility against google's reference runtime.
"""

from pathlib import Path
from typing import Any

import pytest

from tests.generated.nested_pb import Outer, Sibling

FIXTURES = Path(__file__).parent / "fixtures"


def test_generated_shape() -> None:
    src = (Path(__file__).parent / "generated" / "nested_pb.py").read_text()
    # nested classes live inside their container
    assert "class Outer(Message):" in src
    assert "    class Inner(Message):" in src
    assert "        class Leaf(Message):" in src  # two levels deep
    # a nested enum can't be named eagerly in the class body, so its default is
    # deferred and its annotation is quoted
    assert 'color: "Outer.Color" = field(default_factory=lambda: Outer.Color(0))' in src


def test_defaults() -> None:
    outer = Outer()
    assert outer.inner is None
    assert outer.color == Outer.Color.COLOR_UNSPECIFIED
    assert isinstance(outer.color, Outer.Color)
    assert outer.inners == []
    assert outer.by_name == {}


def test_roundtrip_nested_and_self_reference() -> None:
    outer = Outer(
        inner=Outer.Inner(x=1, deeper=Outer.Inner(x=2)),
        color=Outer.Color.COLOR_GREEN,
        mid=Outer.Mid(leaf=Outer.Mid.Leaf(label="hi"), color=Outer.Color.COLOR_RED),
        inners=[Outer.Inner(x=3), Outer.Inner(x=4)],
        by_name={"a": Outer.Inner(x=5)},
    )
    decoded = Outer.from_bytes(outer.to_bytes())
    assert decoded == outer
    # spot-check the deep and sibling-scope paths survived
    assert decoded.inner is not None
    assert decoded.inner.deeper is not None
    assert decoded.inner.deeper.x == 2
    assert decoded.mid is not None
    assert decoded.mid.leaf is not None
    assert decoded.mid.leaf.label == "hi"
    assert decoded.mid.color == Outer.Color.COLOR_RED


def test_roundtrip_cross_scope_reference() -> None:
    sibling = Sibling(ref=Outer.Inner(x=9), color=Outer.Color.COLOR_GREEN)
    decoded = Sibling.from_bytes(sibling.to_bytes())
    assert decoded == sibling
    assert decoded.ref is not None
    assert decoded.ref.x == 9


def test_wire_compatible_with_reference() -> None:
    """Our bytes must be readable by google's protobuf, and vice versa."""
    pytest.importorskip("google.protobuf")
    from google.protobuf import descriptor_pb2, descriptor_pool, message_factory

    fileset = descriptor_pb2.FileDescriptorSet.FromString(
        (FIXTURES / "nested.fds").read_bytes(),
    )
    pool = descriptor_pool.DescriptorPool()
    for file in fileset.file:
        pool.Add(file)
    descriptor = pool.FindMessageTypeByName("nested.Outer")
    ref_cls = message_factory.GetMessageClass(descriptor)

    outer = Outer(
        inner=Outer.Inner(x=1, deeper=Outer.Inner(x=2)),
        color=Outer.Color.COLOR_GREEN,
        mid=Outer.Mid(leaf=Outer.Mid.Leaf(label="hi"), color=Outer.Color.COLOR_RED),
        inners=[Outer.Inner(x=3)],
        by_name={"a": Outer.Inner(x=5)},
    )

    # our bytes -> reference decodes them faithfully
    ref: Any = ref_cls()
    ref.ParseFromString(outer.to_bytes())
    assert ref.inner.deeper.x == 2
    assert ref.mid.leaf.label == "hi"
    assert ref.by_name["a"].x == 5

    # reference bytes -> we decode them faithfully
    assert Outer.from_bytes(ref.SerializeToString()) == outer
