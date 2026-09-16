"""Typed async Python SDK for onemessagebus.

`Client` has one method per capability of the onemessagebus binary, carried by a
`CliTransport` (the binary per call) or a `ResidentTransport` (the resident core
over its unix socket); every wire shape is a model generated from the Rust build.
"""

from ._client import Client
from ._config import ClientConfig
from ._errors import BusError, BusFailed, BusRefused, ContractError, TransportError, VersionMismatch
from ._generated import messages
from ._manifest import render_argv
from ._message import Message
from ._pin import Identity
from ._transport import CliTransport, ResidentTransport, Transport
from ._types import (
    Abandoned,
    Answer,
    CachedBundle,
    CarriedEntry,
    Claimed,
    Envelope,
    FetchedLink,
    FetchedLinkConfirmed,
    FetchedLinkFailed,
    FetchedLinkFetched,
    FetchedLinkRead,
    FetchedLinkReused,
    KindEntry,
    LogRecord,
    Payload,
    QueueStatus,
    Refused,
    Replied,
    Reply,
    SchemaCache,
    SchemaEntry,
    SchemasCleared,
    SchemasFetched,
    Sent,
    StrPath,
    Timeout,
    Validated,
    ValidatedPass,
    ValidatedRefuse,
    ValidatedUnjudged,
)
from ._version import CLI_VERSION, __version__

__all__ = [
    "CLI_VERSION",
    "Abandoned",
    "Answer",
    "BusError",
    "BusFailed",
    "BusRefused",
    "CachedBundle",
    "CarriedEntry",
    "Claimed",
    "CliTransport",
    "Client",
    "ClientConfig",
    "ContractError",
    "Envelope",
    "FetchedLink",
    "FetchedLinkConfirmed",
    "FetchedLinkFailed",
    "FetchedLinkFetched",
    "FetchedLinkRead",
    "FetchedLinkReused",
    "Identity",
    "KindEntry",
    "LogRecord",
    "Message",
    "Payload",
    "QueueStatus",
    "Refused",
    "Replied",
    "Reply",
    "ResidentTransport",
    "SchemaCache",
    "SchemaEntry",
    "SchemasCleared",
    "SchemasFetched",
    "Sent",
    "StrPath",
    "Timeout",
    "Transport",
    "TransportError",
    "Validated",
    "ValidatedPass",
    "ValidatedRefuse",
    "ValidatedUnjudged",
    "VersionMismatch",
    "__version__",
    "messages",
    "render_argv",
]
