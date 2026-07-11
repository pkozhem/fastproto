"""Regenerate the committed test fixtures.

Dev-only helper (never imported by the test suite). Runs protoc to produce the
``FileDescriptorSet`` fixtures, then drives the plugin in-process to (re)write
``tests/generated/*_pb.py``. Run it after changing a ``.proto`` under
``tests/protos`` or the code generator:

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
WELLKNOWN_PY = ROOT / "python" / "fastproto" / "wellknown.py"

# One unit = one .fds fixture (with the full import closure) plus the proto
# files generated from it. Multi-file units exercise cross-file imports.
UNITS: list[tuple[str, list[str]]] = [
    ("rich", ["rich.proto"]),
    ("scalars", ["scalars.proto"]),
    ("tree", ["tree.proto"]),
    ("nested", ["nested.proto"]),
    ("profile", ["common.proto", "profile.proto"]),
    ("event", ["event.proto"]),
    ("wkt", ["wkt.proto"]),
]


def _run_protoc(fds: Path, protos: list[str]) -> None:
    subprocess.run(
        [
            "protoc",
            f"--proto_path={PROTOS}",
            "--include_imports",
            f"--descriptor_set_out={fds}",
            *protos,
        ],
        check=True,
    )


def _generate(fds: Path, to_generate: list[str]) -> None:
    fileset = FileDescriptorSet.FromString(fds.read_bytes())
    request = plugin_pb2.CodeGeneratorRequest(
        file_to_generate=to_generate,
        proto_file=fileset.file,
    )
    response = plugin.generate(request)
    if response.error:
        sys.exit(f"plugin error: {response.error}")
    for generated in response.file:
        (GENERATED / Path(generated.name).name).write_text(generated.content)


def _regen_wellknown() -> None:
    """(Re)write the bundled ``fastproto/wellknown.py`` and its .fds fixture.

    protoc resolves ``google/protobuf/*.proto`` from its own bundled include
    path, so no ``--proto_path`` is needed.
    """
    fds = FIXTURES / "wellknown.fds"
    subprocess.run(
        [
            "protoc",
            "--include_imports",
            f"--descriptor_set_out={fds}",
            *plugin.WELLKNOWN_PROTOS,
        ],
        check=True,
    )
    fileset = FileDescriptorSet.FromString(fds.read_bytes())
    WELLKNOWN_PY.write_text(plugin.generate_wellknown(fileset.file))


def main() -> None:
    """Regenerate every committed fixture from the ``.proto`` sources."""
    FIXTURES.mkdir(exist_ok=True)
    GENERATED.mkdir(exist_ok=True)
    (GENERATED / "__init__.py").write_text("")

    for unit, protos in UNITS:
        fds = FIXTURES / f"{unit}.fds"
        _run_protoc(fds, protos)
        _generate(fds, protos)

    _regen_wellknown()
    sys.stdout.write("Regenerated fixtures under tests/ and fastproto/wellknown.py.\n")


if __name__ == "__main__":
    main()
