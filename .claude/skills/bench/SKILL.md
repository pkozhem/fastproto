---
name: bench
description: Benchmark FastProto against google-protobuf (upb) with bench/compare.py and compare against the numbers in the README. Use when asked to "measure speed", "benchmark", "did it get faster/slower", and after changes to hot paths in src/encode, src/decode, src/wire, src/message.
---

# Bench: performance measurement

## Steps

1. **Release build.** A plain `maturin develop` produces a debug build — numbers
   from it are meaningless. Before measuring:
   ```bash
   VIRTUAL_ENV=.venv .venv/bin/maturin develop --release
   ```
   (the release profile uses thin LTO + `codegen-units=1`, so it builds slower —
   that's normal).

2. **Measure:**
   ```bash
   .venv/bin/python bench/compare.py
   ```
   It measures `rich.User` (~509 B) in 4 scenarios: decode / decode + read every
   field / read every field of an already-decoded message / encode. For
   stability, run it twice and take the best result; if the spread is large, say
   so.

3. **Compare with the README** (Performance section, Apple M-series, CPython 3.14):

   | scenario | fastproto | upb |
   |---|---|---|
   | decode | 4.3 µs | 1.9 µs |
   | decode + read all | 5.9 µs | 8.9 µs |
   | read all (decoded) | 1.5 µs | 6.7 µs |
   | encode | 2.4 µs | 0.9 µs |

   Report: a "before (README) → now" table for fastproto with the % change, and
   separately the fastproto/upb ratio. Absolute numbers depend on the machine, so
   the ratio to upb matters more than the µs.

4. To validate an optimization: measure BEFORE the change (on a clean `main`) and
   AFTER, back to back on the same machine.

5. Don't update the README unless the user asks.

6. Afterwards you may switch back to a regular build (`maturin develop` without
   `--release`), but it's optional — tests pass on both.
