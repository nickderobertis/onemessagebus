"""`subscribe` over both transports: a stream until its predicate, a timeout, and an early close."""

from __future__ import annotations

import asyncio
import contextlib
from typing import Any

import pytest

from onemessagebus import BusFailed, BusRefused, Client, CliTransport, LogRecord, ResidentTransport


async def test_a_subscription_streams_what_is_there_then_what_arrives_until_its_predicate(
    client: Client,
) -> None:
    await client.send("greetings", {"text": "hi"})
    records: list[LogRecord] = []

    async def listen() -> None:
        async for record in client.subscribe(
            "greetings", until={"field": "text", "equals": "bye"}, timeout=30
        ):
            records.append(record)
            if len(records) == 1:
                await client.send("greetings", {"text": "bye"})

    await asyncio.wait_for(listen(), 30)
    assert [record.record for record in records] == [{"text": "hi"}, {"text": "bye"}]
    assert records[0].position < records[1].position

    lines = [
        line
        async for line in client.subscribe(
            "greetings", until='{"field": "text", "equals": "bye"}', format="text"
        )
    ]
    assert len(lines) == 2
    assert lines[1].endswith('{"text":"bye"}')


async def test_a_subscription_that_times_out_or_cannot_parse_its_predicate_raises(
    client: Client,
) -> None:
    with pytest.raises(BusFailed) as lapsed:
        async for _ in client.subscribe(
            "commands", until={"field": "x", "present": True}, timeout=1
        ):
            pass  # pragma: no cover - nothing arrives
    assert lapsed.value.message == "commands: no record --until admits arrived within 1 seconds"
    with pytest.raises(BusRefused, match='--until: "not json" is neither inline JSON'):
        async for _ in client.subscribe("commands", until="not json", timeout=1):
            pass  # pragma: no cover - refused before any line


async def test_closing_a_subscription_early_stops_it(client: Client) -> None:
    await client.send(
        "surfaces", {"kind": "finding", "message": "m", "source": "proposal", "blocking": False}
    )
    stream = client.subscribe("surfaces", until={"field": "message", "equals": "never"})
    async with contextlib.aclosing(stream) as records:
        async for record in records:
            assert record.record["message"] == "m"
            break
    transport: Any = client._transport
    if isinstance(transport, CliTransport):
        assert not transport._running, "the subscribe process was ended"
    else:
        assert isinstance(transport, ResidentTransport)
        assert not transport._pending, "the resident answered the cancel"
    # The transport goes on: a cancelled stream leaves the connection usable.
    assert (await client.status("surfaces"))[0].records == 1
