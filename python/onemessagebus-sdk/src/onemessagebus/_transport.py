"""How a client reaches the bus: the binary once per call, or the resident core over its socket.

A transport takes a capability's manifest method, its options keyed as the
manifest keys them, and the bytes the verb reads on stdin, and hands back what
the verb answered in the resident protocol's `ok` shape — the JSON document of a
`json` verb, the list of line documents of a `jsonl` verb, the text of a `text`
verb or a `format: "text"` rendering — or raises the refusal as the typed error.
Both transports answer in that one shape, so the client parses one shape
whichever carried the call, and a native backend implements the same seam.
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
import json
import os
from collections import deque
from collections.abc import AsyncGenerator, Mapping
from pathlib import Path
from typing import Any, Protocol, cast

from pydantic import ValidationError

from ._config import ClientConfig
from ._errors import BusError, ContractError, TransportError, refusal
from ._generated.messages.bus_resident_protocol_v1 import (
    ResidentAnswer,
    ResidentCancel,
    ResidentEvent,
    ResidentFailure,
    ResidentLine,
    ResidentRequest,
)
from ._manifest import Capability, capability, render_argv
from ._pin import Identity, install_hint, reported_version

# One line of a verb's output can be a whole record; asyncio's 64 KiB default
# would refuse a large one as a protocol error.
_LINE_LIMIT = 16 * 1024 * 1024
# How long a stopped process, or a cancelled stream, is given to end.
_STOP_WAIT = 10.0
_PREFIX = "onemessagebus: "

Line = ResidentAnswer | ResidentFailure | ResidentEvent


class Transport(Protocol):
    """The seam a client calls the bus through."""

    async def open(self, config: ClientConfig) -> Identity:
        """Bind to `config`, and say which binary answers and the version it reports."""
        ...

    async def call(self, capability: str, args: Mapping[str, Any], input: str | None) -> Any:
        """Run one capability to its answer."""
        ...

    def stream(
        self, capability: str, args: Mapping[str, Any], input: str | None
    ) -> AsyncGenerator[Any, None]:
        """Run a streaming capability, yielding each line; closing it stops the stream."""
        ...

    async def close(self) -> None:
        """Release what `open` took: a connection, and a resident this transport started."""
        ...


def _renders_text(entry: Capability, args: Mapping[str, Any]) -> bool:
    return entry.stdout == "text" or args.get("format") == "text"


def _parse(entry: Capability, args: Mapping[str, Any], stdout: str) -> Any:
    """What the verb printed, in the resident protocol's `ok` shape."""
    if _renders_text(entry, args):
        return stdout
    try:
        if entry.stdout == "jsonl":
            return [json.loads(line) for line in stdout.splitlines() if line.strip()]
        return json.loads(stdout)
    except json.JSONDecodeError as error:
        raise ContractError(
            f"{entry.method}: the binary printed output that is not {entry.stdout}: {error}"
        ) from None


def refusal_text(stderr: str, code: int) -> str:
    """The refusal the binary wrote: its last `onemessagebus: ` line, or clap's `error:` paragraph."""
    lines = stderr.splitlines()
    said = [line[len(_PREFIX) :] for line in lines if line.startswith(_PREFIX)]
    if said:
        return said[-1]
    for index, line in enumerate(lines):
        if line.startswith("error: "):
            paragraph = [line[len("error: ") :]]
            for rest in lines[index + 1 :]:
                if not rest.strip():
                    break
                paragraph.append(rest.strip())
            return " ".join(paragraph)
    return stderr.strip() or f"onemessagebus exited {code} and said nothing"


def _failure(entry: Capability, args: Mapping[str, Any], code: int, out: str, err: str) -> BusError:
    output: Any = None
    if out.strip():
        try:
            output = _parse(entry, args, out)
        except ContractError:
            output = out
    return refusal(code, refusal_text(err, code), output)


async def _spawn(
    binary: str, argv: list[str], config: ClientConfig, *, stdin: bool
) -> asyncio.subprocess.Process:
    try:
        return await asyncio.create_subprocess_exec(
            binary,
            *argv,
            cwd=os.fspath(config.cwd) if config.cwd is not None else None,
            env=config.environment(),
            stdin=asyncio.subprocess.PIPE if stdin else asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            limit=_LINE_LIMIT,
        )
    except OSError as error:
        raise TransportError(
            f"cannot run {binary}: {error.strerror or error}; {install_hint()}, or point "
            "ClientConfig(binary=...) or ONEMESSAGEBUS_BIN at one"
        ) from error


async def _stop(process: asyncio.subprocess.Process, wait: float = _STOP_WAIT) -> None:
    """End a process this transport spawned: SIGTERM, then SIGKILL once `wait` seconds pass."""
    if process.returncode is not None:
        return
    with contextlib.suppress(ProcessLookupError):
        process.terminate()
    try:
        await asyncio.wait_for(process.wait(), wait)
    except asyncio.TimeoutError:
        with contextlib.suppress(ProcessLookupError):
            process.kill()
        await process.wait()


async def binary_identity(config: ClientConfig) -> Identity:
    """The binary `config` names, and the version its `--version` reports."""
    binary = config.binary_path()
    process = await _spawn(binary, ["--version"], config, stdin=False)
    out, _ = await process.communicate()
    printed = out.decode("utf-8", "replace")
    version = reported_version(printed)
    if process.returncode != 0 or version is None:
        raise TransportError(
            f"{binary} --version exited {process.returncode} printing {printed.strip()!r}, "
            f"which is not `onemessagebus <version>`; {install_hint()}"
        )
    return Identity(binary, version)


class CliTransport:
    """Spawn the binary once per call, with the argv the manifest renders."""

    def __init__(self) -> None:
        self._config: ClientConfig | None = None
        self._binary: str | None = None
        self._running: set[asyncio.subprocess.Process] = set()

    async def open(self, config: ClientConfig) -> Identity:
        identity = await binary_identity(config)
        self._config, self._binary = config, identity.binary
        return identity

    def _bound(self) -> tuple[ClientConfig, str]:
        if self._config is None or self._binary is None:
            raise TransportError(
                "the transport is not open; use `async with Client(...)`, or await "
                "transport.open(config) first"
            )
        return self._config, self._binary

    async def call(self, capability: str, args: Mapping[str, Any], input: str | None) -> Any:
        entry = _capability(capability)
        argv = render_argv(capability, args)
        config, binary = self._bound()
        process = await _spawn(binary, argv, config, stdin=input is not None)
        self._running.add(process)
        try:
            out, err = await process.communicate(
                input.encode("utf-8") if input is not None else None
            )
        finally:
            # A call cancelled while it waits leaves no verb running behind it.
            await _stop(process)
            self._running.discard(process)
        stdout, stderr = out.decode("utf-8", "replace"), err.decode("utf-8", "replace")
        code = process.returncode or 0
        if code != 0:
            raise _failure(entry, args, code, stdout, stderr)
        return _parse(entry, args, stdout)

    async def stream(
        self, capability: str, args: Mapping[str, Any], input: str | None
    ) -> AsyncGenerator[Any, None]:
        entry = _capability(capability)
        argv = render_argv(capability, args)
        config, binary = self._bound()
        process = await _spawn(binary, argv, config, stdin=input is not None)
        self._running.add(process)
        stdout = cast("asyncio.StreamReader", process.stdout)  # sound: _spawn pipes stdout
        stderr = cast("asyncio.StreamReader", process.stderr)  # sound: _spawn pipes stderr
        # stderr is drained beside stdout: a filled pipe would stall the verb.
        errors = asyncio.ensure_future(stderr.read())
        try:
            if input is not None and process.stdin is not None:
                process.stdin.write(input.encode("utf-8"))
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    await process.stdin.drain()
                process.stdin.close()
            async for raw in stdout:
                line = raw.decode("utf-8", "replace").rstrip("\r\n")
                if not line.strip():
                    continue
                if _renders_text(entry, args):
                    yield line
                    continue
                try:
                    yield json.loads(line)
                except json.JSONDecodeError as error:
                    raise ContractError(
                        f"{capability}: the binary streamed a line that is not JSON: {error}"
                    ) from None
            said = (await errors).decode("utf-8", "replace")
            code = await process.wait()
            if code != 0:
                raise _failure(entry, args, code, "", said)
        finally:
            # Closing the iterator early ends the process it started.
            await _stop(process)
            errors.cancel()
            await asyncio.wait({errors})
            self._running.discard(process)

    async def close(self) -> None:
        for process in list(self._running):
            await _stop(process)
        self._running.clear()


def _capability(method: str) -> Capability:
    return capability(method)


class ResidentTransport:
    """Speak bus.resident-protocol@1 to a resident core over its unix socket.

    Every call and every running subscription shares one connection, told apart
    by request id. The connection is made on the first call, after the client
    has held the binary to its version — so a mismatched binary is refused
    before anything is started. When nothing answers on `socket` and `start` is
    true, that first call starts `onemessagebus serve --resident --socket
    <socket>` with the client configuration's config, transport directory and
    registry, and `close` stops that resident — and only a resident this
    transport started — by removing its socket, ending it with SIGTERM and then
    SIGKILL should it outlive `stop_timeout` seconds after each.
    """

    def __init__(
        self,
        socket: str | os.PathLike[str],
        *,
        start: bool = True,
        start_timeout: float = 20.0,
        stop_timeout: float = _STOP_WAIT,
    ) -> None:
        self._socket = socket
        self._start = start
        self._start_timeout = start_timeout
        self._stop_timeout = stop_timeout
        self._config: ClientConfig | None = None
        self._binary: str | None = None
        self._path: Path | None = None
        self._connecting = asyncio.Lock()
        self._writer: asyncio.StreamWriter | None = None
        self._listener: asyncio.Future[None] | None = None
        self._closed: BusError | None = None
        self._pending: dict[int, asyncio.Queue[Line | BusError]] = {}
        self._ids = itertools.count(1)
        self._process: asyncio.subprocess.Process | None = None
        self._said: deque[str] = deque(maxlen=20)
        self._drain: asyncio.Future[None] | None = None

    @property
    def started(self) -> bool:
        """Whether the resident answering this transport is one it started."""
        return self._process is not None

    async def open(self, config: ClientConfig) -> Identity:
        identity = await binary_identity(config)
        self._config, self._binary = config, identity.binary
        base = Path(os.fspath(config.cwd)) if config.cwd is not None else Path.cwd()
        self._path = base / os.fspath(self._socket)
        return identity

    async def _dial(self) -> tuple[asyncio.StreamReader, asyncio.StreamWriter]:
        return await asyncio.open_unix_connection(str(self._path), limit=_LINE_LIMIT)

    async def _connected(self) -> None:
        """Connect on first use, starting a resident when none answers and `start` allows."""
        if self._writer is not None:
            return
        async with self._connecting:
            if self._writer is not None:
                return
            if self._binary is None or self._config is None or self._path is None:
                raise TransportError(
                    "the transport is not open; use `async with Client(...)`, or await "
                    "transport.open(config) first"
                )
            await self._connect(self._binary, self._config, self._path)

    async def _connect(self, binary: str, config: ClientConfig, path: Path) -> None:
        try:
            reader, writer = await self._dial()
        except OSError as error:
            if not self._start:
                raise TransportError(
                    f"nothing answers on {path}: {error.strerror or error}; start a resident "
                    f"with `onemessagebus serve --resident --socket {path}`, or pass start=True"
                ) from error
            reader, writer = await self._launch(binary, config, path)
        self._writer, self._closed = writer, None
        self._listener = asyncio.ensure_future(self._listen(reader))

    async def _launch(
        self, binary: str, config: ClientConfig, path: Path
    ) -> tuple[asyncio.StreamReader, asyncio.StreamWriter]:
        argv = ["serve", "--resident", "--socket", str(path)]
        defaults = {
            "config": config.config,
            "transportDir": config.transport_dir,
            "registry": config.registry,
        }
        for binding in capability("serve").bindings:
            if (value := defaults.get(binding.option)) is not None:
                argv += [binding.flag, os.fspath(value)]
        process = await _spawn(binary, argv, config, stdin=False)
        self._process = process
        self._said.clear()
        self._drain = asyncio.ensure_future(self._gather(process))
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self._start_timeout
        while True:
            try:
                connection = await self._dial()
            except OSError as error:
                if process.returncode is not None:
                    await self._forget()
                    said = refusal_text("\n".join(self._said), process.returncode)
                    raise TransportError(
                        f"the resident core exited {process.returncode} before it listened on "
                        f"{path}: {said}"
                    ) from error
                if loop.time() > deadline:
                    await self._stop_started()
                    raise TransportError(
                        f"the resident core did not listen on {path} within "
                        f"{self._start_timeout} seconds; see what it said: {list(self._said)}"
                    ) from error
                await asyncio.sleep(0.02)
                continue
            if not await self._answered_by(process, path, deadline):
                # Another client's resident won the socket first; this one exits 1
                # naming it, and closing must not stop the one that answers.
                await asyncio.wait_for(process.wait(), _STOP_WAIT)
                await self._forget()
            return connection

    async def _answered_by(
        self, process: asyncio.subprocess.Process, path: Path, deadline: float
    ) -> bool:
        """Whether the resident answering on `path` is the one `process` is.

        A resident binds its socket and then records its pid beside it, so a
        connection can reach the winner of a race before the winner's pid is on
        disk; and the loser exits 1 naming the winner. Until one of those is seen
        the answer is unknown, and a resident that records nothing by the start
        deadline is the one this transport spawned, since nothing else claims it.
        """
        loop = asyncio.get_running_loop()
        while True:
            owner = _owner(path)
            if owner is not None:
                return owner == process.pid
            if process.returncode is not None:
                return False
            if loop.time() > deadline:
                return True
            await asyncio.sleep(0.02)

    async def _gather(self, process: asyncio.subprocess.Process) -> None:
        async for raw in cast("asyncio.StreamReader", process.stderr):  # sound: _spawn pipes stderr
            self._said.append(raw.decode("utf-8", "replace").rstrip("\r\n"))

    async def _forget(self) -> None:
        if self._drain is not None:
            await asyncio.wait({self._drain})
        self._process, self._drain = None, None

    async def _listen(self, reader: asyncio.StreamReader) -> None:
        reason: BusError = TransportError(
            f"the resident core on {self._path} closed the connection; it was stopped, or its "
            "socket was removed"
        )
        try:
            while raw := await reader.readline():
                try:
                    line = ResidentLine.model_validate_json(raw).root
                except ValidationError:
                    reason = ContractError(
                        f"the resident core on {self._path} wrote a line bus.resident-protocol@1 "
                        f"does not admit: {raw[:200]!r}"
                    )
                    break
                if isinstance(line, (ResidentRequest, ResidentCancel)):
                    reason = ContractError(
                        f"the resident core on {self._path} wrote a client's line: {raw[:200]!r}"
                    )
                    break
                if line.id is None:
                    reason = ContractError(
                        f"the resident core refused a line this client wrote: {raw[:200]!r}"
                    )
                    break
                if (waiter := self._pending.get(line.id)) is not None:
                    waiter.put_nowait(line)
        except (OSError, ValueError) as error:
            reason = TransportError(f"reading from the resident core on {self._path}: {error}")
        finally:
            self._closed = reason
            for waiter in self._pending.values():
                waiter.put_nowait(reason)

    def _register(self) -> tuple[int, asyncio.Queue[Line | BusError]]:
        if self._writer is None:
            raise TransportError(
                "the transport is not open; use `async with Client(...)`, or await "
                "transport.open(config) first"
            )
        if self._closed is not None:
            raise type(self._closed)(self._closed.message)
        request_id = next(self._ids)
        waiter: asyncio.Queue[Line | BusError] = asyncio.Queue()
        self._pending[request_id] = waiter
        return request_id, waiter

    async def _send(self, line: ResidentRequest | ResidentCancel) -> None:
        writer = self._writer
        if writer is None or self._closed is not None:
            raise TransportError(f"the connection to the resident core on {self._path} is closed")
        writer.write(line.model_dump_json(exclude_none=True).encode("utf-8") + b"\n")
        try:
            await writer.drain()
        except OSError as error:
            raise TransportError(
                f"writing to the resident core on {self._path}: {error}"
            ) from error

    async def call(self, capability: str, args: Mapping[str, Any], input: str | None) -> Any:
        await self._connected()
        request_id, waiter = self._register()
        try:
            await self._send(_request(request_id, capability, args, input))
            line = await waiter.get()
        finally:
            self._pending.pop(request_id, None)
        if isinstance(line, ResidentEvent):
            raise ContractError(
                f"the resident core answered request {request_id} ({capability}) with an event line"
            )
        return _answer(line)

    async def stream(
        self, capability: str, args: Mapping[str, Any], input: str | None
    ) -> AsyncGenerator[Any, None]:
        await self._connected()
        request_id, waiter = self._register()
        finished = False
        try:
            await self._send(_request(request_id, capability, args, input))
            while True:
                line = await waiter.get()
                if isinstance(line, ResidentEvent):
                    yield line.event
                    continue
                finished = True
                _answer(line)
                return
        finally:
            if not finished:
                await self._cancel(request_id, waiter)
            self._pending.pop(request_id, None)

    async def _cancel(self, request_id: int, waiter: asyncio.Queue[Line | BusError]) -> None:
        """Write the cancel line, and wait until the resident says the stream ended."""
        with contextlib.suppress(BusError, asyncio.TimeoutError):
            await self._send(ResidentCancel(id=request_id, cancel=True))
            while isinstance(await asyncio.wait_for(waiter.get(), _STOP_WAIT), ResidentEvent):
                pass

    async def close(self) -> None:
        writer, self._writer = self._writer, None
        if writer is not None:
            writer.close()
            with contextlib.suppress(OSError):
                await writer.wait_closed()
        listener, self._listener = self._listener, None
        if listener is not None:
            listener.cancel()
            await asyncio.wait({listener})
        self._closed = None
        await self._stop_started()

    async def _stop_started(self) -> None:
        process = self._process
        if process is None:
            return
        if self._path is not None:
            with contextlib.suppress(FileNotFoundError):
                self._path.unlink()
        try:
            await asyncio.wait_for(process.wait(), self._stop_timeout)
        except asyncio.TimeoutError:
            # A resident that never notices its socket went is ended as any process is.
            await _stop(process, self._stop_timeout)
        await self._forget()


def _request(
    request_id: int, method: str, args: Mapping[str, Any], input: str | None
) -> ResidentRequest:
    """The request line for one capability, refused by name when the manifest has no such verb.

    Validated rather than constructed: the generated line types the verb as the
    protocol does, and a verb is only known to be one once the manifest says so.
    """
    capability(method)
    fields = {"id": request_id, "verb": method, "args": dict(args), "input": input}
    return ResidentRequest.model_validate(fields)


def _answer(line: ResidentAnswer | ResidentFailure | BusError) -> Any:
    if isinstance(line, BusError):
        raise type(line)(line.message)
    if isinstance(line, ResidentFailure):
        raise refusal(line.error.exit, line.error.message, line.error.output)
    return line.ok


def _owner(socket: Path) -> int | None:
    """The pid a resident records beside its socket, when it recorded a readable one."""
    try:
        return int(Path(f"{socket}.pid").read_text("utf-8").split()[0])
    except (OSError, ValueError, IndexError):
        return None
