"""A message type declared in Python, registered with the bus as its JSON Schema.

The bus validates every payload in Rust against the schema registered under the
payload's id; a `Message` subclass is that schema's Python source, so the class,
the registered document and what `next(type=...)` hands back cannot disagree.
"""

from __future__ import annotations

from typing import Any, ClassVar

from pydantic import BaseModel, ValidationError

from ._generated.contract import SchemaId

DRAFT_2020_12 = "https://json-schema.org/draft/2020-12/schema"


class Message(BaseModel):
    """The base of a message type: `class Greeting(Message, schema="demo.greeting@1")`."""

    __message_schema_id__: ClassVar[str | None] = None

    def __init_subclass__(cls, *, schema: str | None = None, **kwargs: Any) -> None:
        super().__init_subclass__(**kwargs)
        if schema is not None:
            try:
                SchemaId.model_validate(schema)
            except ValidationError:
                raise ValueError(
                    f"{cls.__name__}: {schema!r} is not a schema id; it is "
                    "<namespace>.<name>@<version>, e.g. demo.greeting@1"
                ) from None
        # Set on every subclass, so one declaring no id never inherits its parent's.
        cls.__message_schema_id__ = schema

    @classmethod
    def schema_id(cls) -> str:
        """The id this type is registered and validated under."""
        if cls.__message_schema_id__ is None:
            raise TypeError(
                f"{cls.__name__} declares no schema id; declare it as "
                f'`class {cls.__name__}(Message, schema="<namespace>.<name>@<version>")`'
            )
        return cls.__message_schema_id__

    @classmethod
    def json_schema(cls) -> dict[str, Any]:
        """The canonical JSON Schema (draft 2020-12) of this type's serialized form."""
        document = cls.model_json_schema(mode="serialization")
        return {"$schema": DRAFT_2020_12, **document, "title": cls.__name__}
