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

import sys
from collections.abc import Iterable

from google.protobuf.compiler import plugin_pb2
from google.protobuf.descriptor_pb2 import (
    DescriptorProto,
    EnumDescriptorProto,
    FieldDescriptorProto,
    FileDescriptorProto,
)

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


def _short_name(type_name: str) -> str:
    """Reduce ``.pkg.Outer.Inner`` to ``Inner``."""
    return type_name.rsplit(".", 1)[-1]


def _index_messages(file: FileDescriptorProto) -> dict[str, DescriptorProto]:
    """Map every (possibly nested) message's fully-qualified name to its proto."""
    index: dict[str, DescriptorProto] = {}
    prefix = f".{file.package}" if file.package else ""

    def walk(scope: str, messages: Iterable[DescriptorProto]) -> None:
        for msg in messages:
            full_name = f"{scope}.{msg.name}"
            index[full_name] = msg
            walk(full_name, msg.nested_type)

    walk(prefix, file.message_type)
    return index


def _map_entry(
    field: FieldDescriptorProto,
    index: dict[str, DescriptorProto],
) -> DescriptorProto | None:
    """Return the synthetic entry message if ``field`` is a ``map<>``, else None."""
    if (
        field.label != FieldDescriptorProto.LABEL_REPEATED
        or field.type != FieldDescriptorProto.TYPE_MESSAGE
    ):
        return None
    entry = index.get(field.type_name)
    if entry is not None and entry.options.map_entry:
        return entry
    return None


def _element_annotation(field: FieldDescriptorProto) -> str:
    """Return the annotation for a single scalar/enum/message element."""
    if field.type in _SCALAR:
        return _SCALAR[field.type][0]
    if field.type in (
        FieldDescriptorProto.TYPE_ENUM,
        FieldDescriptorProto.TYPE_MESSAGE,
    ):
        return _short_name(field.type_name)
    return "object"  # group / unknown


def _has_presence(field: FieldDescriptorProto) -> bool:
    """Return whether the field is nullable in Python.

    True for a proto3 ``optional`` or a real ``oneof`` member -- protoc models
    the former with a synthetic single-member oneof, which we exclude.
    """
    real_oneof_member = field.HasField("oneof_index") and not field.proto3_optional
    return field.proto3_optional or real_oneof_member


def _render_field(
    field: FieldDescriptorProto, index: dict[str, DescriptorProto]
) -> str:
    """Render one dataclass field line (without leading indentation)."""
    entry = _map_entry(field, index)
    if entry is not None:
        key_field = next(f for f in entry.field if f.number == _MAP_KEY_FIELD)
        value_field = next(f for f in entry.field if f.number == _MAP_VALUE_FIELD)
        key, value = _element_annotation(key_field), _element_annotation(value_field)
        return f"{field.name}: dict[{key}, {value}] = field(default_factory=dict)"

    if field.label == FieldDescriptorProto.LABEL_REPEATED:
        element = _element_annotation(field)
        return f"{field.name}: list[{element}] = field(default_factory=list)"

    if field.type == FieldDescriptorProto.TYPE_MESSAGE:
        # Message fields always carry presence.
        return f'{field.name}: "{_short_name(field.type_name)} | None" = None'

    if field.type == FieldDescriptorProto.TYPE_ENUM:
        annotation = _short_name(field.type_name)
        default = f"{annotation}(0)"
    else:
        annotation, default = _SCALAR[field.type]

    if _has_presence(field):
        return f"{field.name}: {annotation} | None = None"
    return f"{field.name}: {annotation} = {default}"


def _render_enum(enum: EnumDescriptorProto) -> str:
    """Render an enum as an ``IntEnum`` subclass."""
    body = [f"    {value.name} = {value.number}" for value in enum.value]
    return "\n".join([f"class {enum.name}(IntEnum):", *(body or ["    pass"])])


def _render_message(msg: DescriptorProto, index: dict[str, DescriptorProto]) -> str:
    """Render a message as its descriptor constant plus a decorated dataclass."""
    const = f"_{msg.name.upper()}_DESCRIPTOR"
    descriptor_hex = msg.SerializeToString().hex()
    fields = [f"    {_render_field(f, index)}" for f in msg.field]
    return "\n".join(
        [
            f"{const} = bytes.fromhex(  # @generated",
            f'    "{descriptor_hex}"',
            ")",
            "",
            "",
            f"@message({const})",
            "@dataclass(slots=True)",
            f"class {msg.name}(Message):",
            *(fields or ["    pass"]),
        ],
    )


def _render_header(file: FileDescriptorProto) -> str:
    """Render the module header: banner and imports."""
    needs_field = any(
        f.label == FieldDescriptorProto.LABEL_REPEATED
        for msg in file.message_type
        for f in msg.field
    )
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
    if file.enum_type:
        lines.append("from enum import IntEnum")
    lines += ["", "from fastproto import Message, Scalar, message"]
    return "\n".join(lines)


def _generate_file(file: FileDescriptorProto) -> str:
    """Render the full ``<name>_pb.py`` source for one proto file."""
    index = _index_messages(file)
    blocks = [_render_header(file)]
    blocks += [_render_enum(enum) for enum in file.enum_type]
    blocks += [
        _render_message(msg, index)
        for msg in file.message_type
        if not msg.options.map_entry  # synthetic map entries are not user-facing
    ]
    return "\n\n\n".join(blocks) + "\n"


def _output_name(proto_name: str) -> str:
    """Map ``foo/bar.proto`` to ``foo/bar_pb.py``."""
    return f"{proto_name.removesuffix('.proto')}_pb.py"


def generate(
    request: plugin_pb2.CodeGeneratorRequest,
) -> plugin_pb2.CodeGeneratorResponse:
    """Turn a protoc request into a response of generated ``_pb.py`` files."""
    response = plugin_pb2.CodeGeneratorResponse()
    response.supported_features = (
        plugin_pb2.CodeGeneratorResponse.FEATURE_PROTO3_OPTIONAL
    )
    files_by_name = {f.name: f for f in request.proto_file}
    for proto_name in request.file_to_generate:
        output = response.file.add()
        output.name = _output_name(proto_name)
        output.content = _generate_file(files_by_name[proto_name])
    return response


def main() -> None:
    """Read a request from stdin and write the response to stdout."""
    request = plugin_pb2.CodeGeneratorRequest.FromString(sys.stdin.buffer.read())
    sys.stdout.buffer.write(generate(request).SerializeToString())


if __name__ == "__main__":
    main()
