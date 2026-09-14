"""Assemble a publishable copy of the SDK, every placeholder version stamped from Cargo.toml.

The package and the CLI it drives release as one version, and release-plz owns
it in the root Cargo.toml's `[workspace.package]`. The committed package holds
`0.0.0.dev0` in four places — its own version, its `onemessagebus-cli` pin, and
both constants of `_version.py` — and this script writes a copy with each
replaced, refusing when any one of them is not there exactly once: a placeholder
that moved would publish a package pinned to the wrong binary.

    bash python/onemessagebus-sdk/scripts/run python scripts/pack.py
    uv build python/onemessagebus-sdk/dist/pack/onemessagebus-sdk

It prints the directory it assembled.
"""

from __future__ import annotations

import argparse
import re
import shutil
import sys
from pathlib import Path
from typing import NoReturn

PACKAGE = Path(__file__).resolve().parents[1]
ROOT = PACKAGE.parents[1]
PLACEHOLDER = "0.0.0.dev0"
# What a publishable copy holds: the build metadata and the package source.
SHIPPED = ("pyproject.toml", "README.md", "src")


def fail(message: str) -> NoReturn:
    print(f"pack.py: {message}", file=sys.stderr)
    raise SystemExit(1)


def cargo_version(manifest: Path) -> str:
    """The `[workspace.package]` version of `manifest`."""
    section = ""
    for line in manifest.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped
        elif section == "[workspace.package]" and (
            found := re.fullmatch(r'version\s*=\s*"([^"]+)"', stripped)
        ):
            return found.group(1)
    fail(f"{manifest} declares no [workspace.package] version to stamp")


def stamp(path: Path, placeholder: str, stamped: str) -> None:
    """Replace the one `placeholder` in `path`, refusing when there is not exactly one."""
    text = path.read_text(encoding="utf-8")
    found = text.count(placeholder)
    if found != 1:
        fail(
            f"{path} holds {found} copies of `{placeholder}` where exactly one placeholder "
            "belongs; the placeholder moved, so a packed copy would pin the wrong version. "
            "Restore it in the package source, then rerun pack.py."
        )
    path.write_text(text.replace(placeholder, stamped), encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--source", type=Path, default=PACKAGE, help=argparse.SUPPRESS)
    parser.add_argument(
        "--manifest", type=Path, default=ROOT / "Cargo.toml", help=argparse.SUPPRESS
    )
    parser.add_argument(
        "--output", type=Path, default=PACKAGE / "dist" / "pack", help=argparse.SUPPRESS
    )
    args = parser.parse_args(argv)
    version = cargo_version(args.manifest)
    destination = args.output / "onemessagebus-sdk"
    shutil.rmtree(destination, ignore_errors=True)
    destination.mkdir(parents=True)
    ignore = shutil.ignore_patterns("__pycache__", "*.pyc")
    for name in SHIPPED:
        source = args.source / name
        if source.is_dir():
            shutil.copytree(source, destination / name, ignore=ignore)
        else:
            shutil.copy2(source, destination / name)
    project = destination / "pyproject.toml"
    stamp(project, f'version = "{PLACEHOLDER}"', f'version = "{version}"')
    stamp(project, f'"onemessagebus-cli=={PLACEHOLDER}"', f'"onemessagebus-cli=={version}"')
    constants = destination / "src" / "onemessagebus" / "_version.py"
    stamp(constants, f'__version__ = "{PLACEHOLDER}"', f'__version__ = "{version}"')
    stamp(constants, f'CLI_VERSION = "{PLACEHOLDER}"', f'CLI_VERSION = "{version}"')
    print(destination)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
