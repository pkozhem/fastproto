"""Descriptor reflection: the type index and naming helpers.

Everything here answers questions *about* the parsed protos — where a type is
defined, its module-local name, whether a field is a ``map<>`` — without
emitting any source. The renderer and validator build on it.
"""

from typing import TYPE_CHECKING, NamedTuple

from google.protobuf.descriptor_pb2 import FieldDescriptorProto

if TYPE_CHECKING:
    from collections.abc import Iterable

    from google.protobuf.descriptor_pb2 import DescriptorProto, FileDescriptorProto

# Well-known types surfaced as native Python objects instead of generated
# classes. The Rust codec converts on the wire; annotations use the stdlib type.
NATIVE_WKT: dict[str, str] = {
    ".google.protobuf.Timestamp": "datetime",
    ".google.protobuf.Duration": "timedelta",
}


class TypeInfo(NamedTuple):
    """Where a named type (message or enum) is defined."""

    file: str
    # Dotted path within the defining module, with the package stripped, e.g.
    # ``Outer.Inner`` for a nested type or ``Address`` for a top-level one. This
    # is exactly how the type is referenced in generated annotations.
    qualified: str
    message: "DescriptorProto | None"  # None for enums


class TypeIndex:
    """Every type's fully-qualified name mapped to its definition site.

    Covers messages (including nested ones, for map-entry detection) and enums
    across ALL files of the request — ``request.proto_file`` contains the full
    transitive import closure, which is what lets fields reference types from
    other ``.proto`` files.
    """

    def __init__(self, files: "Iterable[FileDescriptorProto]") -> None:
        self._index: dict[str, TypeInfo] = {}
        for file in files:
            prefix = f".{file.package}" if file.package else ""
            strip = len(prefix) + 1  # leading `.pkg.` (or just `.`) to drop
            for enum in file.enum_type:
                self._index[f"{prefix}.{enum.name}"] = TypeInfo(
                    file.name,
                    enum.name,
                    None,
                )
            self._walk(prefix, file.message_type, file.name, strip)

    def get(self, type_name: str) -> TypeInfo | None:
        """Return the definition site of ``type_name``, or None if unknown."""
        return self._index.get(type_name)

    def qualified(self, field: FieldDescriptorProto) -> str:
        """Module-local dotted path of a field's type, e.g. ``Outer.Inner``."""
        info = self._index.get(field.type_name)
        return info.qualified if info is not None else self.short_name(field.type_name)

    def qualified_name(self, full_name: str) -> str:
        """Module-local path of an indexed type by its full name (must be present)."""
        return self._index[full_name].qualified

    def map_entry(self, field: FieldDescriptorProto) -> "DescriptorProto | None":
        """Return the synthetic entry message if ``field`` is a ``map<>``, else None."""
        if (
            field.label != FieldDescriptorProto.LABEL_REPEATED
            or field.type != FieldDescriptorProto.TYPE_MESSAGE
        ):
            return None
        info = self._index.get(field.type_name)
        entry = info.message if info is not None else None
        if entry is not None and entry.options.map_entry:
            return entry
        return None

    def short_name(self, type_name: str) -> str:
        """Reduce ``.pkg.Outer.Inner`` to ``Inner``."""
        return type_name.rsplit(".", 1)[-1]

    def _walk(
        self,
        scope: str,
        messages: "Iterable[DescriptorProto]",
        file_name: str,
        strip: int,
    ) -> None:
        for msg in messages:
            full_name = f"{scope}.{msg.name}"
            self._index[full_name] = TypeInfo(file_name, full_name[strip:], msg)
            for enum in msg.enum_type:
                enum_full = f"{full_name}.{enum.name}"
                self._index[enum_full] = TypeInfo(file_name, enum_full[strip:], None)
            self._walk(full_name, msg.nested_type, file_name, strip)


def all_messages(file: "FileDescriptorProto") -> "Iterable[DescriptorProto]":
    """Every user-facing message in the file, nested ones included.

    Synthetic ``map<>`` entry messages are skipped: they are an implementation
    detail of the wire format, not types the user declared.
    """

    def walk(messages: "Iterable[DescriptorProto]") -> "Iterable[DescriptorProto]":
        for msg in messages:
            if msg.options.map_entry:
                continue
            yield msg
            yield from walk(msg.nested_type)

    yield from walk(file.message_type)


def has_presence(field: FieldDescriptorProto) -> bool:
    """Return whether the field is nullable in Python.

    True for a proto3 ``optional`` or a real ``oneof`` member -- protoc models
    the former with a synthetic single-member oneof, which we exclude.
    """
    real_oneof_member = field.HasField("oneof_index") and not field.proto3_optional
    return field.proto3_optional or real_oneof_member


def is_named_type(field: FieldDescriptorProto) -> bool:
    """Whether the field references a generated message/enum class.

    Scalars use `Scalar.*` aliases and native well-known types use stdlib
    classes imported at the top of the module — neither needs quoting or a
    cross-file class import.
    """
    return (
        field.type
        in (
            FieldDescriptorProto.TYPE_ENUM,
            FieldDescriptorProto.TYPE_MESSAGE,
        )
        and field.type_name not in NATIVE_WKT
    )
