"""Benchmark fastproto against google's protobuf runtime (upb backend).

Run from the repository root (needs the dev environment plus ``protobuf``):

    uv run --with protobuf python bench/compare.py

Measures the same mid-size message (strings, nested messages, maps, repeated
fields, enums — ``rich.User``) through both runtimes, in the scenarios that
matter: raw decode, decode followed by reading every field, reading fields of
an already-decoded message, and encode. See the Performance section of the
README for why the scenarios are separated.
"""

import sys
import time
from pathlib import Path

ROOT = Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "tests"))

from generated.rich_pb import Address, Role, User


def build_google_user_class():
    from google.protobuf import descriptor_pb2, descriptor_pool, message_factory

    fileset = descriptor_pb2.FileDescriptorSet.FromString(
        (ROOT / "tests" / "fixtures" / "rich.fds").read_bytes(),
    )
    pool = descriptor_pool.DescriptorPool()
    for file in fileset.file:
        pool.Add(file)
    return message_factory.GetMessageClass(pool.FindMessageTypeByName("rich.User"))


def bench(fn, n=5000, repeat=5):
    best = float("inf")
    for _ in range(repeat):
        t0 = time.perf_counter()
        for _ in range(n):
            fn()
        best = min(best, (time.perf_counter() - t0) / n * 1e6)
    return best


def read_all(m):  # identical duck-typed reads for both libraries
    s = m.id
    s += len(m.name)
    s += len(m.phone or "")
    s += int(m.role)
    s += sum(len(t) for t in m.tags)
    s += sum(m.scores)
    s += len(m.address.city) + len(m.address.street)
    s += sum(len(a.city) for a in m.past_addresses)
    s += sum(m.counters.values())
    s += sum(len(v.city) for v in m.places.values())
    s += sum(int(r) for r in m.roles)
    return s


def main() -> None:
    try:
        from google.protobuf.internal import api_implementation
    except ImportError:
        sys.exit("google protobuf is not installed; run with `--with protobuf`")

    user = User(
        id=42,
        name="Ada Lovelace",
        email="ada@example.com",
        role=Role.ROLE_ADMIN,
        tags=["vip", "beta", "early-adopter", "x" * 40],
        scores=[1, 2, 3, 500, 70000, -1, 2**31 - 1] * 4,
        address=Address(city="London", street="Baker St"),
        past_addresses=[Address(city=f"C{i}", street=f"S{i}") for i in range(8)],
        counters={f"k{i}": i * 7 for i in range(16)},
        places={f"p{i}": Address(city=f"C{i}") for i in range(4)},
        roles=[Role.ROLE_USER] * 10,
        phone="+44 20 7946 0958",
    )
    data = user.to_bytes()

    g_cls = build_google_user_class()
    g_user = g_cls.FromString(data)
    # Both runtimes must see identical values before timing anything.
    assert read_all(User.from_bytes(data)) == read_all(g_user)

    print(f"google backend: {api_implementation.Type()}   payload: {len(data)} B")
    print(f"{'scenario':<42}{'fastproto':>12}{'google':>12}")

    fp_decoded = User.from_bytes(data)
    rows = [
        ("decode", lambda: User.from_bytes(data), lambda: g_cls.FromString(data)),
        (
            "decode + read every field",
            lambda: read_all(User.from_bytes(data)),
            lambda: read_all(g_cls.FromString(data)),
        ),
        (
            "read every field, already decoded",
            lambda: read_all(fp_decoded),
            lambda: read_all(g_user),
        ),
        ("encode", user.to_bytes, g_user.SerializeToString),
    ]
    for name, fp, gp in rows:
        print(f"{name:<42}{bench(fp):>9.2f} us{bench(gp):>9.2f} us")


if __name__ == "__main__":
    main()
