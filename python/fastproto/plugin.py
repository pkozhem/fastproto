"""``protoc-gen-fastproto``: the protoc code generator plugin.

protoc runs this as a subprocess, handing it a serialized ``CodeGeneratorRequest``
on stdin and reading a ``CodeGeneratorResponse`` from stdout. For every input
``.proto`` we emit a ``<name>_pb.py`` containing readable dataclasses annotated
with ``Scalar.*`` and decorated with :func:`fastproto.message`.

The embedded descriptor for each message is simply that message's own
``DescriptorProto`` bytes -- exactly what the native ``compile_descriptor``
consumes at import time.

Note: ``google.protobuf`` is used *here*, at build time, only to parse the
request. Generated modules depend solely on ``fastproto`` at runtime.
"""

import keyword
import sys
from collections.abc import Iterable
from typing import NamedTuple

from google.protobuf.compiler import plugin_pb2
from google.protobuf.descriptor_pb2 import (
    DescriptorProto,
    EnumDescriptorProto,
    FieldDescriptorProto,
    FileDescriptorProto,
)


class _TypeInfo(NamedTuple):
    """Where a named type (message or enum) is defined."""

    file: str
    # Dotted path within the defining module, with the package stripped, e.g.
    # ``Outer.Inner`` for a nested type or ``Address`` for a top-level one. This
    # is exactly how the type is referenced in generated annotations.
    qualified: str
    message: DescriptorProto | None  # None for enums


class _ShortNameCollisionError(Exception):
    """Two types visible from one module share a short class name."""


class _InvalidSchemaError(Exception):
    """The schema can't be turned into valid, working Python (bad name / syntax)."""


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


def _is_reserved_enum_member(name: str) -> bool:
    """Whether Python's ``enum`` rejects ``name`` (``mro`` or a ``_sunder_``)."""
    is_sunder = (
        name.startswith("_") and name.endswith("_") and not name.startswith("__")
    )
    return name == "mro" or is_sunder


# Field numbers of the synthetic map entry message (key, value).
_MAP_KEY_FIELD = 1
_MAP_VALUE_FIELD = 2

# proto scalar type -> (Scalar alias, default literal)
_SCALAR: dict[int, tuple[str, str]] = {
    FieldDescriptorProto.TYPE_DOUBLE: ("Scalar.Double", "0.0"),
    FieldDescriptorProto.TYPE_FLOAT: ("Scalar.Float", "0.0"),
    FieldDescriptorProto.TYPE_INT64: ("Scalar.Int64", "0"),
    FieldDescriptorProto.TYPE_UINT64: ("Scalar.UInt64", "0"),
    FieldDescriptorProto.TYPE_INT32: ("Scalar.Int32", "0"),
    FieldDescriptorProto.TYPE_FIXED64: ("Scalar.Fixed64", "0"),
    FieldDescriptorProto.TYPE_FIXED32: ("Scalar.Fixed32", "0"),
    FieldDescriptorProto.TYPE_BOOL: ("Scalar.Bool", "False"),
    FieldDescriptorProto.TYPE_STRING: ("Scalar.String", '""'),
    FieldDescriptorProto.TYPE_BYTES: ("Scalar.Bytes", 'b""'),
    FieldDescriptorProto.TYPE_UINT32: ("Scalar.UInt32", "0"),
    FieldDescriptorProto.TYPE_SFIXED32: ("Scalar.SFixed32", "0"),
    FieldDescriptorProto.TYPE_SFIXED64: ("Scalar.SFixed64", "0"),
    FieldDescriptorProto.TYPE_SINT32: ("Scalar.SInt32", "0"),
    FieldDescriptorProto.TYPE_SINT64: ("Scalar.SInt64", "0"),
}


# Well-known types surfaced as native Python objects instead of generated
# classes. The Rust codec converts on the wire; annotations use the stdlib type.
_NATIVE_WKT: dict[str, str] = {
    ".google.protobuf.Timestamp": "datetime",
    ".google.protobuf.Duration": "timedelta",
}


def _short_name(type_name: str) -> str:
    """Reduce ``.pkg.Outer.Inner`` to ``Inner``."""
    return type_name.rsplit(".", 1)[-1]


def _index_types(files: Iterable[FileDescriptorProto]) -> dict[str, _TypeInfo]:
    """Map every type's fully-qualified name to its definition site.

    Covers messages (including nested ones, for map-entry detection) and enums
    across ALL files of the request — `request.proto_file` contains the full
    transitive import closure, which is what lets fields reference types from
    other `.proto` files.
    """
    index: dict[str, _TypeInfo] = {}
    for file in files:
        prefix = f".{file.package}" if file.package else ""
        strip = len(prefix) + 1  # leading `.pkg.` (or just `.`) to drop

        def walk(
            scope: str,
            messages: Iterable[DescriptorProto],
            file_name: str,
            strip: int,
        ) -> None:
            for msg in messages:
                full_name = f"{scope}.{msg.name}"
                index[full_name] = _TypeInfo(file_name, full_name[strip:], msg)
                for enum in msg.enum_type:
                    enum_full = f"{full_name}.{enum.name}"
                    index[enum_full] = _TypeInfo(file_name, enum_full[strip:], None)
                walk(full_name, msg.nested_type, file_name, strip)

        for enum in file.enum_type:
            index[f"{prefix}.{enum.name}"] = _TypeInfo(file.name, enum.name, None)
        walk(prefix, file.message_type, file.name, strip)
    return index


def _module_of(proto_name: str) -> str:
    """Dotted module path of a generated file: ``a/b.proto`` -> ``a.b_pb``."""
    return f"{proto_name.removesuffix('.proto')}_pb".replace("/", ".")


def _all_messages(file: FileDescriptorProto) -> Iterable[DescriptorProto]:
    """Every user-facing message in the file, nested ones included.

    Synthetic ``map<>`` entry messages are skipped: they are an implementation
    detail of the wire format, not types the user declared.
    """

    def walk(messages: Iterable[DescriptorProto]) -> Iterable[DescriptorProto]:
        for msg in messages:
            if msg.options.map_entry:
                continue
            yield msg
            yield from walk(msg.nested_type)

    yield from walk(file.message_type)


def _qualified(field: FieldDescriptorProto, index: dict[str, _TypeInfo]) -> str:
    """Module-local dotted path of a field's enum/message type, e.g. ``Outer.Inner``."""
    info = index.get(field.type_name)
    return info.qualified if info is not None else _short_name(field.type_name)


def _referenced_type_names(
    file: FileDescriptorProto,
    index: dict[str, _TypeInfo],
) -> list[str]:
    """Full names of every message/enum referenced by this file's fields."""
    names: list[str] = []
    for msg in _all_messages(file):
        for f in msg.field:
            entry = _map_entry(f, index)
            target = (
                next(x for x in entry.field if x.number == _MAP_VALUE_FIELD)
                if entry is not None
                else f
            )
            if _is_named_type(target):
                names.append(target.type_name)
    return names


def _uses_scalar(file: FileDescriptorProto, index: dict[str, _TypeInfo]) -> bool:
    """Whether any generated annotation references a ``Scalar.*`` alias."""
    for msg in _all_messages(file):
        for f in msg.field:
            if _map_entry(f, index) is not None:
                return True  # map keys are always scalar
            if f.type in _SCALAR:
                return True
    return False


def _native_names(file: FileDescriptorProto, index: dict[str, _TypeInfo]) -> list[str]:
    """Sorted stdlib names (datetime/timedelta) referenced by this file."""
    names: set[str] = set()
    for msg in _all_messages(file):
        for f in msg.field:
            entry = _map_entry(f, index)
            target = (
                next(x for x in entry.field if x.number == _MAP_VALUE_FIELD)
                if entry is not None
                else f
            )
            if target.type_name in _NATIVE_WKT:
                names.add(_NATIVE_WKT[target.type_name])
    return sorted(names)


def _external_imports(
    file: FileDescriptorProto,
    index: dict[str, _TypeInfo],
) -> list[str]:
    """Dual-import lines for types this file references from other files.

    Emits ``try: from .x_pb import Y / except ImportError: from x_pb import Y``
    so generated modules work both inside a package and in a flat directory.
    Only the top-level container is imported; nested types are reached through
    it (``Outer.Inner``). Raises :class:`_ShortNameCollisionError` when two
    distinct top-level types share a name (the imports couldn't tell them apart).
    """
    # top-level name -> the `file:name` that owns it, seeded with this file's own
    # top-level definitions.
    owner: dict[str, str] = {}
    for enum in file.enum_type:
        owner[enum.name] = f"{file.name}:{enum.name}"
    for msg in file.message_type:
        owner[msg.name] = f"{file.name}:{msg.name}"

    # module -> sorted set of top-level names to import from it.
    by_module: dict[str, set[str]] = {}
    wellknown: set[str] = set()

    def consider(type_name: str) -> None:
        info = index.get(type_name)
        if info is None or info.file == file.name:
            return
        top = info.qualified.split(".")[0]
        claimant = f"{info.file}:{top}"
        if owner.setdefault(top, claimant) != claimant:
            msg = (
                f"cannot generate {file.name}: two different types would both be"
                f" imported as `{top}` ({owner[top]} and {claimant})"
            )
            raise _ShortNameCollisionError(msg)
        if info.file.startswith("google/protobuf/"):
            # Structural well-known types ship inside the fastproto package.
            wellknown.add(top)
        else:
            by_module.setdefault(_module_of(info.file), set()).add(top)

    for type_name in _referenced_type_names(file, index):
        consider(type_name)

    lines: list[str] = []
    if wellknown:
        lines.append(f"from fastproto.wellknown import {', '.join(sorted(wellknown))}")
    depth = file.name.count("/")
    for module, names in sorted(by_module.items()):
        joined = ", ".join(sorted(names))
        relative = "." * (1 + depth) + module
        lines += [
            "try:",
            f"    from {relative} import {joined}",
            "except ImportError:  # generated modules used outside a package",
            f"    from {module} import {joined}",
        ]
    return lines


def _map_entry(
    field: FieldDescriptorProto,
    index: dict[str, _TypeInfo],
) -> DescriptorProto | None:
    """Return the synthetic entry message if ``field`` is a ``map<>``, else None."""
    if (
        field.label != FieldDescriptorProto.LABEL_REPEATED
        or field.type != FieldDescriptorProto.TYPE_MESSAGE
    ):
        return None
    info = index.get(field.type_name)
    entry = info.message if info is not None else None
    if entry is not None and entry.options.map_entry:
        return entry
    return None


def _element_annotation(
    field: FieldDescriptorProto,
    index: dict[str, _TypeInfo],
) -> str:
    """Return the annotation for a single scalar/enum/message element."""
    if field.type in _SCALAR:
        return _SCALAR[field.type][0]
    if field.type_name in _NATIVE_WKT:
        return _NATIVE_WKT[field.type_name]
    if field.type in (
        FieldDescriptorProto.TYPE_ENUM,
        FieldDescriptorProto.TYPE_MESSAGE,
    ):
        return _qualified(field, index)
    return "object"  # group / unknown


def _has_presence(field: FieldDescriptorProto) -> bool:
    """Return whether the field is nullable in Python.

    True for a proto3 ``optional`` or a real ``oneof`` member -- protoc models
    the former with a synthetic single-member oneof, which we exclude.
    """
    real_oneof_member = field.HasField("oneof_index") and not field.proto3_optional
    return field.proto3_optional or real_oneof_member


def _is_named_type(field: FieldDescriptorProto) -> bool:
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
        and field.type_name not in _NATIVE_WKT
    )


def _render_field(field: FieldDescriptorProto, index: dict[str, _TypeInfo]) -> str:
    """Render one dataclass field line (without leading indentation).

    Annotations that reference a message or enum class are quoted: class-body
    annotations are evaluated eagerly (until PEP 649 lands in 3.14), so an
    unquoted forward or cyclic reference would raise ``NameError`` at import.
    """
    entry = _map_entry(field, index)
    if entry is not None:
        key_field = next(f for f in entry.field if f.number == _MAP_KEY_FIELD)
        value_field = next(f for f in entry.field if f.number == _MAP_VALUE_FIELD)
        key = _element_annotation(key_field, index)
        value = _element_annotation(value_field, index)
        annotation = f"dict[{key}, {value}]"
        if _is_named_type(value_field):  # map keys are always scalar
            annotation = f'"{annotation}"'
        return f"{field.name}: {annotation} = field(default_factory=dict)"

    if field.label == FieldDescriptorProto.LABEL_REPEATED:
        element = _element_annotation(field, index)
        annotation = f"list[{element}]"
        if _is_named_type(field):
            annotation = f'"{annotation}"'
        return f"{field.name}: {annotation} = field(default_factory=list)"

    return _render_singular_field(field, index)


def _render_enum_field(field: FieldDescriptorProto, index: dict[str, _TypeInfo]) -> str:
    """Render a singular enum field.

    A nested enum (``Outer.Color``) can't be named in the class body — the
    enclosing class isn't defined yet — so its annotation is quoted and its
    zero-value default is deferred to a factory that runs at instance creation.
    Top-level enums stay eager and unquoted.
    """
    qual = _qualified(field, index)
    nested = "." in qual
    if _has_presence(field):
        annotation = f'"{qual} | None"' if nested else f"{qual} | None"
        return f"{field.name}: {annotation} = None"
    if nested:
        return f'{field.name}: "{qual}" = field(default_factory=lambda: {qual}(0))'
    return f"{field.name}: {qual} = {qual}(0)"


def _render_singular_field(
    field: FieldDescriptorProto,
    index: dict[str, _TypeInfo],
) -> str:
    """Render a non-repeated, non-map field line."""
    if field.type_name in _NATIVE_WKT:
        # Native well-known type: plain stdlib object with presence.
        return f"{field.name}: {_NATIVE_WKT[field.type_name]} | None = None"

    if field.type == FieldDescriptorProto.TYPE_MESSAGE:
        # Message fields always carry presence.
        return f'{field.name}: "{_qualified(field, index)} | None" = None'

    if field.type == FieldDescriptorProto.TYPE_ENUM:
        return _render_enum_field(field, index)

    annotation, default = _SCALAR[field.type]
    if _has_presence(field):
        return f"{field.name}: {annotation} | None = None"
    return f"{field.name}: {annotation} = {default}"


def _render_enum(enum: EnumDescriptorProto, indent: int = 0) -> str:
    """Render an enum as an ``IntEnum`` subclass, indented for nesting."""
    pad = "    " * indent
    body = [f"{pad}    {value.name} = {value.number}" for value in enum.value]
    header = f"{pad}class {enum.name}(IntEnum):"
    return "\n".join([header, *(body or [f"{pad}    pass"])])


def _descriptor_const_name(qualified: str) -> str:
    """Descriptor constant name: ``Outer.Inner`` -> ``_OUTER_INNER_DESCRIPTOR``."""
    return f"_{qualified.replace('.', '_').upper()}_DESCRIPTOR"


def _collect_constants(
    msg: DescriptorProto,
    full_name: str,
    index: dict[str, _TypeInfo],
) -> list[str]:
    """Descriptor constants for ``msg`` and its nested messages, outermost first.

    The constants live at module level (even for nested classes) so a class
    body can reference its own constant, which is defined just above it.
    """
    const = _descriptor_const_name(index[full_name].qualified)
    blocks = [
        "\n".join(
            [
                f"{const} = bytes.fromhex(  # @generated",
                f'    "{msg.SerializeToString().hex()}"',
                ")",
            ],
        ),
    ]
    for nested in msg.nested_type:
        if not nested.options.map_entry:
            blocks += _collect_constants(nested, f"{full_name}.{nested.name}", index)
    return blocks


def _render_class(
    msg: DescriptorProto,
    full_name: str,
    index: dict[str, _TypeInfo],
    indent: int,
) -> str:
    """Render one message class, recursing into nested enums and messages."""
    pad = "    " * indent
    inner = pad + "    "
    const = _descriptor_const_name(index[full_name].qualified)
    header = [
        f"{pad}@message({const})",
        f"{pad}@dataclass(slots=True)",
        f"{pad}class {msg.name}(Message):",
    ]
    members = [_render_enum(enum, indent + 1) for enum in msg.enum_type]
    members += [
        _render_class(nested, f"{full_name}.{nested.name}", index, indent + 1)
        for nested in msg.nested_type
        if not nested.options.map_entry
    ]
    fields = [f"{inner}{_render_field(f, index)}" for f in msg.field]
    body = [*members]
    if fields:
        body.append("\n".join(fields))
    if not body:
        body = [f"{inner}pass"]
    return "\n".join(header) + "\n" + "\n\n".join(body)


def _render_message(
    msg: DescriptorProto,
    full_name: str,
    index: dict[str, _TypeInfo],
) -> str:
    """Render a message: its descriptor constants plus the decorated class tree."""
    constants = _collect_constants(msg, full_name, index)
    class_src = _render_class(msg, full_name, index, 0)
    return "\n\n\n".join([*constants, class_src])


def _render_header(
    file: FileDescriptorProto,
    index: dict[str, _TypeInfo],
    external: list[str],
    natives: list[str],
) -> str:
    """Render the module header: banner, imports, and cross-file imports."""
    # `field(...)` is emitted for repeated/map fields (default_factory) and for
    # nested-enum singular fields (a deferred zero-value factory).
    needs_field = any(
        f.label == FieldDescriptorProto.LABEL_REPEATED
        or (f.type == FieldDescriptorProto.TYPE_ENUM and "." in _qualified(f, index))
        for msg in _all_messages(file)
        for f in msg.field
    )
    has_enum = bool(file.enum_type) or any(m.enum_type for m in _all_messages(file))
    dataclass_import = (
        "from dataclasses import dataclass, field"
        if needs_field
        else "from dataclasses import dataclass"
    )
    lines = [
        "# @generated by fastproto. DO NOT EDIT.",
        f"# source: {file.name}",
    ]
    if needs_field:
        # `field(default_factory=list/dict)` makes Pyright's strict mode infer
        # `list[Unknown]`; the annotation already states the real element type.
        lines.append("# pyright: reportUnknownVariableType=false")
    lines.append(dataclass_import)
    if natives:
        lines.append(f"from datetime import {', '.join(natives)}")
    if has_enum:
        lines.append("from enum import IntEnum")
    names = (
        "Message, Scalar, message" if _uses_scalar(file, index) else "Message, message"
    )
    lines += ["", f"from fastproto import {names}"]
    if external:
        lines += ["", *external]
    return "\n".join(lines)


def _file_blocks(file: FileDescriptorProto, index: dict[str, _TypeInfo]) -> list[str]:
    """Enum and message blocks of one proto file (no header)."""
    prefix = f".{file.package}" if file.package else ""
    blocks = [_render_enum(enum) for enum in file.enum_type]
    blocks += [
        _render_message(msg, f"{prefix}.{msg.name}", index)
        for msg in file.message_type
        if not msg.options.map_entry  # synthetic map entries are not user-facing
    ]
    return blocks


def _generate_file(file: FileDescriptorProto, index: dict[str, _TypeInfo]) -> str:
    """Render the full ``<name>_pb.py`` source for one proto file."""
    header = _render_header(
        file,
        index,
        _external_imports(file, index),
        _native_names(file, index),
    )
    return "\n\n\n".join([header, *_file_blocks(file, index)]) + "\n"


# Files whose types are bundled as structural dataclasses in
# ``fastproto.wellknown`` (Timestamp/Duration are native and excluded).
WELLKNOWN_PROTOS = [
    "google/protobuf/any.proto",
    "google/protobuf/empty.proto",
    "google/protobuf/field_mask.proto",
    "google/protobuf/struct.proto",
    "google/protobuf/wrappers.proto",
]

_WELLKNOWN_HEADER = '''"""Structural well-known types, bundled with fastproto.

@generated by scripts/regen.py from protoc's google/protobuf descriptors --
DO NOT EDIT. `Timestamp` and `Duration` are not here: the codec maps them to
`datetime` / `timedelta` natively. Note that `Any` is protobuf's
``google.protobuf.Any`` message, not ``typing.Any``.
"""

# pyright: reportUnknownVariableType=false
from dataclasses import dataclass, field
from enum import IntEnum

from fastproto import Message, Scalar, message'''


def generate_wellknown(files: Iterable[FileDescriptorProto]) -> str:
    """Render ``fastproto/wellknown.py`` from the WKT file descriptors.

    The well-known ``.proto`` files don't import each other, so their blocks
    concatenate safely into one module. Shared by ``scripts/regen.py`` and the
    golden test.
    """
    by_name = {f.name: f for f in files}
    ordered = [by_name[name] for name in WELLKNOWN_PROTOS]
    index = _index_types(ordered)
    blocks = [_WELLKNOWN_HEADER]
    for file in ordered:
        blocks += _file_blocks(file, index)
    return "\n\n\n".join(blocks) + "\n"


def _output_name(proto_name: str) -> str:
    """Map ``foo/bar.proto`` to ``foo/bar_pb.py``."""
    return f"{proto_name.removesuffix('.proto')}_pb.py"


def _check_identifier(file_name: str, name: str, what: str) -> None:
    """Reject a name that isn't a usable Python identifier."""
    if not name.isidentifier() or keyword.iskeyword(name):
        msg = (
            f"cannot generate {file_name}: {what} {name!r} is not a valid Python"
            " identifier"
        )
        raise _InvalidSchemaError(msg)


def _check_type_name(file_name: str, name: str, what: str) -> None:
    """Reject a message/enum name that isn't a valid identifier or shadows infra."""
    _check_identifier(file_name, name, what)
    if name in _GENERATED_NAMES:
        msg = (
            f"cannot generate {file_name}: {what} {name!r} shadows a name the"
            " generated module depends on"
        )
        raise _InvalidSchemaError(msg)


def _check_enum(file_name: str, enum: EnumDescriptorProto) -> None:
    _check_type_name(file_name, enum.name, "enum")
    for value in enum.value:
        _check_identifier(file_name, value.name, "enum value")
        if _is_reserved_enum_member(value.name):
            msg = (
                f"cannot generate {file_name}: enum value {value.name!r} is"
                " reserved by Python's enum"
            )
            raise _InvalidSchemaError(msg)


def _check_message(file_name: str, msg: DescriptorProto) -> None:
    if msg.options.map_entry:
        return  # synthetic; never emitted as a class
    _check_type_name(file_name, msg.name, "message")
    for f in msg.field:
        _check_identifier(file_name, f.name, "field")
        if f.name in _RESERVED_FIELD_NAMES:
            msg_text = (
                f"cannot generate {file_name}: field {f.name!r} is reserved (would"
                " break the generated module)"
            )
            raise _InvalidSchemaError(msg_text)
    for enum in msg.enum_type:
        _check_enum(file_name, enum)
    for nested in msg.nested_type:
        _check_message(file_name, nested)


def _validate_file(file: FileDescriptorProto) -> None:
    """Reject schemas we can't render into valid, working proto3 Python.

    Guards the generated source against non-proto3 input, Python keywords, and
    names that would shadow the runtime API — each of which would otherwise
    yield a module that fails to import or misbehaves at runtime.
    """
    if file.syntax != "proto3":
        msg = (
            f"cannot generate {file.name}: only proto3 is supported"
            f" (syntax is {file.syntax or 'proto2'})"
        )
        raise _InvalidSchemaError(msg)
    for enum in file.enum_type:
        _check_enum(file.name, enum)
    for msg in file.message_type:
        _check_message(file.name, msg)


def generate(
    request: plugin_pb2.CodeGeneratorRequest,
) -> plugin_pb2.CodeGeneratorResponse:
    """Turn a protoc request into a response of generated ``_pb.py`` files."""
    response = plugin_pb2.CodeGeneratorResponse()
    response.supported_features = (
        plugin_pb2.CodeGeneratorResponse.FEATURE_PROTO3_OPTIONAL
    )
    files_by_name = {f.name: f for f in request.proto_file}
    index = _index_types(request.proto_file)
    for proto_name in request.file_to_generate:
        file = files_by_name[proto_name]
        try:
            _validate_file(file)
            content = _generate_file(file, index)
        except (_ShortNameCollisionError, _InvalidSchemaError) as exc:
            response.error = str(exc)
            return response
        output = response.file.add()
        output.name = _output_name(proto_name)
        output.content = content
    return response


def main() -> None:
    """Read a request from stdin and write the response to stdout."""
    request = plugin_pb2.CodeGeneratorRequest.FromString(sys.stdin.buffer.read())
    sys.stdout.buffer.write(generate(request).SerializeToString())


if __name__ == "__main__":
    main()
