"""Golden test for the protoc plugin, run in-process (no subprocess).

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
from google.protobuf.descriptor_pb2 import FileDescriptorSet

from fastproto import plugin

FIXTURES = Path(__file__).parent / "fixtures"
GENERATED = Path(__file__).parent / "generated"


def _request(proto: str) -> CodeGeneratorRequest:
    fileset = FileDescriptorSet.FromString(
        (FIXTURES / f"{proto.removesuffix('.proto')}.fds").read_bytes(),
    )
    return CodeGeneratorRequest(file_to_generate=[proto], proto_file=fileset.file)


@pytest.mark.parametrize("proto", ["rich.proto", "scalars.proto"])
def test_generates_committed_output(proto: str) -> None:
    response = plugin.generate(_request(proto))
    assert len(response.file) == 1
    generated = response.file[0]

    expected_name = f"{proto.removesuffix('.proto')}_pb.py"
    assert generated.name == expected_name

    committed = (GENERATED / expected_name).read_text()
    assert generated.content == committed


def test_declares_proto3_optional_support() -> None:
    response = plugin.generate(_request("scalars.proto"))
    assert response.supported_features & CodeGeneratorResponse.FEATURE_PROTO3_OPTIONAL
