"""Golden tests for the protoc plugin, run in-process (no subprocess).

Builds a ``CodeGeneratorRequest`` from a committed ``FileDescriptorSet`` and
asserts the plugin reproduces the committed ``tests/generated/*_pb.py`` exactly.
If the generator changes, regenerate the fixtures with ``scripts/regen.py``.
"""

from pathlib import Path

import pytest
from google.protobuf.compiler.plugin_pb2 import (
    CodeGeneratorRequest,
    CodeGeneratorResponse,
)
from google.protobuf.descriptor_pb2 import FileDescriptorProto, FileDescriptorSet

from fastproto import plugin

FIXTURES = Path(__file__).parent / "fixtures"
GENERATED = Path(__file__).parent / "generated"

# (fds fixture, files generated from it) — mirrors scripts/regen.py.
UNITS = [
    ("rich", ["rich.proto"]),
    ("scalars", ["scalars.proto"]),
    ("tree", ["tree.proto"]),
    ("nested", ["nested.proto"]),
    ("profile", ["common.proto", "profile.proto"]),
    ("event", ["event.proto"]),
    ("wkt", ["wkt.proto"]),
]


def _request(unit: str, to_generate: list[str]) -> CodeGeneratorRequest:
    fileset = FileDescriptorSet.FromString((FIXTURES / f"{unit}.fds").read_bytes())
    return CodeGeneratorRequest(file_to_generate=to_generate, proto_file=fileset.file)


@pytest.mark.parametrize(("unit", "protos"), UNITS)
def test_generates_committed_output(unit: str, protos: list[str]) -> None:
    response = plugin.generate(_request(unit, protos))
    assert not response.error
    assert len(response.file) == len(protos)

    for proto, generated in zip(protos, response.file, strict=True):
        expected_name = f"{proto.removesuffix('.proto')}_pb.py"
        assert generated.name == expected_name
        assert generated.content == (GENERATED / expected_name).read_text()


def test_wellknown_module_matches_generator() -> None:
    fileset = FileDescriptorSet.FromString((FIXTURES / "wellknown.fds").read_bytes())
    committed = Path(plugin.__file__).with_name("wellknown.py").read_text()
    assert plugin.generate_wellknown(fileset.file) == committed


def test_declares_proto3_optional_support() -> None:
    response = plugin.generate(_request("scalars", ["scalars.proto"]))
    feature = CodeGeneratorResponse.FEATURE_PROTO3_OPTIONAL
    assert response.supported_features & feature


def test_short_name_collision_is_reported() -> None:
    # Two files both define `Clash`; a third imports the name from each — the
    # annotations couldn't tell them apart, so the plugin must refuse.
    def file_with_message(name: str, package: str) -> FileDescriptorProto:
        file = FileDescriptorProto(name=name, package=package, syntax="proto3")
        file.message_type.add(name="Clash")
        return file

    a = file_with_message("a.proto", "pa")
    b = file_with_message("b.proto", "pb")
    user = FileDescriptorProto(name="user.proto", package="user", syntax="proto3")
    msg = user.message_type.add(name="User")
    msg.field.add(name="x", number=1, type=11, type_name=".pa.Clash", label=1)
    msg.field.add(name="y", number=2, type=11, type_name=".pb.Clash", label=1)

    request = CodeGeneratorRequest(
        file_to_generate=["user.proto"],
        proto_file=[a, b, user],
    )
    response = plugin.generate(request)
    assert "Clash" in response.error
    assert not response.file


def _single_field_request(
    *, syntax: str = "proto3", field_name: str = "value"
) -> CodeGeneratorRequest:
    file = FileDescriptorProto(name="m.proto", package="m", syntax=syntax)
    msg = file.message_type.add(name="M")
    msg.field.add(name=field_name, number=1, type=9, label=1)  # string
    return CodeGeneratorRequest(file_to_generate=["m.proto"], proto_file=[file])


def test_keyword_field_name_is_reported() -> None:
    response = plugin.generate(_single_field_request(field_name="class"))
    assert "class" in response.error
    assert not response.file


def test_reserved_field_name_is_reported() -> None:
    # A field shadowing the Message API would break to_bytes() at runtime.
    response = plugin.generate(_single_field_request(field_name="to_bytes"))
    assert "to_bytes" in response.error
    assert not response.file


def test_keyword_enum_value_is_reported() -> None:
    file = FileDescriptorProto(name="e.proto", package="e", syntax="proto3")
    enum = file.enum_type.add(name="E")
    enum.value.add(name="None", number=0)  # `None` is a Python keyword
    request = CodeGeneratorRequest(file_to_generate=["e.proto"], proto_file=[file])
    response = plugin.generate(request)
    assert "None" in response.error
    assert not response.file


def test_non_proto3_is_reported() -> None:
    response = plugin.generate(_single_field_request(syntax="proto2"))
    assert "proto3" in response.error
    assert not response.file
