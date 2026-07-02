"""Nested message and enum types round-trip through the codec.

Regression for the generator flattening nested `message`/`enum` declarations to
module scope (previously only top-level types were emitted, so any proto using
nesting failed to import). Also exercises the forward reference: `Outer` is
defined before the `Inner` it refers to.
"""

from tests.generated.nested_pb import Inner, Kind, Outer


def test_nested_types_exist_and_roundtrip() -> None:
    outer = Outer(
        inner=Inner(label="root"),
        kind=Kind.KIND_A,
        inners=[Inner(label="a"), Inner(label="b")],
    )
    back = Outer.from_bytes(outer.to_bytes())
    assert back == outer
    assert isinstance(back.inner, Inner)
    assert isinstance(back.inners[0], Inner)
    assert isinstance(back.kind, Kind)


def test_nested_defaults() -> None:
    empty = Outer()
    assert empty.inner is None
    assert empty.kind == Kind.KIND_UNSPECIFIED
    assert empty.inners == []
    assert Outer.from_bytes(empty.to_bytes()) == empty
