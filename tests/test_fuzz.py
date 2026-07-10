"""Property-based fuzzing of the decoder and descriptor compiler.

Arbitrary bytes fed to the public API must either produce a valid object or
raise a clean Python exception (`ValueError` for malformed wire data /
descriptors, `UnicodeDecodeError` for invalid UTF-8 in string fields) — never
crash, hang, or exhaust the stack. The Rust-level counterparts (wire reader,
descriptor parser) are property-tested with proptest in ``src/**/tests.rs``.
"""

from contextlib import suppress

from hypothesis import given, settings
from hypothesis import strategies as st

from fastproto._core import compile_descriptor
from tests.generated.rich_pb import User
from tests.generated.tree_pb import Node

DECODE_ERRORS = (ValueError, UnicodeDecodeError)


@given(st.binary(max_size=512))
@settings(max_examples=300)
def test_decode_arbitrary_bytes_never_crashes(data: bytes) -> None:
    with suppress(*DECODE_ERRORS):
        User.from_bytes(data)


@given(st.binary(max_size=512))
@settings(max_examples=300)
def test_decode_recursive_type_never_crashes(data: bytes) -> None:
    with suppress(*DECODE_ERRORS):
        Node.from_bytes(data)


@given(st.binary(max_size=512))
@settings(max_examples=300)
def test_compile_descriptor_never_crashes(data: bytes) -> None:
    with suppress(ValueError):
        compile_descriptor(data)


@given(st.binary(max_size=256))
@settings(max_examples=200)
def test_valid_prefix_plus_junk_never_crashes(junk: bytes) -> None:
    # A valid message with garbage appended is a realistic corruption shape.
    data = User(id=7, name="x").to_bytes() + junk
    with suppress(*DECODE_ERRORS):
        User.from_bytes(data)


@given(st.binary(max_size=512))
@settings(max_examples=200)
def test_successful_decode_reencodes_cleanly(data: bytes) -> None:
    # Whatever the decoder accepts, the encoder must be able to serialize
    # (including preserved unknown fields) without errors.
    try:
        msg = User.from_bytes(data)
    except DECODE_ERRORS:
        return
    reencoded = msg.to_bytes()
    assert isinstance(reencoded, bytes)
