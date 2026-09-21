"""The resident transport's own behaviour: starting, sharing, concurrency, and a resident that goes."""

from __future__ import annotations

import asyncio
import time
from dataclasses import replace
from pathlib import Path

import pytest

from onemessagebus import (
    Client,
    ClientConfig,
    CliTransport,
    LogRecord,
    ResidentTransport,
    TransportError,
)
from tests.conftest import bus_config


async def test_a_resident_is_started_shared_and_stopped_only_by_the_transport_that_started_it(
    binary: Path, scratch: Path
) -> None:
    config = bus_config(binary, scratch)
    socket = scratch / "bus.sock"
    owner_transport = ResidentTransport(socket)
    async with Client(config, owner_transport) as owner:
        assert not socket.exists(), "nothing starts before the first call"
        assert "local" in await owner.transports(format="text")
        assert owner_transport.started
        assert socket.exists()

        guest_transport = ResidentTransport(socket, start=False)
        async with Client(config, guest_transport) as guest:
            assert "local" in {kind.kind for kind in await guest.transports()}
            assert not guest_transport.started
        assert socket.exists(), "closing a guest leaves a resident it did not start"

        records: list[LogRecord] = []

        async def listen() -> None:
            async for record in owner.subscribe(
                "greetings", until={"field": "text", "equals": "last"}, timeout=30
            ):
                records.append(record)

        # Calls and a running subscription share one connection, told apart by id.
        listening = asyncio.ensure_future(listen())
        sent = await asyncio.gather(*(owner.send("greetings", {"text": t}) for t in ("a", "b")))
        assert sorted(batch[0].queue for batch in sent) == ["greetings", "greetings"]
        statuses = await asyncio.gather(*(owner.status("greetings") for _ in range(3)))
        assert {status[0].records for status in statuses} == {2}
        await owner.send("greetings", {"text": "last"})
        await asyncio.wait_for(listening, 30)
        assert records[-1].record == {"text": "last"}
        assert len(records) == 3
    assert not socket.exists(), "the owner removed its resident's socket"
    assert not owner_transport.started


async def test_nothing_answering_and_start_false_is_refused_with_the_command_to_run(
    binary: Path, scratch: Path
) -> None:
    transport = ResidentTransport(scratch / "bus.sock", start=False)
    async with Client(bus_config(binary, scratch), transport) as client:
        with pytest.raises(TransportError, match=r"nothing answers on .*serve --resident --socket"):
            await client.transports()


async def test_a_resident_that_cannot_start_is_refused_in_its_own_words(
    binary: Path, scratch: Path
) -> None:
    config = ClientConfig(binary=binary, config=scratch / "missing.yaml", cwd=scratch)
    async with Client(config, ResidentTransport("bus.sock")) as client:
        with pytest.raises(TransportError, match="exited 2 before it listened on") as refused:
            await client.status()
    assert "missing.yaml" in refused.value.message
    assert not (scratch / "bus.sock").exists()


async def test_a_resident_that_goes_away_ends_what_ran_over_it(binary: Path, scratch: Path) -> None:
    config = bus_config(binary, scratch)
    socket = scratch / "bus.sock"
    async with Client(config, ResidentTransport(socket)) as owner:
        await owner.transports()
        async with Client(config, ResidentTransport(socket, start=False)) as guest:
            stream = guest.subscribe("actions", until={"field": "x", "present": True})

            async def listen() -> None:
                async for _ in stream:
                    pass

            listening = asyncio.ensure_future(listen())
            await asyncio.sleep(0.3)
            socket.unlink()  # how a resident is stopped: its socket goes
            with pytest.raises(TransportError, match="closed the connection"):
                await asyncio.wait_for(listening, 30)
            with pytest.raises(TransportError, match="closed the connection"):
                await guest.transports()


async def test_a_transport_that_was_never_opened_says_how_to_open_it() -> None:
    with pytest.raises(TransportError, match="not open"):
        await ResidentTransport("bus.sock").call("transports", {}, None)
    with pytest.raises(TransportError, match="not open"):
        await CliTransport().call("transports", {}, None)


def wait_for_file(path: Path, within: float = 30.0) -> None:
    """Block until `path` exists; run in a thread, beside the event loop."""
    deadline = time.monotonic() + within
    while not path.exists():
        if time.monotonic() > deadline:
            raise AssertionError(f"{path} never appeared")
        time.sleep(0.02)


async def test_a_resident_another_client_started_first_is_used_and_left_running(
    binary: Path, scratch: Path
) -> None:
    config = bus_config(binary, scratch)
    socket = scratch / "bus.sock"
    spawned = scratch / "late-resident-spawned"
    # A binary path this test controls: the built binary, except that `serve` first
    # says it was spawned and pauses, so another client's resident takes the socket
    # between the late transport's spawn and its connect.
    late_binary = scratch / "late-onemessagebus"
    late_binary.write_text(
        f'#!/bin/sh\nif [ "$1" = serve ]; then touch "{spawned}"; sleep 1; fi\n'
        f'exec "{binary}" "$@"\n',
        encoding="utf-8",
    )
    late_binary.chmod(0o755)
    owner_transport = ResidentTransport(socket)
    late_transport = ResidentTransport(socket)
    async with Client(config, owner_transport) as owner:
        async with Client(replace(config, binary=late_binary), late_transport) as late:
            answering = asyncio.ensure_future(late.transports(format="text"))
            await asyncio.to_thread(wait_for_file, spawned)
            assert "local" in await owner.transports(format="text")
            assert owner_transport.started
            assert "local" in await asyncio.wait_for(answering, 30)
            assert not late_transport.started, "the late transport's own resident lost the socket"
        assert socket.exists(), "closing the late client left the live resident running"
        assert "local" in await owner.transports(format="text")
        live = owner_transport._process
        assert live is not None
        assert live.returncode is None
    assert not socket.exists()
