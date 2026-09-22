"""Message types: declared in Python, registered, sent, received typed, refused in Rust."""

from __future__ import annotations

import pytest
from pydantic import BaseModel, ValidationError

from onemessagebus import BusFailed, Client, Message, messages
from onemessagebus._generated.contract import Config
from onemessagebus._message import DRAFT_2020_12
from onemessagebus.models import TransportHello, TransportHelloV1
from tests.conftest import Greeting


class Farewell(BaseModel):
    text: str
    final: bool = True


def test_generated_config_validates_author_names_and_refusal_reasons() -> None:
    base = {"version": 1, "transport": {"kind": "memory"}}
    Config.model_validate({**base, "authors": {"sentinel": {"capabilities": ["finding"]}}})
    with pytest.raises(ValidationError):
        Config.model_validate({**base, "authors": {"Bad_Name": {"capabilities": []}}})
    with pytest.raises(ValidationError):
        Config.model_validate(
            {
                **base,
                "authors": {"sentinel": {"capabilities": [], "refusals": {"finding": "   "}}},
            }
        )


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


async def test_a_core_schema_round_trips_through_its_generated_model(client: Client) -> None:
    assert messages.MESSAGES["onemessagebus.transport-hello@1"] is TransportHello
    assert TransportHello is TransportHelloV1
    registered = {entry.id for entry in await client.schema_list()}
    assert set(messages.MESSAGES) == {
        entry for entry in registered if not entry.startswith("demo.")
    }, "the models are the binary's own registry, and nothing of a product's"
    assert not any(entry.startswith("agent.") for entry in registered)
    hello = TransportHello.model_validate(
        {"protocol": "onemessagebus-transport", "version": 1, "config": {"kind": "nats"}}
    )
    await client.send("hellos", hello)
    claimed = await client.next("hellos", type=TransportHello)
    assert claimed is not None
    assert isinstance(claimed.record, TransportHello)
    assert claimed.record.model_dump(mode="json", exclude_none=True) == {
        "protocol": "onemessagebus-transport",
        "version": 1,
        "config": {"kind": "nats"},
    }
    with pytest.raises(BusFailed, match=r"onemessagebus\.transport-hello@1"):
        await client.send("hellos", {"protocol": "onemessagebus-transport", "version": "one"})
    with pytest.raises(BusFailed, match=r"onemessagebus\.transport-hello@1"):
        await client.send("hellos", {"protocol": 7, "version": 1, "config": {"kind": "nats"}})
    assert await client.next("hellos") is None, "nothing refused was appended"
