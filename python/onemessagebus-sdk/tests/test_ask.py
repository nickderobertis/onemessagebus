"""`ask` and `reply` through the client, over both transports: every answer is data or a raise."""

from __future__ import annotations

import asyncio

import pytest

from onemessagebus import Abandoned, BusFailed, BusRefused, Client, Reply, Timeout

QUESTION = {"message": "which base?", "source": "proposal"}
VERDICT = {"completion": True, "reason": "main"}


async def queued(client: Client) -> None:
    """Wait until a question is on `questions`."""
    for _ in range(1500):
        if (await client.status("questions"))[0].records:
            return
        await asyncio.sleep(0.02)
    raise AssertionError("the question never reached questions")


async def test_an_ask_answers_the_reply_a_concurrent_reply_gives(client: Client) -> None:
    asking = asyncio.ensure_future(
        client.ask(
            "questions", QUESTION, blocking=True, asker="worker-1", about="build", timeout=30
        )
    )
    await queued(client)
    replied = await client.reply("questions", None, VERDICT)
    answer = await asyncio.wait_for(asking, 30)
    assert isinstance(answer, Reply)
    assert answer.reply == {"id": 0, **VERDICT, "correlation": answer.correlation}
    assert replied.correlation is not None
    assert replied.correlation.root == answer.correlation
    assert replied.answered is not None
    assert replied.answered.record["message"] == "which base?"
    assert (replied.answered.record["asker"], replied.answered.record["about"]) == (
        "worker-1",
        "build",
    )
    assert replied.answered.record["blocking"] is True
    assert [sent.queue for sent in replied.sent] == ["answers"]


async def test_an_unanswered_ask_times_out_and_a_listener_again_finds_it_abandoned(
    client: Client,
) -> None:
    timed = await client.ask("questions", QUESTION, timeout=1)
    assert isinstance(timed, Timeout)
    assert timed.correlation.startswith("c-")
    again = await client.ask("questions", None, correlation=timed.correlation, timeout=1)
    assert isinstance(again, (Abandoned, Timeout))
    assert again.correlation == timed.correlation
    replied = await client.reply("questions", timed.correlation, VERDICT)
    assert replied.correlation is not None
    assert replied.correlation.root == timed.correlation
    with pytest.raises(
        BusFailed, match=f"no pending ask carries the correlation {timed.correlation}"
    ):
        await client.reply("questions", timed.correlation, VERDICT)


async def test_a_question_the_bus_refuses_raises(client: Client) -> None:
    with pytest.raises(BusRefused, match="the payload is not JSON") as refused:
        await client.ask("questions", "which base?")
    assert refused.value.exit == 2
    with pytest.raises(BusRefused, match="--asker is set to a blank value"):
        await client.ask("questions", QUESTION, asker=" ")
    with pytest.raises(BusRefused, match="answers is a plain queue, so it has no questions"):
        await client.ask("answers", QUESTION, timeout=1)
