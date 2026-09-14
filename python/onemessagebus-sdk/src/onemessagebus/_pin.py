"""Where the client finds its binary, and the onemessagebus-cli version it holds that binary to.

The SDK and the CLI release as one version. A published package has that version
stamped into `_version.CLI_VERSION` by scripts/pack.py; a development checkout
still carries the placeholder, and there the pin is the workspace version of the
Cargo.toml above the package source, which is what `target/debug/onemessagebus`
was built from.
"""

from __future__ import annotations

import os
import re
import shutil
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from ._errors import TransportError, VersionMismatch
from ._version import CLI_VERSION

PLACEHOLDER = "0.0.0.dev0"
_REPORTED = re.compile(r"onemessagebus (\S+)\s*")
_VERSION_LINE = re.compile(r'version\s*=\s*"([^"]+)"')


@dataclass(frozen=True)
class Identity:
    """What answered a transport's open: the binary, and the version it reports."""

    binary: str
    version: str


def workspace_version(manifest: str) -> str | None:
    """The `[workspace.package]` version of a Cargo.toml's text, when it declares one."""
    section = ""
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped
        elif section == "[workspace.package]" and (found := _VERSION_LINE.fullmatch(stripped)):
            return found.group(1)
    return None


def pinned_cli_version(stamped: str = CLI_VERSION, source: Path | None = None) -> str | None:
    """The onemessagebus-cli version this SDK drives, or `None` when nothing says."""
    if stamped != PLACEHOLDER:
        return stamped
    for directory in (source or Path(__file__)).resolve().parents:
        manifest = directory / "Cargo.toml"
        if manifest.is_file() and (version := workspace_version(manifest.read_text("utf-8"))):
            return version
    return None


def reported_version(stdout: str) -> str | None:
    """The version `onemessagebus --version` printed, as `onemessagebus X.Y.Z`."""
    found = _REPORTED.fullmatch(stdout)
    return found.group(1) if found else None


def install_hint() -> str:
    """The next action for a missing or mismatched binary."""
    pinned = pinned_cli_version()
    return f"install onemessagebus-cli=={pinned}" if pinned else "install onemessagebus-cli"


def resolve_binary(binary: str | os.PathLike[str] | None, env: Mapping[str, str]) -> str:
    """`binary`, else `ONEMESSAGEBUS_BIN`, else `onemessagebus` on `PATH`."""
    if binary is not None:
        return os.fspath(binary)
    if named := env.get("ONEMESSAGEBUS_BIN"):
        return named
    if found := shutil.which("onemessagebus", path=env.get("PATH")):
        return found
    raise TransportError(
        "no onemessagebus binary to drive: ClientConfig(binary=...) and ONEMESSAGEBUS_BIN "
        f"are unset, and none is on PATH; {install_hint()}, or set ONEMESSAGEBUS_BIN"
    )


def hold(identity: Identity, *, stamped: str = CLI_VERSION, source: Path | None = None) -> None:
    """Refuse a binary that is not the version this SDK drives."""
    expected = pinned_cli_version(stamped, source)
    if expected is None:
        raise VersionMismatch(
            "this onemessagebus SDK is an unstamped development copy with no onemessagebus "
            f"checkout's Cargo.toml above it, so there is no onemessagebus-cli version to hold "
            f"{identity.binary} ({identity.version}) to; install a released onemessagebus package",
            expected=None,
            reported=identity.version,
        )
    if identity.version != expected:
        raise VersionMismatch(
            f"this onemessagebus SDK drives onemessagebus-cli {expected}, and {identity.binary} "
            f"reports {identity.version}; install onemessagebus-cli=={expected}",
            expected=expected,
            reported=identity.version,
        )
