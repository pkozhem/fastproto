"""Schema validation: reject protos we can't render into valid, working Python.

Guards the generated source against non-proto3 input, Python keywords, and names
that would shadow the runtime API or the names the generated module itself relies
on — each of which would otherwise yield a module that fails to import or
misbehaves at runtime.
"""

import keyword
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from google.protobuf.descriptor_pb2 import (
        DescriptorProto,
        EnumDescriptorProto,
        FileDescriptorProto,
    )

# Names the generated module reads at module scope: the runtime imports, the
# `@message` / `@dataclass` decorators, and the `bytes.fromhex(...)` builtin. A
# user *type* (message or enum) sharing one of these shadows it and breaks the
# module (e.g. a `bytes` message breaks the next `bytes.fromhex`; a `message`
# message shadows the decorator for later classes).
_GENERATED_NAMES = frozenset(
    {
        "Message", "Scalar", "message",  # fastproto runtime
        "dataclass", "field",  # dataclasses
        "IntEnum",  # enum
        "datetime", "timedelta",  # datetime (native well-known types)
        "bytes",  # builtin, used as bytes.fromhex(...)
    },
)  # fmt: skip

# Field names that break the runtime: the Message API, its private slots (proto
# field names *may* start with `_`, so these are reachable), and the few infra
# names the class body itself re-reads in annotations/defaults. (`message` and
# `bytes` are safe and common as fields, so they are deliberately allowed.)
_RESERVED_FIELD_NAMES = frozenset(
    {
        "to_bytes", "from_bytes", "which_oneof",  # Message API
        "_fastproto_unknown", "__fastproto__",  # Message slots / descriptor
        "field", "Scalar", "datetime", "timedelta",  # re-read in the class body
    },
)  # fmt: skip


class InvalidSchemaError(Exception):
    """The schema can't be turned into valid, working Python (bad name / syntax)."""


class SchemaValidator:
    """Checks one file's names against what the generated module needs."""

    def __init__(self, file: "FileDescriptorProto") -> None:
        self._file = file

    def validate(self) -> None:
        """Raise :class:`InvalidSchemaError` if the file can't be rendered safely."""
        file = self._file
        if file.syntax != "proto3":
            msg = (
                f"cannot generate {file.name}: only proto3 is supported"
                f" (syntax is {file.syntax or 'proto2'})"
            )
            raise InvalidSchemaError(msg)
        for enum in file.enum_type:
            self._check_enum(enum)
        for msg in file.message_type:
            self._check_message(msg)

    def _check_message(self, msg: "DescriptorProto") -> None:
        if msg.options.map_entry:
            return  # synthetic; never emitted as a class
        self._check_type_name(msg.name, "message")
        for f in msg.field:
            self._check_identifier(f.name, "field")
            if f.name in _RESERVED_FIELD_NAMES:
                msg_text = (
                    f"cannot generate {self._file.name}: field {f.name!r} is"
                    " reserved (would break the generated module)"
                )
                raise InvalidSchemaError(msg_text)
        for enum in msg.enum_type:
            self._check_enum(enum)
        for nested in msg.nested_type:
            self._check_message(nested)

    def _check_enum(self, enum: "EnumDescriptorProto") -> None:
        self._check_type_name(enum.name, "enum")
        for value in enum.value:
            self._check_identifier(value.name, "enum value")
            if self._is_reserved_enum_member(value.name):
                msg = (
                    f"cannot generate {self._file.name}: enum value"
                    f" {value.name!r} is reserved by Python's enum"
                )
                raise InvalidSchemaError(msg)

    def _check_type_name(self, name: str, what: str) -> None:
        """Reject a message/enum name that isn't valid or shadows infra."""
        self._check_identifier(name, what)
        if name in _GENERATED_NAMES:
            msg = (
                f"cannot generate {self._file.name}: {what} {name!r} shadows a"
                " name the generated module depends on"
            )
            raise InvalidSchemaError(msg)

    def _check_identifier(self, name: str, what: str) -> None:
        """Reject a name that isn't a usable Python identifier."""
        if not name.isidentifier() or keyword.iskeyword(name):
            msg = (
                f"cannot generate {self._file.name}: {what} {name!r} is not a"
                " valid Python identifier"
            )
            raise InvalidSchemaError(msg)

    def _is_reserved_enum_member(self, name: str) -> bool:
        """Whether Python's ``enum`` rejects ``name`` (``mro`` or a ``_sunder_``)."""
        is_sunder = (
            name.startswith("_") and name.endswith("_") and not name.startswith("__")
        )
        return name == "mro" or is_sunder
