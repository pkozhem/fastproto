# Contributing to FastProto

Thanks for your interest in contributing! FastProto is a Rust + Python project:
the performance-critical core is written in Rust (via [PyO3](https://pyo3.rs)),
and a thin, fully-typed Python layer plus a `protoc` code-generator plugin sit
on top. This guide covers how to set up, develop, and submit changes.

## Prerequisites

- **[uv](https://docs.astral.sh/uv/)** — manages the Python environment and dev tools.
- **A Rust toolchain** (stable) — <https://rustup.rs>. Needed to build the native extension.
- **Python 3.12+** — uv can install it for you (`uv python install 3.12`).
- **`protoc`** — *only* needed to regenerate test fixtures (see below). Not required
  for normal development, since generated fixtures are committed.

## Getting started

```bash
uv sync                 # create the venv, install dev tools, build the extension
uv run maturin develop  # (re)build the Rust extension into the venv after Rust changes
```

`uv sync` installs everything from the `dev` dependency group (maturin, pytest,
ruff, ty, protobuf) and builds `fastproto._core`.

## Project layout

```
src/                     Rust core (one folder per module: mod.rs + tests.rs)
  wire/                  protobuf wire-format primitives
  descriptor/            compiled message descriptor model
  parse/                 DescriptorProto parser
  encode/ decode/        the codec
  message/               the `Descriptor` pyclass exposed to Python
python/fastproto/        Python package (public API + `_core.pyi` stub)
  plugin.py              the protoc code generator (`protoc-gen-fastproto`)
tests/                   pytest suite (+ committed fixtures under tests/generated)
scripts/                 dev helpers (fixture regeneration, release version bump)
```

## Development workflow

Run the same gate that CI runs before opening a PR:

```bash
uv run cargo test                          # Rust unit tests
uv run ruff check python tests scripts     # lint
uv run ruff format --check python tests scripts
uv run ty check                            # type check
uv run pytest                              # Python tests
```

All of the above must be green. Linting is strict (`ruff` with `select = ["ALL"]`)
and type checking is strict (`ty`); generated files under `tests/generated` are
excluded from linting.

## Regenerating generated code and fixtures

The `.proto` fixtures and their generated `*_pb.py` files under `tests/` are
committed. If you change a `.proto` in `tests/protos/` or the code generator in
`python/fastproto/plugin.py`, regenerate them (requires `protoc` on your PATH):

```bash
uv run python scripts/regen.py
```

The golden test in `tests/test_plugin.py` will fail if the committed output and
the generator disagree, so always regenerate after touching the plugin.

## Commit messages

We follow [Conventional Commits](https://www.conventionalcommits.org). PRs are
**squash-merged**, and the squash commit takes the **PR title** as its message —
so your **PR title must be a Conventional Commit**. (The PR title defaults to the
branch's first commit, so writing that commit conventionally is usually enough.)
This is enforced in CI by the required **`pr-title`** check — a non-conforming
title blocks the merge.

Format: `type(optional scope): summary`. For example:

- `feat: <a new capability>`
- `fix(decode): <a bug fix, optionally scoped>`
- `docs: <documentation change>`
- `refactor(parse): <internal change, no behavior change>`
- `test: <added or changed tests>`
- `chore(ci): <tooling / maintenance>`

Common types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`,
`chore`. [Commitizen](https://commitizen-tools.github.io/commitizen/) is in the
dev tools to help write and check messages:

```bash
uv run cz commit           # interactive prompt for a compliant message
uv run cz check --rev HEAD # validate the latest commit
```

## Pull requests

- `main` is protected: **no direct pushes** — everything goes through a PR.
- Branch, commit, push, and open a PR:
  ```bash
  git switch -c my-change
  # ... changes ...
  git push -u origin my-change
  gh pr create --fill
  ```
- Give the PR a **Conventional Commit title** (see above) — it becomes the
  squashed commit message on `main`.
- The **`tests-pass`** check must be green before a PR can be merged (it runs the
  full gate on Python 3.12 and 3.14).
- **Do not change the version in `Cargo.toml`.** A dedicated check (`version-guard`)
  will fail your PR if you do — releases own the version (see below).
- Keep changes focused and matching the surrounding style.

## Releases (maintainers)

Versioning is automated and the version lives in **git tags**, not in a committed
`Cargo.toml` bump. To cut a release:

1. Go to **Actions → release → Run workflow**.
2. Pick the bump type: `patch`, `minor`, or `major`.
3. The workflow computes the next version from the latest tag, builds wheels for
   Linux (x86_64 / aarch64), Windows (x64), macOS (universal2), and an sdist,
   publishes them to PyPI, and creates the git tag and GitHub Release.

No local `maturin publish` and no manual tagging are needed.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
