"""`ask` and `reply` through the client, over both transports: every answer is data or a raise."""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from pathlib import Path

import pytest

from onemessagebus import Abandoned, BusFailed, BusRefused, Client, ClientConfig, Reply, Timeout
from tests.conftest import make_transport

QUESTION = {"kind": "planner-question", "message": "which base?", "source": "proposal"}
VERDICT = {"version": 3, "completion": True, "reason": "main"}


@pytest.fixture
async def channel(
    binary: Path, tmp_path_factory: pytest.TempPathFactory, transport_kind: str
) -> AsyncIterator[Client]:
    """A client over a planner channel kept in a transport directory, with no configuration."""
    scratch = tmp_path_factory.mktemp("ask")
    config = ClientConfig(binary=binary, transport_dir=scratch / "channel")
    async with Client(config, make_transport(transport_kind, scratch)) as client:
        yield client


async def queued(client: Client) -> None:
    """Wait until a question is on `surfaces`."""
    for _ in range(1500):
        if (await client.status("surfaces"))[0].records:
            return
        await asyncio.sleep(0.02)
    raise AssertionError("the question never reached surfaces")  # pragma: no cover


async def test_an_ask_answers_the_reply_a_concurrent_reply_gives(channel: Client) -> None:
    asking = asyncio.ensure_future(
        channel.ask(
            "surfaces", QUESTION, blocking=True, asker="worker-1", about="build", timeout=30
        )
    )
    await queued(channel)
    replied = await channel.reply("surfaces", None, VERDICT)
    answer = await asyncio.wait_for(asking, 30)
    assert isinstance(answer, Reply)
    assert answer.reply["reply"] == VERDICT
    assert replied.correlation is not None
    assert replied.correlation.root == answer.correlation
    assert replied.answered is not None
    assert replied.answered.record["message"] == "which base?"
    assert replied.answered.record["workstream"] == "build"
    assert [sent.queue for sent in replied.sent] == ["replies"]


async def test_an_unanswered_ask_times_out_and_a_listener_again_finds_it_abandoned(
    channel: Client,
) -> None:
    timed = await channel.ask("surfaces", QUESTION, timeout=1)
    assert isinstance(timed, Timeout)
    assert timed.correlation.startswith("c-")
    again = await channel.ask("surfaces", None, correlation=timed.correlation, timeout=1)
    assert isinstance(again, (Abandoned, Timeout))
    assert again.correlation == timed.correlation
    replied = await channel.reply("surfaces", timed.correlation, VERDICT)
    assert replied.correlation is not None
    assert replied.correlation.root == timed.correlation
    with pytest.raises(
        BusFailed, match=f"no pending ask carries the correlation {timed.correlation}"
    ):
        await channel.reply("surfaces", timed.correlation, VERDICT)


async def test_a_question_the_bus_refuses_raises(channel: Client) -> None:
    with pytest.raises(BusRefused, match="the payload is not JSON") as refused:
        await channel.ask("surfaces", "which base?")
    assert refused.value.exit == 2
    with pytest.raises(BusRefused, match="--asker is set to a blank value"):
        await channel.ask("surfaces", QUESTION, asker=" ")
