# FastProto

A protobuf library for Python (PyPI: `fastproto`). It generates **readable
`@dataclass(slots=True)`** classes with honest type annotations instead of
google's opaque classes; all serialization lives in a **Rust core** (pyo3 +
maturin, abi3-py312, Python ≥3.12). Data lives in plain Python objects: reading a
field is pure Python, Rust is only involved in `to_bytes()` / `from_bytes()`.
Wire compatibility with google-protobuf is verified by tests in both directions.

## How it works

1. `protoc` + the `protoc-gen-fastproto` plugin → `*_pb.py`: a dataclass plus its
   schema as a hex `DescriptorProto` (`_X_DESCRIPTOR = bytes.fromhex(...)`). The
   schema comes from the descriptor, NOT from the annotations (they carry no field
   numbers).
2. The `@message(...)` decorator compiles the descriptor into Rust (`Descriptor`)
   once.
3. On the first `to_bytes`/`from_bytes`, `_ensure_linked` resolves references to
   other classes (enums/messages, including nested and cyclic ones) and enables the
   fast-init path.
4. From then on, encode/decode run entirely in Rust.

## Code map

```
src/                  Rust; every module is a folder: mod.rs + tests.rs
  wire/               wire-format primitives (varint, zigzag, tags, lengths)
  descriptor/         internal schema model + FieldIndex (field by number in O(1))
  parse/              hand-written DescriptorProto parser
  encode/ decode/     the codec (hot paths, depth limit, unknown fields, open enums, oneof)
  wellknown/          Timestamp <-> datetime, Duration <-> timedelta
  message/            the Descriptor pyclass: linking, fast-init, caches
  lib.rs              the _core module
python/fastproto/
  __init__.py         Message, @message, Scalar, _ensure_linked, _fast_init_defaults
  plugin/             code generator: _index (types), _render (output), _validate (schema)
  wellknown.py        @generated structural WKTs (Any, Struct, wrappers, ...)
  _core.pyi           stubs for the Rust module
tests/                pytest; protos/ → fixtures/*.fds → generated/*_pb.py (committed)
bench/compare.py      benchmark against google-protobuf (upb)
scripts/regen.py      regenerates fixtures and wellknown.py
```

`CONTRIBUTING.md` is for humans (setup, commits, releases); its "Project layout"
section is outdated (still mentions `plugin.py`).

## Commands

The environment is `.venv` (created by `uv sync`). Run the full gate with the
`/gate` skill.

```bash
VIRTUAL_ENV=.venv .venv/bin/maturin develop      # rebuild Rust after changes in src/
uv run cargo test                                  # via uv: plain cargo may pick a system
uv run cargo clippy --all-targets -- -D warnings   #   Python < 3.12 and fail in pyo3
cargo fmt --check                                  # rustfmt is enforced in CI
.venv/bin/ruff check python tests scripts
.venv/bin/ruff format --check python tests scripts
.venv/bin/ty check
.venv/bin/python -m pytest tests/ -q
```

Fixture regeneration: `/regen`. Benchmark: `/bench`.

## Code rules

- **Tests go through the public API only** (`to_bytes`, `from_bytes`, generated
  classes). White-box tests belong in Rust, in the `tests.rs` next to the module.
- Order within files and classes: public first, private below (Rust too).
- Python: ruff `select = ["ALL"]`, ty strict. Type-only imports go under
  `if TYPE_CHECKING:`. The `ruff format` warning about the COM812 conflict is
  expected.
- Rust: `clippy -D warnings`, `cargo fmt`.
- **Never hand-edit generated code** (`tests/generated/`, `wellknown.py`) — change
  the `.proto` or the plugin and run `/regen`. Golden tests compare plugin output
  byte for byte.
- New `.proto` fixture → add a unit to `UNITS` in `scripts/regen.py`.
- **Do not change the version in `Cargo.toml`** — it lives in git tags; CI
  (`version-guard`) rejects the PR.
- PR title is a Conventional Commit (`feat:`, `fix(decode):`, `perf:` ...); merges
  are squash-only.

## Invariants and pitfalls

- **Stale `.so`.** If `python/fastproto/` contains a `_core.cpython-*.so`, Python
  imports it instead of `_core.abi3.so` and Rust changes seem to have no effect.
  Delete the extra file.
- **protoc version.** Fixtures were generated with protoc **35.1**; a different
  version changes the descriptor hex in every fixture. Don't regenerate with
  another version without a reason.
- **The decode fast-init path** (`object.__new__` + setattr, bypassing `__init__`)
  is enabled only when `_fast_init_defaults` confirms a "plain" generated dataclass.
  Any change to the base `Message` (new `__post_init__`, slots) or to the generator
  (custom `__init__`, field order ≠ descriptor) must preserve the invariant —
  otherwise `_fast_init_defaults` must return `None` (slow but correct path).
- **`byte_slice` uses `unsafe` for `bytearray`** — sound only while the slice is
  consumed immediately, with no Python code running in between (which could resize
  or free the bytearray).
- **`_fastproto_unknown`** is a slot on `Message`, NOT a dataclass field. Never
  give it an annotation in the class body.
- `FieldDescriptor.type_name` is the full name (`pkg.Outer.Inner`); the Python
  `_resolve` strips package segments and walks into nested classes.
- `MAX_DEPTH = 100` on both decode and encode (on encode it catches object cycles).
- Message/enum field annotations in generated code are quoted (otherwise forward
  references and cycles raise NameError on Python ≤3.13).
- abi3 (limited API) gives no access to slot layout — that caps how far
  optimizations can go.

## Deliberately NOT doing

- Storing data in a Rust struct (`#[pyclass]`, the upb model) — it would bring
  back the opaque wrappers this project exists to avoid. Codec speed yields to
  ergonomics.
- Merging singular message fields on decode (documented in the README).
- proto2 (the plugin rejects it), gRPC/services.

## Where we left off

Each developer has their own `.claude/SESSION.md` (not in git) — a note about the
work in progress, written by the `/handoff` skill. If the file doesn't exist, this
section is empty.

@.claude/SESSION.md
