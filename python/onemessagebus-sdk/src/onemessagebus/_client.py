"""The async client: exactly one public method per capability the Rust build declares.

Each method takes its capability's options (the options root's properties,
snake-cased), plus the payload where the verb reads stdin. It hands them to the
transport keyed as the manifest keys them, and reads the answer through the
generated models. The parity gate holds this class to the manifest in both
directions, so it has no other public method: the lifecycle is `async with`, and
`client.schema` is an attribute.
"""

from __future__ import annotations

import asyncio
import json
import os
from collections.abc import AsyncGenerator, Mapping, Sequence
from functools import cache
from types import TracebackType
from typing import Any, Literal, TypeVar, overload

from pydantic import BaseModel, TypeAdapter, ValidationError

from ._config import ClientConfig
from ._errors import BusFailed, ContractError
from ._manifest import VOCABULARY_NAME, capability, snake_case
from ._pin import Identity, hold
from ._schema import SchemaNamespace
from ._transport import CliTransport, Transport
from ._types import (
    Answer,
    CarriedEntry,
    Claimed,
    Envelope,
    KindEntry,
    LogRecord,
    Payload,
    QueueStatus,
    Replied,
    SchemaCache,
    SchemaEntry,
    SchemasCleared,
    SchemasFetched,
    Sent,
    StrPath,
    Validated,
)

ModelT = TypeVar("ModelT", bound=BaseModel)

# The options a ClientConfig supplies to every call that binds them and leaves them unset.
_DEFAULTED = ("config", "transportDir", "registry")


@cache
def _adapter(shape: Any) -> TypeAdapter[Any]:
    return TypeAdapter(shape)


def _read(shape: Any, value: Any, method: str) -> Any:
    """`value` as the generated `shape`, or a ContractError naming what disagreed."""
    try:
        return _adapter(shape).validate_python(value)
    except ValidationError as error:
        raise ContractError(
            f"{method}: the bus answered a document the SDK's generated models refuse: {error}"
        ) from None


def _text(value: Any, method: str) -> str:
    if not isinstance(value, str):
        raise ContractError(f"{method}: a text rendering was asked for, and the bus answered JSON")
    return value


def _encode(payload: Payload | None) -> str | None:
    """The JSON text a payload is written to stdin as."""
    match payload:
        case None:
            return None
        case BaseModel():
            return json.dumps(payload.model_dump(mode="json", by_alias=True, exclude_none=True))
        case bytes():
            return payload.decode("utf-8")
        case str():
            return payload
        case _:
            return json.dumps(payload)


def _spec(value: str | Mapping[str, Any] | None) -> str | None:
    """A filter or predicate spec: inline JSON for a mapping, else the text as given."""
    if value is None or isinstance(value, str):
        return value
    return json.dumps(value)


def _plain(value: Any) -> Any:
    """`value` as JSON-ready data: paths as strings, mappings and sequences walked."""
    match value:
        case os.PathLike():
            return os.fspath(value)
        case Mapping():
            return {str(key): _plain(item) for key, item in value.items()}
        case str() | bytes():
            return value
        case Sequence():
            return [_plain(item) for item in value]
        case _:
            return value


class Client:
    """The bus, from Python: `async with Client(ClientConfig(...)) as client`."""

    def __init__(self, config: ClientConfig | None = None, transport: Transport | None = None):
        self._config = config or ClientConfig()
        self._transport: Transport = transport or CliTransport()
        self._identity: Identity | None = None
        self._opening = asyncio.Lock()
        self.schema = SchemaNamespace(self)

    async def __aenter__(self) -> Client:
        await self._open()
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self._identity = None
        await self._transport.close()

    async def _open(self) -> None:
        """Open the transport once, and refuse a binary of another version before any call."""
        if self._identity is not None:
            return
        async with self._opening:
            if self._identity is not None:
                return
            identity = await self._transport.open(self._config)
            try:
                hold(identity)
            except BaseException:
                await self._transport.close()
                raise
            self._identity = identity

    def _args(self, method: str, values: Mapping[str, Any]) -> dict[str, Any]:
        """`values`, keyed as the manifest keys `method`'s options, with the config's defaults."""
        entry = capability(method)
        names = {snake_case(binding.option): binding.option for binding in entry.bindings}
        args: dict[str, Any] = {}
        for name, option in names.items():
            value = values.get(name)
            if value is None and option in _DEFAULTED:
                value = getattr(self._config, name)
            if value is not None:
                args[option] = _plain(value)
        return args

    async def _call(
        self, method: str, values: Mapping[str, Any], payload: str | None = None
    ) -> Any:
        await self._open()
        return await self._transport.call(method, self._args(method, values), payload)

    async def _reading(self, method: str, values: Mapping[str, Any], shape: Any) -> Any:
        answered = await self._call(method, values)
        if values.get("format") == "text":
            return _text(answered, method)
        return _read(shape, answered, method)

    @overload
    async def schema_list(
        self,
        *,
        registry: StrPath | None = None,
        config: StrPath | None = None,
        format: Literal["json"] | None = None,
    ) -> list[SchemaEntry]: ...
    @overload
    async def schema_list(
        self,
        *,
        registry: StrPath | None = None,
        config: StrPath | None = None,
        format: Literal["text"],
    ) -> str: ...
    async def schema_list(
        self,
        *,
        registry: StrPath | None = None,
        config: StrPath | None = None,
        format: Literal["json", "text"] | None = None,
    ) -> list[SchemaEntry] | str:
        """Every registered id: the binary's own, the registry directory's, and the linked ones."""
        values = {"registry": registry, "config": config, "format": format}
        return await self._reading("schemaList", values, list[SchemaEntry])

    async def schema_check(
        self,
        id: str,
        payload: Payload | None = None,
        *,
        file: StrPath | None = None,
        registry: StrPath | None = None,
        config: StrPath | None = None,
    ) -> str:
        """Validate a payload against the schema registered under `id`; exit 1 raises BusFailed."""
        values = {"id": id, "file": file, "registry": registry, "config": config}
        return _text(await self._call("schemaCheck", values, _encode(payload)), "schemaCheck")

    async def schema_gen(
        self,
        id: str,
        lang: str,
        *,
        registry: StrPath | None = None,
        config: StrPath | None = None,
    ) -> str:
        """The schema registered under `id`, rendered for `lang`."""
        values = {"id": id, "lang": lang, "registry": registry, "config": config}
        return _text(await self._call("schemaGen", values), "schemaGen")

    async def schema_register(
        self,
        id: str,
        file: StrPath,
        *,
        registry: StrPath | None = None,
        config: StrPath | None = None,
    ) -> str:
        """Record the JSON Schema document in `file` under `id`, in the registry directory."""
        values = {"id": id, "file": file, "registry": registry, "config": config}
        return _text(await self._call("schemaRegister", values), "schemaRegister")

    @overload
    async def schemas(self, *, format: Literal["json"] | None = None) -> SchemaCache: ...
    @overload
    async def schemas(self, *, format: Literal["text"]) -> str: ...
    async def schemas(self, *, format: Literal["json", "text"] | None = None) -> SchemaCache | str:
        """The schema cache: its directory, and each linked bundle it holds."""
        return await self._reading("schemas", {"format": format}, SchemaCache)

    @overload
    async def schemas_clear(self, *, format: Literal["json"] | None = None) -> SchemasCleared: ...
    @overload
    async def schemas_clear(self, *, format: Literal["text"]) -> str: ...
    async def schemas_clear(
        self, *, format: Literal["json", "text"] | None = None
    ) -> SchemasCleared | str:
        """Remove every entry of the schema cache, and report how many there were."""
        return await self._reading("schemasClear", {"format": format}, SchemasCleared)

    # llmlint: ignore-block[modern_domain_modeling] every method of this client takes the contract's text-shaped values as `str` — `id`, `queue`, `correlation` — the way a caller writes them, and hands them to the transport as the options root keys them; a link is text in the same way, validated where the binary parses it, and a generated `SchemaLink` root model here alone would be the one parameter a caller must wrap.
    @overload
    async def schemas_fetch(
        self,
        links: Sequence[str] | None = None,
        *,
        config: StrPath | None = None,
        format: Literal["json"] | None = None,
    ) -> SchemasFetched: ...
    @overload
    async def schemas_fetch(
        self,
        links: Sequence[str] | None = None,
        *,
        config: StrPath | None = None,
        format: Literal["text"],
    ) -> str: ...
    async def schemas_fetch(
        self,
        links: Sequence[str] | None = None,
        *,
        config: StrPath | None = None,
        format: Literal["json", "text"] | None = None,
    ) -> SchemasFetched | str:
        """Resolve each link, or every link the configuration names, revalidating the cache.

        A link that does not resolve raises BusFailed, whose `output` is the report.
        """
        values = {"links": links, "config": config, "format": format}
        return await self._reading("schemasFetch", values, SchemasFetched)

    # llmlint: ignore-end[modern_domain_modeling]

    @overload
    async def events_merge(
        self,
        files: Sequence[StrPath],
        *,
        filter: str | Mapping[str, Any] | None = None,
        profile: Literal["open"] | None = None,
        format: Literal["json"] | None = None,
    ) -> list[Envelope]: ...
    @overload
    async def events_merge(
        self,
        files: Sequence[StrPath],
        *,
        filter: str | Mapping[str, Any] | None = None,
        profile: str,
        format: Literal["json"] | None = None,
    ) -> list[dict[str, Any]]: ...
    @overload
    async def events_merge(
        self,
        files: Sequence[StrPath],
        *,
        filter: str | Mapping[str, Any] | None = None,
        profile: str | None = None,
        format: Literal["text"],
    ) -> str: ...
    async def events_merge(
        self,
        files: Sequence[StrPath],
        *,
        filter: str | Mapping[str, Any] | None = None,
        profile: str | None = None,
        format: Literal["json", "text"] | None = None,
    ) -> list[Envelope] | list[dict[str, Any]] | str:
        """Merge stream files into one stream in `(ts, stream, seq)` order.

        The binary reads through `open`, the one profile it links, whether or not
        `profile` names it, and each envelope is read into `Envelope`, that
        vocabulary's model. Naming a profile the binary does not link raises
        `BusRefused`.
        """
        values = {"files": files, "filter": _spec(filter), "profile": profile, "format": format}
        shape = list[Envelope] if profile in (None, VOCABULARY_NAME) else list[dict[str, Any]]
        return await self._reading("eventsMerge", values, shape)

    @overload
    async def events_emit(
        self,
        path: StrPath,
        kind: str,
        stream: str,
        payload: Payload | None = None,
        *,
        source: str | None = None,
        profile: Literal["open"] | None = None,
        labels: Mapping[str, Any] | None = None,
        file: StrPath | None = None,
        format: Literal["json"] | None = None,
    ) -> Envelope: ...
    @overload
    async def events_emit(
        self,
        path: StrPath,
        kind: str,
        stream: str,
        payload: Payload | None = None,
        *,
        source: str | None = None,
        profile: str,
        labels: Mapping[str, Any] | None = None,
        file: StrPath | None = None,
        format: Literal["json"] | None = None,
    ) -> dict[str, Any]: ...
    @overload
    async def events_emit(
        self,
        path: StrPath,
        kind: str,
        stream: str,
        payload: Payload | None = None,
        *,
        source: str | None = None,
        profile: str | None = None,
        labels: Mapping[str, Any] | None = None,
        file: StrPath | None = None,
        format: Literal["text"],
    ) -> str: ...
    async def events_emit(
        self,
        path: StrPath,
        kind: str,
        stream: str,
        payload: Payload | None = None,
        *,
        source: str | None = None,
        profile: str | None = None,
        labels: Mapping[str, Any] | None = None,
        file: StrPath | None = None,
        format: Literal["json", "text"] | None = None,
    ) -> Envelope | dict[str, Any] | str:
        """Append one envelope to the stream file `path`, and answer the envelope written."""
        values = {
            "path": path,
            "kind": kind,
            "stream": stream,
            "source": source,
            "profile": profile,
            "labels": labels,
            "file": file,
            "format": format,
        }
        shape = Envelope if profile in (None, VOCABULARY_NAME) else dict[str, Any]
        answered = await self._call("eventsEmit", values, _encode(payload))
        if format == "text":
            return _text(answered, "eventsEmit")
        return _read(shape, answered, "eventsEmit")

    async def deliver(
        self,
        address: StrPath,
        payload: Payload | None = None,
        *,
        message: str | Mapping[str, Any] | None = None,
        file: StrPath | None = None,
        wait: int | None = None,
    ) -> Any:
        """Send one message to the spool at `address`, and answer the receiver's disposition."""
        values = {"address": address, "message": _spec(message), "file": file, "wait": wait}
        return await self._call("deliver", values, _encode(payload))

    @overload
    async def inbox_carried(
        self, store: StrPath, *, format: Literal["json"] | None = None
    ) -> list[CarriedEntry]: ...
    @overload
    async def inbox_carried(self, store: StrPath, *, format: Literal["text"]) -> str: ...
    async def inbox_carried(
        self, store: StrPath, *, format: Literal["json", "text"] | None = None
    ) -> list[CarriedEntry] | str:
        """Every message the carry store holds, in the order carried, without draining it."""
        values = {"store": store, "format": format}
        return await self._reading("inboxCarried", values, list[CarriedEntry])

    async def send(
        self,
        queue: str,
        message: Payload | None,
        *,
        file: StrPath | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> list[Sent]:
        """Append a record to `queue`, and answer every record appended."""
        values = {
            "queue": queue,
            "file": file,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        return _read(list[Sent], await self._call("send", values, _encode(message)), "send")

    @overload
    async def next(
        self,
        queue: str,
        *,
        consumer: str | None = None,
        asker: str | None = None,
        type: type[BaseModel] | None = None,
        format: Literal["json"] | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> Claimed | None: ...
    @overload
    async def next(
        self,
        queue: str,
        *,
        consumer: str | None = None,
        asker: str | None = None,
        type: type[BaseModel] | None = None,
        format: Literal["text"],
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> str | None: ...
    async def next(
        self,
        queue: str,
        *,
        consumer: str | None = None,
        asker: str | None = None,
        type: type[BaseModel] | None = None,
        format: Literal["json", "text"] | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> Claimed | str | None:
        """Claim the next record of `queue`; `None` when there is nothing to claim.

        With `type`, the claimed record is read into that model.
        """
        values = {
            "queue": queue,
            "consumer": consumer,
            "asker": asker,
            "format": format,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        try:
            claimed = await self._reading("next", values, Claimed)
        except BusFailed as refused:
            if refused.message == f"nothing on {queue} to claim":
                return None
            raise
        if isinstance(claimed, Claimed) and type is not None:
            return claimed.model_copy(update={"record": _read(type, claimed.record, "next")})
        return claimed

    async def reply(
        self,
        queue: str,
        correlation: str | None,
        reply: Payload | None,
        *,
        position: int | None = None,
        file: StrPath | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> Replied:
        """Answer a pending ask of `queue`: by `correlation`, by `position`, or the one pending."""
        values = {
            "queue": queue,
            "position": position,
            "correlation": correlation,
            "file": file,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        return _read(Replied, await self._call("reply", values, _encode(reply)), "reply")

    @overload
    def subscribe(
        self,
        queue: str,
        *,
        until: str | Mapping[str, Any],
        timeout: int | None = None,
        format: Literal["json"] | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> AsyncGenerator[LogRecord, None]: ...
    @overload
    def subscribe(
        self,
        queue: str,
        *,
        until: str | Mapping[str, Any],
        timeout: int | None = None,
        format: Literal["text"],
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> AsyncGenerator[str, None]: ...
    async def subscribe(
        self,
        queue: str,
        *,
        until: str | Mapping[str, Any],
        timeout: int | None = None,
        format: Literal["json", "text"] | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> AsyncGenerator[LogRecord | str, None]:
        """Every line of `queue`'s log, then each as it arrives, until `until` admits one.

        The iterator ends when the predicate holds; a timeout raises BusFailed.
        Closing it early (`break` inside `contextlib.aclosing`, or `aclose()`)
        stops the stream: the resident is told to cancel, a spawned process ends.
        """
        await self._open()
        values = {
            "queue": queue,
            "until": _spec(until),
            "timeout": timeout,
            "format": format,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        lines = self._transport.stream("subscribe", self._args("subscribe", values), None)
        try:
            async for line in lines:
                yield (
                    _text(line, "subscribe")
                    if format == "text"
                    else _read(LogRecord, line, "subscribe")
                )
        finally:
            await lines.aclose()

    @overload
    async def status(
        self,
        queue: str | None = None,
        *,
        format: Literal["json"] | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> list[QueueStatus]: ...
    @overload
    async def status(
        self,
        queue: str | None = None,
        *,
        format: Literal["text"],
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> str: ...
    async def status(
        self,
        queue: str | None = None,
        *,
        format: Literal["json", "text"] | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> list[QueueStatus] | str:
        """Report `queue`, or every declared queue."""
        values = {
            "queue": queue,
            "format": format,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        return await self._reading("status", values, list[QueueStatus])

    @overload
    async def transports(self, *, format: Literal["json"] | None = None) -> list[KindEntry]: ...
    @overload
    async def transports(self, *, format: Literal["text"]) -> str: ...
    async def transports(
        self, *, format: Literal["json", "text"] | None = None
    ) -> list[KindEntry] | str:
        """Every transport kind this build can open."""
        return await self._reading("transports", {"format": format}, list[KindEntry])

    async def validate(
        self,
        queue: str,
        message: Payload | None,
        *,
        file: StrPath | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> Validated:
        """Judge a record as `send` would, appending nothing; a refusal verdict is returned."""
        values = {
            "queue": queue,
            "file": file,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        try:
            judged = await self._call("validate", values, _encode(message))
        except BusFailed as refused:
            if refused.output is None:
                raise
            judged = refused.output
        return _read(Validated, judged, "validate")

    async def ask(
        self,
        queue: str,
        question: Payload | None,
        *,
        blocking: bool | None = None,
        asker: str | None = None,
        about: str | None = None,
        timeout: int | None = None,
        correlation: str | None = None,
        file: StrPath | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> Answer:
        """Ask on `queue` and wait: a Reply, or the Timeout, Abandoned or Refused answer."""
        values = {
            "queue": queue,
            "blocking": blocking,
            "asker": asker,
            "about": about,
            "timeout": timeout,
            "correlation": correlation,
            "file": file,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        try:
            answered = await self._call("ask", values, _encode(question))
        except BusFailed as refused:
            if refused.output is None:
                raise
            answered = refused.output
        return _read(Answer, answered, "ask")

    async def serve(
        self,
        queue: str,
        codec: str,
        frames: str | Sequence[Payload] | None = None,
        *,
        session_seconds: int | None = None,
        asker: str | None = None,
        file: StrPath | None = None,
        config: StrPath | None = None,
        transport_dir: StrPath | None = None,
        registry: StrPath | None = None,
    ) -> list[dict[str, Any]]:
        """Serve a member protocol over `frames`, answering each frame's response."""
        values = {
            "queue": queue,
            "codec": codec,
            "session_seconds": session_seconds,
            "asker": asker,
            "file": file,
            "config": config,
            "transport_dir": transport_dir,
            "registry": registry,
        }
        if frames is None or isinstance(frames, str):
            written = frames
        else:
            written = "".join(f"{_encode(frame)}\n" for frame in frames)
        answered = await self._call("serve", values, written)
        return _read(list[dict[str, Any]], answered, "serve")
