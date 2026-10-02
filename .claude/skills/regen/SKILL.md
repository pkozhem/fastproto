---
name: regen
description: Regenerate FastProto test fixtures (tests/fixtures/*.fds, tests/generated/*_pb.py) and python/fastproto/wellknown.py via scripts/regen.py. Use after editing a .proto in tests/protos/ or the plugin code (python/fastproto/plugin/), or when golden tests in test_plugin.py fail.
---

# Regen: regenerate fixtures

## Steps

1. **protoc version.** `protoc --version`. Fixtures were generated with
   **protoc 35.1**. If the version differs — STOP and warn the user: another
   version changes the descriptor hex in every fixture (noise in the diff).
   Continue only with their agreement.

2. **New `.proto`?** If a new file was added to `tests/protos/`, first add a unit
   to the `UNITS` list in `scripts/regen.py` (one `.fds` = a group of related
   protos).

3. **Regenerate:**
   ```bash
   .venv/bin/python scripts/regen.py
   ```
   If the plugin returns an error (`plugin error: ...`), the schema validator
   (`plugin/_validate.py`) rejected the `.proto`; investigate, don't work around it.

4. **Check the diff:**
   ```bash
   git status --short tests/ python/fastproto/wellknown.py
   git diff --stat tests/ python/fastproto/wellknown.py
   ```
   Changes should appear only where expected: in the fixtures of the edited
   `.proto` files, or in every generated file if the plugin output changed. Hex
   changes in fixtures nobody touched mean protoc version drift — report it to the
   user.

5. The script is idempotent: a second run without changes must not modify any
   files.

6. Run `/gate` (at minimum `.venv/bin/python -m pytest tests/ -q`; the goldens
   live in `tests/test_plugin.py`).
