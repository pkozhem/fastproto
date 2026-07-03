"""Rewrite the ``[package]`` version in Cargo.toml.

CI release helper: the release workflow sets the version on the runner just
before building, so the bump never has to be committed to ``main`` (the version
lives in git tags). Usage: ``python scripts/set_version.py 1.2.3``.
"""

import re
import sys
from pathlib import Path


def main() -> None:
    """Set Cargo.toml's version to the value passed as the first argument."""
    version = sys.argv[1]
    path = Path("Cargo.toml")
    updated, count = re.subn(
        r'^version = ".*"',
        f'version = "{version}"',
        path.read_text(),
        count=1,
        flags=re.MULTILINE,
    )
    if count != 1:
        sys.exit("Cargo.toml: no version line to replace")
    path.write_text(updated)


if __name__ == "__main__":
    main()
