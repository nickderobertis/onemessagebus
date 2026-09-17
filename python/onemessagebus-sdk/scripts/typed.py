"""Hold a built wheel to the `Typing :: Typed` promise its manifest makes.

The manifest's classifier tells a consumer's type checker that the package's own
annotations are the types; the wheel keeps that promise only when it ships an empty
`onemessagebus/py.typed`, without which every SDK name reads as `Any`. This script
reads the wheel as the zip it is and refuses when the marker is missing or non-empty,
or when its METADATA carries no `Classifier: Typing :: Typed` — over the built wheel,
never the source tree, because only the wheel's contents say what a release ships.

    python scripts/typed.py dist/packages/onemessagebus-X.Y.Z-py3-none-any.whl

`scripts/build` runs it over the wheel `just python-sdk-dist` builds.
"""

from __future__ import annotations

import argparse
import sys
import zipfile
from pathlib import Path
from typing import NoReturn

MARKER = "onemessagebus/py.typed"
CLASSIFIER = "Classifier: Typing :: Typed"
REBUILD = "then build again"


def fail(problem: str, action: str) -> NoReturn:
    print(f"typed.py: {problem}\n  fix: {action}", file=sys.stderr)
    raise SystemExit(1)


def read_wheel(wheel: Path) -> tuple[dict[str, int], list[str]]:
    """The wheel's members by size, and the lines of its METADATA."""
    try:
        with zipfile.ZipFile(wheel) as archive:
            members = {info.filename: info.file_size for info in archive.infolist()}
            metadata = next(
                (name for name in members if name.endswith(".dist-info/METADATA")), None
            )
            if metadata is None:
                fail(
                    f"{wheel.name} holds no .dist-info/METADATA, so it is not a wheel uv built",
                    f"rerun `just python-sdk-dist` into an empty directory, {REBUILD}",
                )
            lines = archive.read(metadata).decode("utf-8").splitlines()
    except (OSError, zipfile.BadZipFile, UnicodeDecodeError) as error:
        fail(
            f"cannot read {wheel} as a wheel: {error!r}",
            f"rerun `just python-sdk-dist` into an empty directory, {REBUILD}",
        )
    return members, lines


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("wheel", type=Path, help="the built wheel to hold to the classifier")
    args = parser.parse_args(argv)
    members, metadata = read_wheel(args.wheel)
    size = members.get(MARKER)
    if size is None:
        fail(
            f"{args.wheel.name} ships no {MARKER}, so a consumer's type checker reads every "
            "name as Any",
            f"restore the empty marker at src/onemessagebus/py.typed, {REBUILD}",
        )
    if size != 0:
        fail(
            f"{args.wheel.name}'s {MARKER} holds {size} bytes where the marker is empty",
            f"empty src/onemessagebus/py.typed, {REBUILD}",
        )
    if CLASSIFIER not in metadata:
        fail(
            f"{args.wheel.name}'s METADATA carries no `{CLASSIFIER}`",
            f'restore "Typing :: Typed" under [project].classifiers in pyproject.toml, {REBUILD}',
        )
    print(f"typed.py: {args.wheel.name} ships {MARKER} and declares Typing :: Typed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
