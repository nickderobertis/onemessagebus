"""Fixtures over the real binary: a scratch bus, and a client over either transport."""

from __future__ import annotations

import json
import os
import subprocess
from collections.abc import AsyncIterator
from pathlib import Path

import pytest

from onemessagebus import (
    Client,
    ClientConfig,
    CliTransport,
    Message,
    ResidentTransport,
    Transport,
)

PACKAGE = Path(__file__).resolve().parents[1]
ROOT = PACKAGE.parents[1]
BINARY = ROOT / "target" / "debug" / ("onemessagebus.exe" if os.name == "nt" else "onemessagebus")
BUILD = "just nx run onemessagebus-cli:build"


class Greeting(Message, schema="demo.greeting@1"):
    """The message type every scratch registry holds, registered before a test runs."""

    text: str


@pytest.fixture(scope="session")
def binary() -> Path:
    """The binary built from this checkout; a missing one fails, naming the build."""
    if not BINARY.is_file():
        pytest.fail(f"{BINARY} is not built; build it with `{BUILD}` from the repository root")
    return BINARY


@pytest.fixture
def scratch(binary: Path, tmp_path_factory: pytest.TempPathFactory) -> Path:
    """A short scratch directory: a unix socket path must fit in 108 bytes.

    It holds `onemessagebus.yaml`, declaring a `greetings` queue whose schema,
    `demo.greeting@1` — the `Greeting` type above — is registered in `registry/` by the binary — beside the
    planner channel's queues, kept in `channel/`.
    """
    directory = tmp_path_factory.mktemp("bus")
    (directory / "onemessagebus.yaml").write_text(
        "version: 1\n"
        f"transport: {{kind: local, dir: {directory / 'channel'}}}\n"
        "profile: planner-channel\n"
        "queues:\n"
        "  greetings: {schema: demo.greeting@1}\n",
        encoding="utf-8",
    )
    document = directory / "greeting.schema.json"
    document.write_text(json.dumps(Greeting.json_schema()), encoding="utf-8")
    subprocess.run(  # noqa: S603 - argv is the checkout's built binary and constant words; a resolved path is never a literal, and a bare name trips S607
        [
            str(binary),
            *("schema", "register", "--file", str(document)),
            *("--registry", str(directory / "registry"), "--", Greeting.schema_id()),
        ],
        check=True,
        capture_output=True,
    )
    return directory


@pytest.fixture(params=["cli", "resident"])
def transport_kind(request: pytest.FixtureRequest) -> str:
    """Each transport in turn: every test taking this runs over both."""
    return request.param


def make_transport(kind: str, scratch: Path) -> Transport:
    """A fresh transport of `kind` over `scratch`."""
    return CliTransport() if kind == "cli" else ResidentTransport(scratch / "bus.sock")


def bus_config(binary: Path, scratch: Path) -> ClientConfig:
    """The scratch bus's configuration and registry, driving the built binary."""
    return ClientConfig(
        binary=binary,
        config=scratch / "onemessagebus.yaml",
        registry=scratch / "registry",
        cwd=scratch,
    )


@pytest.fixture
async def client(binary: Path, scratch: Path, transport_kind: str) -> AsyncIterator[Client]:
    """A client over the scratch bus, on the transport under test."""
    async with Client(bus_config(binary, scratch), make_transport(transport_kind, scratch)) as bus:
        yield bus
