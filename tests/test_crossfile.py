"""Cross-file `.proto` imports: generation, linking, and round-trips.

`profile.proto` imports `common.proto` and references its types every way a
field can: single message, repeated message, map value, and enum. The runtime
linker resolves the imported short names from the generated module's namespace.
"""

from pathlib import Path
from typing import Any

import pytest

from tests.generated.common_pb import Address, Country
from tests.generated.profile_pb import Profile

FIXTURES = Path(__file__).parent / "fixtures"


def test_generated_imports() -> None:
    src = (Path(__file__).parent / "generated" / "profile_pb.py").read_text()
    assert "from .common_pb import Address, Country" in src
    assert "from common_pb import Address, Country" in src  # flat-layout fallback


def test_crossfile_roundtrip() -> None:
    profile = Profile(
        name="P",
        home=Address(city="Prague", street="Karlova", country=Country.COUNTRY_CZ),
        past_homes=[Address(city="Dresden", country=Country.COUNTRY_DE)],
        places={"office": Address(city="Brno")},
        citizenship=Country.COUNTRY_CZ,
    )
    back = Profile.from_bytes(profile.to_bytes())
    assert back == profile
    assert isinstance(back.home, Address)
    assert isinstance(back.home.country, Country)
    assert isinstance(back.places["office"], Address)


def test_crossfile_wire_compatible_with_reference() -> None:
    pytest.importorskip("google.protobuf")
    from google.protobuf import descriptor_pb2, descriptor_pool, message_factory

    fileset = descriptor_pb2.FileDescriptorSet.FromString(
        (FIXTURES / "profile.fds").read_bytes(),
    )
    pool = descriptor_pool.DescriptorPool()
    for file in fileset.file:
        pool.Add(file)
    ref_cls = message_factory.GetMessageClass(
        pool.FindMessageTypeByName("profile.Profile"),
    )

    profile = Profile(
        name="x",
        home=Address(city="C", country=Country.COUNTRY_DE),
        places={"h": Address(city="H")},
    )
    ref: Any = ref_cls()  # reflective google message is dynamically typed
    ref.ParseFromString(profile.to_bytes())
    assert ref.name == "x"
    assert ref.home.city == "C"
    assert ref.home.country == Country.COUNTRY_DE
    assert ref.places["h"].city == "H"

    assert Profile.from_bytes(ref.SerializeToString()) == profile
