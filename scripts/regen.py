"""Regenerate the committed test fixtures.

Dev-only helper (never imported by the test suite). Runs protoc to produce the
``FileDescriptorSet`` fixtures, then drives the plugin in-process to (re)write
``tests/generated/*_pb.py`` and the raw ``DescriptorProto`` fixture. Run it
after changing a ``.proto`` under ``tests/protos`` or the code generator:

    python scripts/regen.py
"""

import subprocess
import sys
from pathlib import Path

from google.protobuf.compiler import plugin_pb2
from google.protobuf.descriptor_pb2 import FileDescriptorSet

from fastproto import plugin

ROOT = Path(__file__).resolve().parent.parent
PROTOS = ROOT / "tests" / "protos"
FIXTURES = ROOT / "tests" / "fixtures"
GENERATED = ROOT / "tests" / "generated"

PROTO_FILES = ["rich.proto", "scalars.proto"]


def _fds_path(proto: str) -> Path:
    return FIXTURES / f"{proto.removesuffix('.proto')}.fds"


def _run_protoc(proto: str) -> None:
    subprocess.run(
        [
            "protoc",
            f"--proto_path={PROTOS}",
            f"--descriptor_set_out={_fds_path(proto)}",
            proto,
        ],
        check=True,
    )


def main() -> None:
    """Regenerate every committed fixture from the ``.proto`` sources."""
    FIXTURES.mkdir(exist_ok=True)
    GENERATED.mkdir(exist_ok=True)
    (GENERATED / "__init__.py").write_text("")

    for proto in PROTO_FILES:
        _run_protoc(proto)
        fileset = FileDescriptorSet.FromString(_fds_path(proto).read_bytes())
        request = plugin_pb2.CodeGeneratorRequest(
            file_to_generate=[proto],
            proto_file=fileset.file,
        )
        for generated in plugin.generate(request).file:
            (GENERATED / Path(generated.name).name).write_text(generated.content)

    sys.stdout.write("Regenerated fixtures under tests/.\n")


if __name__ == "__main__":
    main()
