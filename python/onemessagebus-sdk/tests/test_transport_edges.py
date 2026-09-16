"""What a transport does at its edges: output it cannot read, and a peer that breaks the protocol.

The journeys drive the real binary, which never misbehaves on purpose; here the
misbehaviour is staged — a unix socket speaking lines `bus.resident-protocol@1`
does not admit — so the refusals the SDK owes a caller are held too.
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import signal
import sys
from collections.abc import AsyncIterator, Callable
from contextlib import asynccontextmanager
from pathlib import Path

import pytest

from onemessagebus import (
    BusError,
    BusFailed,
    Client,
    ClientConfig,
    CliTransport,
    ContractError,
    ResidentTransport,
    Transport,
    TransportError,
)
from onemessagebus._errors import refusal
from onemessagebus._manifest import capability
from onemessagebus._pin import pinned_cli_version
from onemessagebus._transport import _failure, _owner, _parse, _spawn, _stop, refusal_text
from tests.conftest import bus_config

# What the staged peer writes back for the request line it read.
Answer = Callable[[dict[str, object]], list[str]]


def test_output_that_is_not_its_shape_and_stderr_with_no_refusal_line() -> None:
    with pytest.raises(ContractError, match="status: the binary printed output that is not json"):
        _parse(capability("status"), {}, "not json")
    assert _parse(capability("send"), {}, '{"a": 1}\n\n{"b": 2}\n') == [{"a": 1}, {"b": 2}]
    failed = _failure(capability("validate"), {}, 1, "half a verdict", "onemessagebus: no")
    assert isinstance(failed, BusFailed)
    assert (failed.message, failed.output) == ("no", "half a verdict")
    assert refusal_text("", 3) == "onemessagebus exited 3 and said nothing"
    assert refusal_text("thread 'main' panicked\n", 101) == "thread 'main' panicked"
    other = refusal(101, "panicked")
    assert type(other) is BusError
    assert other.exit == 101
    assert _owner(Path("/nonexistent/bus.sock")) is None


async def test_a_streamed_verb_with_input_runs_over_either_transport(
    binary: Path, scratch: Path
) -> None:
    config = bus_config(binary, scratch)
    args = {
        "queue": "surfaces",
        "codec": "example",
        "config": str(config.config),
        "registry": str(config.registry),
    }
    transports: list[Transport] = [CliTransport(), ResidentTransport(scratch / "bus.sock")]
    for transport in transports:
        await transport.open(config)
        try:
            assert [line async for line in transport.stream("serve", args, "")] == []
        finally:
            await transport.close()


@asynccontextmanager
async def staged_peer(socket: Path, answer: Answer) -> AsyncIterator[None]:
    """A unix socket that reads request lines and writes back what `answer` stages."""

    async def serve(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        while line := await reader.readline():
            for reply in answer(json.loads(line)):
                writer.write(reply.encode() + b"\n")
            await writer.drain()
        writer.close()

    server = await asyncio.start_unix_server(serve, path=str(socket))
    try:
        yield
    finally:
        server.close()
        await server.wait_closed()


async def over_peer(binary: Path, scratch: Path, answer: Answer) -> AsyncIterator[Client]:
    socket = scratch / "peer.sock"
    async with staged_peer(socket, answer):
        transport = ResidentTransport(socket, start=False)
        async with Client(ClientConfig(binary=binary), transport) as client:
            yield client


@pytest.mark.parametrize(
    ("staged", "refused"),
    [
        (lambda _: ["not json"], "does not admit"),
        (lambda r: [json.dumps({"id": r["id"], "verb": "send"})], "wrote a client's line"),
        (lambda _: ['{"id": null, "error": {"exit": 2, "message": "no"}}'], "refused a line"),
    ],
)
async def test_a_peer_writing_outside_the_protocol_ends_the_connection_with_a_contract_error(
    binary: Path, scratch: Path, staged: Answer, refused: str
) -> None:
    async for client in over_peer(binary, scratch, staged):
        with pytest.raises(ContractError, match=refused):
            await client.transports()
        with pytest.raises(ContractError, match=refused):
            await client.transports()


async def test_answers_the_models_reject_are_contract_errors(binary: Path, scratch: Path) -> None:
    def staged(request: dict[str, object]) -> list[str]:
        if request["verb"] == "status":
            return [json.dumps({"id": request["id"], "event": {"position": 1, "record": {}}})]
        return [json.dumps({"id": request["id"], "ok": [{"kind": 7}]})]

    async for client in over_peer(binary, scratch, staged):
        with pytest.raises(ContractError, match="generated models refuse"):
            await client.transports()
        with pytest.raises(ContractError, match="a text rendering was asked for"):
            await client.schema_gen("demo.greeting@1", "json")
        with pytest.raises(ContractError, match=r"answered request \d+ \(status\) with an event"):
            await client.status()


async def test_a_process_that_ignores_sigterm_is_killed_once_its_wait_passes() -> None:
    shell = shutil.which("sh")
    assert shell is not None
    stubborn = await _spawn(
        shell, ["-c", "trap '' TERM; echo ready; exec sleep 30"], ClientConfig(), stdin=False
    )
    assert stubborn.stdout is not None
    assert await stubborn.stdout.readline() == b"ready\n", "SIGTERM is ignored from here on"
    await _stop(stubborn, wait=0.3)
    assert stubborn.returncode == -signal.SIGKILL


# A resident double from a binary path this test controls: it reports the pinned
# version, records its pid beside its socket as a resident does, answers every
# request with an empty list, and ignores both the removal of its socket and SIGTERM.
STUBBORN_RESIDENT = """#!{python}
import json, os, signal, socket, sys
signal.signal(signal.SIGTERM, signal.SIG_IGN)
if sys.argv[1:] == ["--version"]:
    print("onemessagebus {version}")
    sys.exit(0)
listener = socket.socket(socket.AF_UNIX)
path = sys.argv[sys.argv.index("--socket") + 1]
listener.bind(path)
listener.listen()
with open(path + ".pid", "w") as recorded:
    recorded.write(str(os.getpid()) + "\\n")
while True:
    connection, _ = listener.accept()
    for line in connection.makefile("r"):
        answer = {{"id": json.loads(line)["id"], "ok": []}}
        connection.sendall((json.dumps(answer) + "\\n").encode())
"""


async def test_a_started_resident_that_outlives_its_socket_and_sigterm_is_killed(
    tmp_path_factory: pytest.TempPathFactory,
) -> None:
    scratch = tmp_path_factory.mktemp("stubborn")
    binary = scratch / "onemessagebus"
    binary.write_text(
        STUBBORN_RESIDENT.format(python=sys.executable, version=pinned_cli_version()),
        encoding="utf-8",
    )
    binary.chmod(0o755)
    transport = ResidentTransport(scratch / "bus.sock", stop_timeout=0.3)
    async with Client(ClientConfig(binary=binary), transport) as client:
        assert await client.transports() == []
        resident = transport._process
        assert resident is not None
    assert resident.returncode == -signal.SIGKILL
    assert not transport.started
    assert not (scratch / "bus.sock").exists()


# A resident double that reports the pinned version, records its pid, and never
# listens — nor exits within any wait this test allows: a start that hangs, as the
# transport sees one.
SILENT_RESIDENT = """#!/bin/sh
if [ "$1" = --version ]; then echo "onemessagebus {version}"; exit 0; fi
echo $$ > "{pid}"
exec sleep 600
"""


async def test_a_started_resident_that_never_listens_is_stopped_and_the_wait_named(
    tmp_path_factory: pytest.TempPathFactory,
) -> None:
    scratch = tmp_path_factory.mktemp("silent")
    binary = scratch / "onemessagebus"
    recorded = scratch / "pid"
    binary.write_text(
        SILENT_RESIDENT.format(version=pinned_cli_version(), pid=recorded), encoding="utf-8"
    )
    binary.chmod(0o755)
    transport = ResidentTransport(scratch / "bus.sock", start_timeout=0.3, stop_timeout=0.3)
    async with Client(ClientConfig(binary=binary), transport) as client:
        # Bounded well inside the double's own life: a transport that waited it out
        # rather than stopping it would time out here instead of refusing.
        with pytest.raises(TransportError, match=r"did not listen on .* within 0\.3 seconds"):
            await asyncio.wait_for(client.transports(), 10)
        assert not transport.started
    with pytest.raises(ProcessLookupError):
        os.kill(int(recorded.read_text(encoding="utf-8")), 0)
