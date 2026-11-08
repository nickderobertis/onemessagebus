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
from ._generated.contract import AskedDropped as DroppedQuestion
from ._generated.contract import AskedRefused as Refused
from ._generated.contract import AskedReply as Reply
from ._generated.contract import AskedTimeout as Timeout
from ._generated.contract import (
    CacheConfig,
    CachedBundle,
    CarriedEntry,
    Config,
    Envelope,
    FetchedLinkConfirmed,
    FetchedLinkFailed,
    FetchedLinkFetched,
    FetchedLinkRead,
    FetchedLinkReused,
    HookConfig,
    HookRef,
    KindEntry,
    LogRecord,
    PolicyConfig,
    Predicate,
    QueueConfig,
    QueueStatus,
    Replied,
    SchemaCache,
    SchemaEntry,
    SchemasCleared,
    SchemasFetched,
    Sent,
    TransportConfig,
    ValidatedPass,
    ValidatedRecord,
    ValidatedRefuse,
    ValidatedUnjudged,
)
from ._generated.contract import ClaimedRecord as Claimed
from ._generated.contract import DroppedRecord as Dropped
from ._generated.contract import When1 as Carries

#: A path the binary is handed.
StrPath: TypeAlias = str | os.PathLike[str]

#: What `ask` answered: a reply, or the named reason there is none.
Answer: TypeAlias = Reply | Timeout | Abandoned | Refused | DroppedQuestion

#: One line `send` printed: a record it appended, or one a hook dropped.
SentLine: TypeAlias = Sent | Dropped

#: What `reply` did: the reply delivered, or `Dropped` when a hook dropped it whole,
#: in which case nothing reached the answers queue.
ReplyLine: TypeAlias = Replied | Dropped

#: How `schemas fetch` ended for one link, told apart by `outcome`.
FetchedLink: TypeAlias = (
    FetchedLinkRead
    | FetchedLinkFetched
    | FetchedLinkConfirmed
    | FetchedLinkReused
    | FetchedLinkFailed
)

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
    "CacheConfig",
    "CachedBundle",
    "CarriedEntry",
    "Carries",
    "Claimed",
    "Config",
    "Dropped",
    "DroppedQuestion",
    "Envelope",
    "FetchedLink",
    "FetchedLinkConfirmed",
    "FetchedLinkFailed",
    "FetchedLinkFetched",
    "FetchedLinkRead",
    "FetchedLinkReused",
    "HookConfig",
    "HookRef",
    "KindEntry",
    "LogRecord",
    "Payload",
    "PolicyConfig",
    "Predicate",
    "QueueConfig",
    "QueueStatus",
    "Refused",
    "Replied",
    "Reply",
    "ReplyLine",
    "SchemaCache",
    "SchemaEntry",
    "SchemasCleared",
    "SchemasFetched",
    "Sent",
    "SentLine",
    "StrPath",
    "Timeout",
    "TransportConfig",
    "Validated",
    "ValidatedPass",
    "ValidatedRecord",
    "ValidatedRefuse",
    "ValidatedUnjudged",
]
