"""The public names of the generated models a client hands back.

Nothing here declares a shape: each name is a generated model, or a union of
them, under the name a caller reads it by.
"""

from __future__ import annotations

import os
from collections.abc import Mapping, Sequence
from typing import Any, TypeAlias

from pydantic import BaseModel

from ._generated.contract import AskedAbandoned as Abandoned
from ._generated.contract import AskedRefused as Refused
from ._generated.contract import AskedReply as Reply
from ._generated.contract import AskedTimeout as Timeout
from ._generated.contract import (
    CachedBundle,
    CarriedEntry,
    Envelope,
    FetchedLink,
    KindEntry,
    LogRecord,
    QueueStatus,
    Replied,
    SchemaCache,
    SchemaEntry,
    SchemasCleared,
    SchemasFetched,
    Sent,
    ValidatedPass,
    ValidatedRefuse,
    ValidatedUnjudged,
)
from ._generated.contract import ClaimedRecord as Claimed

#: A path the binary is handed.
StrPath: TypeAlias = str | os.PathLike[str]

#: What `ask` answered: a reply, or the named reason there is none.
Answer: TypeAlias = Reply | Timeout | Abandoned | Refused

#: What `validate` judged, told apart by `verdict`.
Validated: TypeAlias = ValidatedPass | ValidatedRefuse | ValidatedUnjudged

#: A payload written to the binary's stdin: a model, serialized as JSON; a `str`
#: or `bytes`, taken as JSON text already; or any other JSON value.
Payload: TypeAlias = (
    BaseModel | Mapping[str, Any] | Sequence[Any] | str | bytes | int | float | bool
)

__all__ = [
    "Abandoned",
    "Answer",
    "CachedBundle",
    "CarriedEntry",
    "Claimed",
    "Envelope",
    "FetchedLink",
    "KindEntry",
    "LogRecord",
    "Payload",
    "QueueStatus",
    "Refused",
    "Replied",
    "Reply",
    "SchemaCache",
    "SchemaEntry",
    "SchemasCleared",
    "SchemasFetched",
    "Sent",
    "StrPath",
    "Timeout",
    "Validated",
    "ValidatedPass",
    "ValidatedRefuse",
    "ValidatedUnjudged",
]
