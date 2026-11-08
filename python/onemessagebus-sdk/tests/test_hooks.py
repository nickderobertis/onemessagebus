"""Hooks, configured through the SDK's own types and run by the real binary, over both transports.

Every configuration here is a `Config` built from `HookConfig` entries and
written with `write_config`; each hook is a shell command, and the counting ones
append a line to a marker file so a test can read how many times they ran.
"""

from __future__ import annotations

import time
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path

import pytest

from onemessagebus import (
    CacheConfig,
    Carries,
    Client,
    ClientConfig,
    Config,
    Dropped,
    DroppedQuestion,
    HookConfig,
    LogRecord,
    PolicyConfig,
    QueueConfig,
    Replied,
    Sent,
    Timeout,
    TransportConfig,
    ValidatedPass,
    ValidatedRecord,
    write_config,
)
from tests.conftest import make_transport

# Each hook reads its whole input first, so the binary never writes into a closed pipe.
DROP = ["sh", "-c", "cat >/dev/null"]
SHOUT = ["sh", "-c", 'cat >/dev/null; printf \'{"said":"LOUD"}\\n\'']
MOVE = ["sh", "-c", 'cat; printf elsewhere > "$ONEMESSAGEBUS_HOOK_ROUTE"']


def counting(marker: Path, then: str) -> list[str]:
    """A command appending one line to `marker` each time it runs, then running `then`."""
    return ["sh", "-c", f'echo ran >> "$0"; {then}', str(marker)]


@dataclass(frozen=True)
class Hooked:
    """A scratch bus whose configuration declares hooks, and the marker files they count in."""

    directory: Path
    config: Config
    dropped_runs: Path
    cached_runs: Path


def runs(marker: Path) -> int:
    """How many times the command counting in `marker` ran."""
    return len(marker.read_text(encoding="utf-8").splitlines()) if marker.exists() else 0


def hooked_config(directory: Path) -> Hooked:
    """The configuration every test here drives, built through the SDK's types alone.

    On `notes`: a counting drop of what carries `drop`, a change of what carries
    `shout`, a reroute to `elsewhere` of what carries `move`, and a cached counting
    keep of what carries `count`. A drop of a question carrying `drop` on
    `questions`, and of a reply carrying `discard` on its answers queue.
    """
    dropped_runs = directory / "dropped.runs"
    cached_runs = directory / "cached.runs"
    config = Config(
        version=1,
        transport=TransportConfig(kind="local", dir=str(directory / "bus")),
        queues={
            "notes": QueueConfig(numbered=True),
            "elsewhere": QueueConfig(),
            "questions": QueueConfig(policy=PolicyConfig(hold_pending=True), answers="answers"),
            "answers": QueueConfig(numbered=True),
        },
        hooks=[
            HookConfig(
                on="notes",
                when=Carries(carries="drop"),
                command=counting(dropped_runs, "cat >/dev/null"),
            ),
            HookConfig(on="notes", when=Carries(carries="shout"), command=SHOUT),
            HookConfig(
                on="notes", when=Carries(carries="move"), routes=["elsewhere"], command=MOVE
            ),
            HookConfig(
                on="notes",
                when=Carries(carries="count"),
                command=counting(cached_runs, "cat"),
                cache=CacheConfig(dir=str(directory / "cache"), bar_fingerprint=["echo", "bar-1"]),
            ),
            HookConfig(on="questions", when=Carries(carries="drop"), command=DROP),
            HookConfig(on="answers", when=Carries(carries="discard"), command=DROP),
        ],
    )
    return Hooked(directory, config, dropped_runs, cached_runs)


@pytest.fixture
def hooked(tmp_path_factory: pytest.TempPathFactory) -> Hooked:
    """A short scratch directory holding the hooked configuration, written by `write_config`."""
    directory = tmp_path_factory.mktemp("hooks")
    bus = hooked_config(directory)
    write_config(bus.config, directory / "onemessagebus.yaml")
    return bus


@pytest.fixture
async def client(binary: Path, hooked: Hooked, transport_kind: str) -> AsyncIterator[Client]:
    """A client over the hooked bus, on the transport under test."""
    config = ClientConfig(
        binary=binary, config=hooked.directory / "onemessagebus.yaml", cwd=hooked.directory
    )
    async with Client(config, make_transport(transport_kind, hooked.directory)) as bus:
        yield bus


async def drop_log(client: Client, queue: str) -> list[LogRecord]:
    """Every entry of `queue`'s drop log, read through `subscribe` up to its first one."""
    until = {"field": "queue", "equals": queue}
    return [record async for record in client.subscribe(f"{queue}-dropped", until=until, timeout=5)]


def test_write_config_writes_what_the_typed_configuration_says(tmp_path: Path) -> None:
    bus = hooked_config(tmp_path)
    written = write_config(bus.config, tmp_path / "bus.yaml")
    assert written == tmp_path / "bus.yaml"
    assert Config.model_validate_json(written.read_text(encoding="utf-8")) == bus.config
    assert '"bar_fingerprint"' in written.read_text(encoding="utf-8")
    assert ": null" not in written.read_text(encoding="utf-8"), "an unset key is left out"


async def test_a_drop_on_send_answers_dropped_and_when_skips_what_it_does_not_name(
    client: Client, hooked: Hooked
) -> None:
    [kept] = await client.send("notes", {"kind": "plain"})
    assert isinstance(kept, Sent)
    assert (kept.queue, kept.id) == ("notes", 0)
    assert runs(hooked.dropped_runs) == 0, "the drop hook's `when` skipped a message without `drop`"

    [dropped] = await client.send("notes", {"kind": "plain", "drop": True})
    assert isinstance(dropped, Dropped)
    assert (dropped.queue, dropped.dropped) == ("notes", True)
    assert runs(hooked.dropped_runs) == 1

    [notes] = await client.status("notes")
    assert (notes.records, notes.dropped) == (1, 1)
    assert " dropped=1" in await client.status("notes", format="text")
    [entry] = await drop_log(client, "notes")
    assert entry.record["message"] == {"kind": "plain", "drop": True}
    assert entry.record["hook"] == {
        "index": 0,
        "command": counting(hooked.dropped_runs, "cat >/dev/null"),
    }


async def test_a_route_lands_the_message_on_the_queue_it_names(client: Client) -> None:
    [moved] = await client.send("notes", {"move": True})
    assert isinstance(moved, Sent)
    assert moved.queue == "elsewhere"
    statuses = {status.queue: status for status in await client.status()}
    assert (statuses["notes"].records, statuses["elsewhere"].records) == (0, 1)
    assert statuses["elsewhere"].waiting == [{"move": True}]


async def test_a_cache_hit_does_not_run_the_command_again(client: Client, hooked: Hooked) -> None:
    first = await client.send("notes", {"count": 1})
    second = await client.send("notes", {"count": 1})
    assert [(sent.queue, isinstance(sent, Sent)) for sent in [*first, *second]] == [
        ("notes", True),
        ("notes", True),
    ]
    assert runs(hooked.cached_runs) == 1, "the second offer was answered from the cache"
    await client.send("notes", {"count": 2})
    assert runs(hooked.cached_runs) == 2, "another message is a cache miss"
    assert (await client.status("notes"))[0].records == 3


async def test_validate_answers_the_outcome_and_the_records_a_send_would_append(
    client: Client,
) -> None:
    kept = await client.validate("notes", {"kind": "plain"})
    assert isinstance(kept, ValidatedPass)
    assert (kept.outcome, kept.records) == (
        "keep",
        [ValidatedRecord(queue="notes", record={"kind": "plain"})],
    )
    changed = await client.validate("notes", {"shout": True})
    assert isinstance(changed, ValidatedPass)
    assert (changed.outcome, changed.records) == (
        "change",
        [ValidatedRecord(queue="notes", record={"said": "LOUD"})],
    )
    dropped = await client.validate("notes", {"drop": True})
    assert isinstance(dropped, ValidatedPass)
    assert (dropped.queue, dropped.outcome, dropped.records) == ("notes", "drop", [])
    assert (await client.status("notes"))[0].records == 0, "validate appends nothing"


async def test_a_dropped_reply_answers_dropped_and_reaches_no_answers_queue(
    client: Client,
) -> None:
    timed = await client.ask("questions", {"message": "which base?"}, timeout=1)
    assert isinstance(timed, Timeout)
    reply = {"completion": True, "discard": True}
    replied = await client.reply("questions", timed.correlation, reply)
    assert isinstance(replied, Dropped), "a dropped reply is not read as delivered"
    assert not isinstance(replied, Replied)
    assert replied.queue == "answers"
    [answers] = await client.status("answers")
    assert (answers.records, answers.dropped) == (0, 1)
    [entry] = await drop_log(client, "answers")
    assert entry.record["message"] == reply
    assert entry.record["correlation"] == timed.correlation


async def test_a_dropped_question_answers_dropped_at_once(client: Client) -> None:
    started = time.monotonic()
    answer = await client.ask("questions", {"message": "which base?", "drop": True}, timeout=30)
    assert time.monotonic() - started < 15, "a dropped question does not wait for its timeout"
    assert isinstance(answer, DroppedQuestion)
    assert (answer.answer, answer.queue) == ("dropped", "questions")
    assert (answer.hook.index, answer.hook.command) == (0, DROP)
    [questions] = await client.status("questions")
    assert (questions.records, questions.dropped) == (0, 1)
