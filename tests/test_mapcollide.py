"""A repeated message whose short name collides with a map-entry name.

Regression for the Rust parser detecting maps by the *short* type name: a
`repeated .coll.ItemsEntry` field was misread as a `map` because the sibling
`map<string, int32> items` field generates a synthetic `Box.ItemsEntry` sharing
that short name. The parser now requires the entry's enclosing scope to match.
"""

from tests.generated.mapcollide_pb import Box, ItemsEntry


def test_colliding_repeated_message_is_not_a_map() -> None:
    box = Box(items={"a": 1, "b": 2}, extras=[ItemsEntry(note="x"), ItemsEntry()])
    back = Box.from_bytes(box.to_bytes())
    assert back == box
    assert back.items == {"a": 1, "b": 2}
    assert [e.note for e in back.extras] == ["x", ""]
    assert all(isinstance(e, ItemsEntry) for e in back.extras)
