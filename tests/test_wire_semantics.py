"""Decoder conformance with proto3 wire semantics.

These craft raw wire bytes (or concatenate encodings) to exercise decode paths
that valid high-level construction can't reach, covering three fixed bugs:

* unknown enum values are preserved, not rejected (open-enum semantics);
* a field carrying an unexpected wire type is skipped without desync;
* repeated occurrences of a singular message field merge rather than overwrite.
"""

from tests.generated.rich_pb import Address, Role, User

# rich.User field numbers: id=1, role=4, address=7, roles=11.


def test_unknown_singular_enum_is_preserved() -> None:
    # tag = field 4, wire type 0 (varint); value 99 has no named member.
    back = User.from_bytes(b"\x20\x63")
    assert back.role == 99


def test_unknown_packed_enum_is_preserved() -> None:
    # tag = field 11, wire type 2 (len); one packed varint 99.
    back = User.from_bytes(b"\x5a\x01\x63")
    assert back.roles == [99]


def test_wire_type_mismatch_on_enum_is_skipped() -> None:
    # role (field 4) arrives length-delimited instead of varint; id follows.
    back = User.from_bytes(b"\x22\x01\x78\x08\x07")
    assert back.id == 7  # reader stayed in sync
    assert back.role == Role.ROLE_UNSPECIFIED  # bad field left at default


def test_wire_type_mismatch_on_message_is_skipped() -> None:
    # address (field 7) arrives as a varint instead of length-delimited.
    back = User.from_bytes(b"\x38\x05\x08\x07")
    assert back.id == 7
    assert back.address is None


def test_repeated_singular_message_merges() -> None:
    # Two encodings each set a different subfield of `address`. Protobuf merge
    # semantics require the concatenation to combine both, not keep only the last.
    first = User(address=Address(city="A")).to_bytes()
    second = User(address=Address(street="B")).to_bytes()
    merged = User.from_bytes(first + second)
    assert merged.address == Address(city="A", street="B")
