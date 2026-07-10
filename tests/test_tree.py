"""Recursive message types and the nesting-depth limit.

``tree.Node`` references itself, exercising the lazy reference linker's cycle
handling and the codec's ``MAX_DEPTH`` guard (100 levels, matching google's
implementations) on both decode and encode.
"""

import pytest

from tests.generated.tree_pb import Node

MAX_DEPTH = 100


def wrap(data: bytes) -> bytes:
    """Wrap `data` as the `child` field (1, length-delimited) of an outer Node."""
    length = len(data)
    varint = b""
    while True:
        byte = length & 0x7F
        length >>= 7
        if length:
            varint += bytes([byte | 0x80])
        else:
            varint += bytes([byte])
            break
    return b"\x0a" + varint + data


def test_recursive_roundtrip() -> None:
    tree = Node(value=1, child=Node(value=2, child=Node(value=3)))
    back = Node.from_bytes(tree.to_bytes())
    assert back == tree
    assert back.child is not None
    assert back.child.child is not None
    assert back.child.child.value == 3


def test_decode_depth_limit() -> None:
    data = b"\x10\x2a"  # value = 42
    for _ in range(MAX_DEPTH + 10):
        data = wrap(data)
    with pytest.raises(ValueError, match="nesting"):
        Node.from_bytes(data)


def test_decode_at_reasonable_depth_is_fine() -> None:
    data = b"\x10\x2a"
    for _ in range(MAX_DEPTH - 5):
        data = wrap(data)
    node = Node.from_bytes(data)  # must not raise
    assert node.child is not None


def test_encode_object_cycle_is_an_error_not_a_crash() -> None:
    node = Node(value=1)
    node.child = node  # self-reference
    with pytest.raises(ValueError, match="nesting"):
        node.to_bytes()
