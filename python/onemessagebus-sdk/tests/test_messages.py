"""Message types: declared in Python, registered, sent, received typed, refused in Rust."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import BaseModel

from onemessagebus import BusFailed, Client, ClientConfig, Message, messages
from onemessagebus._message import DRAFT_2020_12
from onemessagebus.models import PlannerSurface as Surface
from onemessagebus.models import PlannerSurfaceV1
from tests.conftest import Greeting, make_transport


class Farewell(BaseModel):
    text: str
    final: bool = True


def test_a_malformed_id_is_refused_when_the_class_is_created() -> None:
    with pytest.raises(ValueError, match="'greeting@1' is not a schema id"):

        class Bad(Message, schema="greeting@1"):
            text: str


def test_a_message_type_emits_its_canonical_schema_under_its_id() -> None:
    assert Greeting.schema_id() == "demo.greeting@1"
    document = Greeting.json_schema()
    assert document["$schema"] == DRAFT_2020_12
    assert (document["title"], document["type"], document["required"]) == (
        "Greeting",
        "object",
        ["text"],
    )
    assert document["properties"]["text"]["type"] == "string"

    class Untagged(Greeting):
        loud: bool = False

    with pytest.raises(TypeError, match="Untagged declares no schema id"):
        Untagged.schema_id()


async def test_a_message_type_is_registered_sent_received_typed_and_refused_by_its_schema(
    client: Client,
) -> None:
    assert await client.schema.register(Greeting) == ""
    assert "demo.greeting@1" in {entry.id for entry in await client.schema.list()}
    assert await client.schema.check(Greeting, Greeting(text="hi")) == ""

    sent = await client.send("greetings", Greeting(text="hello"))
    assert [record.queue for record in sent] == ["greetings"]
    claimed = await client.next("greetings", type=Greeting)
    assert claimed is not None
    assert claimed.record == Greeting(text="hello")

    with pytest.raises(BusFailed) as refused:
        await client.send("greetings", {"text": 7})
    assert "demo.greeting@1" in refused.value.message
    assert "/text" in refused.value.message
    with pytest.raises(BusFailed, match=r"demo\.greeting@1"):
        await client.schema.check("demo.greeting@1", '{"text": false}')
    assert await client.next("greetings") is None, "nothing refused was appended"


async def test_a_plain_model_or_document_registers_under_the_id_it_is_given(
    client: Client,
) -> None:
    assert await client.schema.register(Farewell, id="demo.farewell@1") == ""
    document = {"type": "object", "properties": {"n": {"type": "integer"}}, "required": ["n"]}
    assert await client.schema.register(document, id="demo.count@1") == ""
    ids = {entry.id for entry in await client.schema.list()}
    assert {"demo.farewell@1", "demo.count@1"} <= ids
    with pytest.raises(ValueError, match="registers under an id"):
        await client.schema.register(document)


async def test_a_profile_schema_round_trips_through_its_generated_model(
    binary: Path, tmp_path_factory: pytest.TempPathFactory, transport_kind: str
) -> None:
    scratch = tmp_path_factory.mktemp("channel")
    config = ClientConfig(binary=binary, transport_dir=scratch / "channel")
    async with Client(config, make_transport(transport_kind, scratch)) as client:
        assert messages.MESSAGES["agent.planner-surface@1"] is Surface is PlannerSurfaceV1
        registered = {entry.id for entry in await client.schema_list()}
        assert set(messages.MESSAGES) <= registered
        surface = Surface(
            id=0,
            kind="finding",
            message="the base moved",
            source="proposal",
            blocking=True,
            queued_at=0,
        )
        await client.send("surfaces", surface)
        claimed = await client.next("surfaces", type=Surface)
        assert claimed is not None
        assert isinstance(claimed.record, Surface)
        assert (claimed.record.kind, claimed.record.message, claimed.record.blocking) == (
            "finding",
            "the base moved",
            True,
        )
        with pytest.raises(BusFailed, match=r"agent\.planner-surface@1"):
            await client.send("surfaces", {"kind": "finding", "message": 7, "source": "proposal"})
