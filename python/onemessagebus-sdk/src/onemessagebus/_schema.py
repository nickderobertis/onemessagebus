"""`client.schema`: registering, checking and listing schemas, by type or by id."""

from __future__ import annotations

import builtins
import json
import tempfile
from collections.abc import Mapping
from pathlib import Path
from typing import TYPE_CHECKING, Any

from pydantic import BaseModel

from ._message import Message
from ._types import Payload, SchemaEntry, StrPath

if TYPE_CHECKING:
    from ._client import Client


class SchemaNamespace:
    """The schema verbs, taking a `Message` type wherever they take an id."""

    def __init__(self, client: Client) -> None:
        self._client = client

    async def register(
        self,
        schema: type[BaseModel] | Mapping[str, Any],
        id: str | None = None,
        *,
        registry: StrPath | None = None,
    ) -> str:
        """Register a `Message` type's JSON Schema under its id, or a document under `id`."""
        if isinstance(schema, Mapping):
            document = dict(schema)
        elif issubclass(schema, Message):
            document = schema.json_schema()
            id = id or schema.schema_id()
        else:
            document = schema.model_json_schema(mode="serialization")
        if id is None:
            raise ValueError(
                "a JSON Schema document that is not a Message type registers under an id; "
                "pass id='<namespace>.<name>@<version>'"
            )
        with tempfile.TemporaryDirectory(prefix="onemessagebus-schema-") as scratch:
            path = Path(scratch) / "schema.json"
            path.write_text(json.dumps(document, indent=2, sort_keys=True), encoding="utf-8")
            return await self._client.schema_register(id, path, registry=registry)

    async def check(
        self,
        schema: str | type[Message],
        payload: Payload,
        *,
        registry: StrPath | None = None,
    ) -> str:
        """Validate `payload` against the schema registered under an id, or a type's id."""
        schema_id = schema if isinstance(schema, str) else schema.schema_id()
        return await self._client.schema_check(schema_id, payload, registry=registry)

    async def list(self, *, registry: StrPath | None = None) -> builtins.list[SchemaEntry]:
        """Every registered id."""
        return await self._client.schema_list(registry=registry)
