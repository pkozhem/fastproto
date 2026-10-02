---
name: gate
description: Run the full FastProto check suite — the same as CI (cargo test, clippy, rustfmt, ruff, ty, pytest) — plus a rebuild of the Rust module. Use when asked to "run the checks", "gate", "is everything green", "ready for a PR?", and on your own before saying a task is done.
---

# Gate: all checks before a PR

The goal is an honest "green / here's what failed". Run ALL checks without
stopping at the first failure, then give a short summary.

## Steps

1. **Stale `.so`.** `ls python/fastproto/*.so`. Only `_core.abi3.so` should be
   there. If there's a `_core.cpython-*.so`, delete it and tell the user
   (otherwise Python imports the old build).

2. **Rebuild the Rust module** (if anything changed in `src/` or `Cargo.toml`, or
   you're not sure):
   ```bash
   VIRTUAL_ENV=.venv .venv/bin/maturin develop
   ```
   If the build fails, the Python checks are pointless — report it right away.

3. **Checks** — one after another, each to completion, even if a previous one
   failed:
   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   .venv/bin/ruff check python tests scripts
   .venv/bin/ruff format --check python tests scripts
   .venv/bin/ty check
   .venv/bin/python -m pytest tests/ -q
   ```
   Expected noise, NOT an error: `ruff format` warns that the COM812 rule
   conflicts with the formatter (exit code 0).

4. **Report** as a table:

   | check | result |
   |---|---|
   | rustfmt | ✅ |
   | clippy | ❌ 2 warnings |
   | cargo test | ✅ 35 passed |
   | ... | ... |

   For each ❌ — 1–3 lines: file:line and the gist, no walls of logs.

## Fixing

- Don't fix anything if the user only asked for a check.
- If asked to "get it green": formatting can be applied right away (`cargo fmt`,
  `.venv/bin/ruff format python tests scripts`); fix everything else properly and
  rerun the gate.
- Never "fix" a test by bending its expectation to new behavior without the
  user's agreement.
