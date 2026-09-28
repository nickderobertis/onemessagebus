"""Fixtures over the real binary: a scratch bus, and a client over either transport."""

from __future__ import annotations

import json
import os
import shutil
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
EXECUTABLE = "onemessagebus.exe" if os.name == "nt" else "onemessagebus"
BUILD = "just nx run onemessagebus-cli:build"


def cargo_target_dir() -> Path:
    """Cargo's effective target directory, where the build left the binary.

    `ONEMESSAGEBUS_TARGET_DIR` when a recipe resolved it, else `cargo metadata`'s
    own, which honours `CARGO_TARGET_DIR` and Cargo's configuration alike.
    """
    handed = os.environ.get("ONEMESSAGEBUS_TARGET_DIR")
    if handed:
        return Path(handed)
    cargo = shutil.which("cargo")
    if cargo is None:
        pytest.fail("cargo is not on PATH; install the pinned toolchain with `just bootstrap`")
    run = subprocess.run(  # noqa: S603 - argv is cargo as shutil.which resolved it and constant arguments; a bare name trips S607
        [cargo, "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if run.returncode != 0:
        pytest.fail(f"`cargo metadata` did not name Cargo's target directory:\n{run.stderr}")
    metadata = run.stdout
    return Path(json.loads(metadata)["target_directory"])


class Greeting(Message, schema="demo.greeting@1"):
    """The message type every scratch registry holds, registered before a test runs."""

    text: str


@pytest.fixture(scope="session")
def binary() -> Path:
    """The binary built from this checkout; a missing one fails, naming the build."""
    built = cargo_target_dir() / "debug" / EXECUTABLE
    if not built.is_file():
        pytest.fail(f"{built} is not built; build it with `{BUILD}` from the repository root")
    return built


@pytest.fixture
async def scratch(binary: Path, tmp_path_factory: pytest.TempPathFactory) -> Path:
    """A short scratch directory: a unix socket path must fit in 108 bytes.

    It holds `onemessagebus.yaml`, declaring a `greetings` queue typed by
    `demo.greeting@1` — the `Greeting` type above, registered in `registry/` through
    the client as a user registers one — a `hellos` queue typed by the core's own
    `onemessagebus.transport-hello@1`, and a `questions` event queue whose asks are answered on
    `answers`, kept in `bus/`.
    """
    directory = tmp_path_factory.mktemp("bus")
    (directory / "onemessagebus.yaml").write_text(
        "version: 1\n"
        f"transport: {{kind: local, dir: {directory / 'bus'}}}\n"
        "queues:\n"
        "  greetings: {schema: demo.greeting@1}\n"
        "  hellos: {schema: onemessagebus.transport-hello@1}\n"
        "  questions: {policy: {hold_pending: true, blocking_first: true}, answers: answers}\n"
        "  answers: {numbered: true}\n"
        "  actions: {numbered: true}\n"
        "codecs:\n"
        "  example:\n"
        "    select: kind\n"
        "    frames:\n"
        "      finding:\n"
        "        schema: demo.greeting@1\n"
        "        bindings:\n"
        "          - do: answer\n"
        "            response: {completion: false}\n",
        encoding="utf-8",
    )
    registry = ClientConfig(binary=binary, registry=directory / "registry", cwd=directory)
    async with Client(registry, CliTransport()) as registering:
        await registering.schema.register(Greeting)
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
