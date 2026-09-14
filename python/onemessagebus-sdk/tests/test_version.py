"""The binary a client drives: how it is found, and the version it is held to."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from onemessagebus import Client, ClientConfig, Identity, TransportError, VersionMismatch
from onemessagebus._pin import (
    PLACEHOLDER,
    hold,
    pinned_cli_version,
    reported_version,
    workspace_version,
)
from tests.conftest import ROOT, make_transport

CHECKOUT_VERSION = workspace_version((ROOT / "Cargo.toml").read_text(encoding="utf-8"))


def executable(directory: Path, script: str) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "onemessagebus"
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)
    return path


async def test_a_binary_of_another_version_is_refused_naming_both(
    tmp_path_factory: pytest.TempPathFactory, transport_kind: str
) -> None:
    scratch = tmp_path_factory.mktemp("pin")
    # The one subprocess double: a binary reporting a version nothing released.
    other = executable(scratch / "bin", "#!/bin/sh\necho 'onemessagebus 9.9.9'\n")
    transport = make_transport(transport_kind, scratch)
    with pytest.raises(VersionMismatch) as refused:
        async with Client(ClientConfig(binary=other), transport):
            pass  # pragma: no cover - the client refuses before it is entered
    assert refused.value.message == (
        f"this onemessagebus SDK drives onemessagebus-cli {CHECKOUT_VERSION}, and {other} reports "
        f"9.9.9; install onemessagebus-cli=={CHECKOUT_VERSION}"
    )
    assert (refused.value.expected, refused.value.reported) == (CHECKOUT_VERSION, "9.9.9")
    assert not (scratch / "bus.sock").exists(), "nothing was started for a refused binary"


async def test_the_binary_is_config_then_the_environment_then_path(
    binary: Path, tmp_path: Path
) -> None:
    named = ClientConfig(env={"ONEMESSAGEBUS_BIN": str(binary)})
    async with Client(named) as client:
        assert [kind.kind for kind in await client.transports()][:2] == ["local", "memory"]

    on_path = tmp_path / "bin"
    on_path.mkdir()
    (on_path / "onemessagebus").symlink_to(binary)
    found = ClientConfig(env={"PATH": str(on_path), "ONEMESSAGEBUS_BIN": ""})
    async with Client(found) as client:
        assert "local" in await client.transports(format="text")

    nowhere = ClientConfig(env={"PATH": str(tmp_path / "empty"), "ONEMESSAGEBUS_BIN": ""})
    with pytest.raises(TransportError, match="no onemessagebus binary to drive") as missing:
        async with Client(nowhere):
            pass  # pragma: no cover - refused on entry
    assert f"install onemessagebus-cli=={CHECKOUT_VERSION}" in missing.value.message


async def test_a_first_call_opens_a_client_that_was_never_entered(binary: Path) -> None:
    client = Client(ClientConfig(binary=binary))
    assert "local" in await client.transports(format="text")


async def test_a_binary_that_cannot_run_or_does_not_report_a_version_is_refused(
    tmp_path: Path,
) -> None:
    with pytest.raises(TransportError, match=r"cannot run .*missing"):
        async with Client(ClientConfig(binary=tmp_path / "missing")):
            pass  # pragma: no cover - refused on entry
    silent = shutil.which("true")
    assert silent is not None
    with pytest.raises(TransportError, match="which is not `onemessagebus <version>`"):
        async with Client(ClientConfig(binary=silent)):
            pass  # pragma: no cover - refused on entry


def test_the_pin_is_the_stamped_version_else_the_checkout_version(tmp_path: Path) -> None:
    assert pinned_cli_version() == CHECKOUT_VERSION
    assert pinned_cli_version("1.2.3") == "1.2.3"
    checkout = tmp_path / "checkout"
    (checkout / "python").mkdir(parents=True)
    (checkout / "Cargo.toml").write_text(
        '[package]\nversion = "0.0.1"\n\n[workspace.package]\nversion = "4.5.6"\n',
        encoding="utf-8",
    )
    assert pinned_cli_version(PLACEHOLDER, checkout / "python" / "_version.py") == "4.5.6"
    hold(Identity("/opt/onemessagebus", "4.5.6"), source=checkout / "python" / "_version.py")

    elsewhere = tmp_path / "unstamped" / "_version.py"
    assert pinned_cli_version(PLACEHOLDER, elsewhere) is None
    with pytest.raises(VersionMismatch, match="unstamped development copy") as unpinned:
        hold(Identity("/opt/onemessagebus", "4.5.6"), source=elsewhere)
    assert unpinned.value.expected is None
    with pytest.raises(VersionMismatch, match=r"drives onemessagebus-cli 1\.0\.0"):
        hold(Identity("/opt/onemessagebus", "4.5.6"), stamped="1.0.0")


def test_a_version_line_is_read_only_as_onemessagebus_prints_it() -> None:
    assert reported_version("onemessagebus 0.3.0\n") == "0.3.0"
    assert reported_version("onemessagebus-cli 0.3.0\n") is None
    assert workspace_version('[workspace]\nmembers = []\n[package]\nversion = "1"\n') is None
