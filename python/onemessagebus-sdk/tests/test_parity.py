"""The client against the capability manifest: every method, every option, every flag."""

from __future__ import annotations

import inspect
import re
import types
import typing
from collections.abc import AsyncGenerator, Mapping
from pathlib import Path
from typing import Any

import pytest
from pydantic import BaseModel

from onemessagebus import BusRefused, Client, ClientConfig, Identity, render_argv
from onemessagebus._generated import contract
from onemessagebus._manifest import CAPABILITIES, Binding, Capability, snake_case
from onemessagebus._pin import pinned_cli_version

CLIENT_SOURCE = Path(__file__).resolve().parents[1] / "src" / "onemessagebus" / "_client.py"
# The parity gate's own expression for a method of `class Client`.
METHOD = re.compile(r"^ {4}(?:async )?def ([a-z][a-z0-9_]*)\s*\(", re.MULTILINE)
# The parameter each stdin-reading method takes its payload by.
PAYLOADS = {
    "schemaCheck": "payload",
    "eventsEmit": "payload",
    "deliver": "payload",
    "send": "message",
    "reply": "reply",
    "validate": "message",
    "ask": "question",
    "serve": "frames",
}
# Parameters that are the SDK's rather than an option of the verb.
EXTRAS = {"next": {"type"}}


def options_model(entry: Capability) -> type[BaseModel]:
    assert entry.options is not None
    return getattr(contract, "".join(part.title() for part in entry.options.split("_")))


def client_body() -> str:
    source = CLIENT_SOURCE.read_text(encoding="utf-8")
    body = source.split("\nclass Client", 1)[1].split("\n", 1)[1]
    return re.split(r"\n(?=\S)", body, maxsplit=1)[0]


def test_client_has_exactly_one_public_method_per_capability() -> None:
    declared = set(METHOD.findall(client_body()))
    assert declared == {snake_case(method) for method in CAPABILITIES}
    assert len(CAPABILITIES) == 17


@pytest.mark.parametrize("method", sorted(CAPABILITIES))
def test_each_method_takes_its_options_root_and_its_payload(method: str) -> None:
    entry = CAPABILITIES[method]
    fields = options_model(entry).model_fields
    parameters = dict(inspect.signature(getattr(Client, snake_case(method))).parameters)
    parameters.pop("self")
    expected = set(fields) | EXTRAS.get(method, set())
    if entry.stdin:
        expected.add(PAYLOADS[method])
    assert set(parameters) == expected
    assert (method in PAYLOADS) == entry.stdin
    assert {snake_case(binding.option) for binding in entry.bindings} == set(fields)
    for name, field in fields.items():
        if field.is_required():
            positional = name not in KEYWORD_ONLY.get(method, set())
            kind = parameters[name].kind
            assert (kind is inspect.Parameter.POSITIONAL_OR_KEYWORD) == positional, name
            assert parameters[name].default is inspect.Parameter.empty, name


# Required options the contract fixes as keyword-only: `subscribe(queue, *, until)`.
KEYWORD_ONLY = {"subscribe": {"until"}}


class Recorded(Exception):
    """What a call handed its transport."""

    def __init__(self, capability: str, options: Mapping[str, Any], payload: str | None) -> None:
        super().__init__(capability)
        self.capability = capability
        self.options = dict(options)
        self.payload = payload


class RecordingTransport:
    """A transport that records the call and answers nothing: the client is under test."""

    async def open(self, config: ClientConfig) -> Identity:
        version = pinned_cli_version()
        assert version is not None
        return Identity("recording", version)

    async def call(self, capability: str, args: Mapping[str, Any], input: str | None) -> Any:
        raise Recorded(capability, args, input)

    async def stream(
        self, capability: str, args: Mapping[str, Any], input: str | None
    ) -> AsyncGenerator[Any, None]:
        raise Recorded(capability, args, input)
        yield  # pragma: no cover - makes this an async generator

    async def close(self) -> None:
        return None


def sample(name: str, annotation: Any) -> Any:
    """A value of the option's own type that renders visibly."""
    kinds = set(typing.get_args(annotation)) | {typing.get_origin(annotation) or annotation}
    for kind in list(kinds):
        if isinstance(kind, types.GenericAlias) or typing.get_origin(kind) is not None:
            kinds.add(typing.get_origin(kind))
    literals = [
        value
        for kind in kinds
        if typing.get_origin(kind) is typing.Literal
        for value in typing.get_args(kind)
    ]
    if typing.get_origin(annotation) is typing.Literal:
        literals += list(typing.get_args(annotation))
    if name == "format":
        return "text"
    if literals:
        return literals[0]
    if bool in kinds:
        return True
    if int in kinds:
        return 7
    if list in kinds:
        return ["first.jsonl", "-dash-led.jsonl"]
    if dict in kinds:
        return {"run_id": "R", "round": 2}
    return f"{name}-value"


@pytest.mark.parametrize("method", sorted(CAPABILITIES))
async def test_every_bound_option_renders_its_flag_or_positional(method: str) -> None:
    entry = CAPABILITIES[method]
    fields = options_model(entry).model_fields
    values = {name: sample(name, field.annotation) for name, field in fields.items()}
    if entry.stdin:
        values[PAYLOADS[method]] = {"payload": True}
    client = Client(ClientConfig(), RecordingTransport())
    call = getattr(client, snake_case(method))

    async def drive() -> None:
        if method == "subscribe":
            async for _ in call(**values):
                pass  # pragma: no cover - the transport answers nothing
        else:
            await call(**values)

    with pytest.raises(Recorded) as recorded:
        await drive()
    assert recorded.value.capability == method
    argv = render_argv(method, recorded.value.options)
    assert argv[: len(entry.verb)] == list(entry.verb)
    positionals = argv[argv.index("--") + 1 :] if "--" in argv else []
    flags = argv[: argv.index("--")] if "--" in argv else argv
    for binding in entry.bindings:
        value = values[snake_case(binding.option)]
        if binding.kind == "positional":
            expected = [str(item) for item in value] if isinstance(value, list) else [str(value)]
            assert all(word in positionals for word in expected), (binding, argv)
        elif binding.kind == "switch":
            assert binding.flag in flags, (binding, argv)
        elif binding.kind == "key-value":
            for key, item in value.items():
                assert adjacent(flags, binding.flag, f"{key}={item}"), (binding, argv)
        else:
            assert adjacent(flags, binding.flag, word(value)), (binding, argv)
    if entry.stdin:
        assert recorded.value.payload is not None


def word(value: Any) -> str:
    return ("true" if value else "false") if isinstance(value, bool) else str(value)


def adjacent(argv: list[str], flag: str, value: str) -> bool:
    return any(argv[i] == flag and argv[i + 1] == value for i in range(len(argv) - 1))


def test_config_defaults_apply_only_where_the_caller_left_the_option_unset() -> None:
    client = Client(ClientConfig(config="c.yaml", transport_dir="dir", registry="reg"))
    assert client._args("send", {"queue": "q", "registry": "mine"}) == {
        "queue": "q",
        "config": "c.yaml",
        "transportDir": "dir",
        "registry": "mine",
    }
    # A capability that binds none of them takes none of them.
    assert client._args("transports", {}) == {}


def test_render_argv_refuses_what_no_binding_renders() -> None:
    with pytest.raises(BusRefused, match="`queues` is not an option of status"):
        render_argv("status", {"queues": "x"})
    with pytest.raises(BusRefused, match="`publish` is not a capability of onemessagebus"):
        render_argv("publish", {})
    with pytest.raises(BusRefused, match="ask: `blocking` is true or false, not str"):
        render_argv("ask", {"queue": "q", "blocking": "yes"})
    with pytest.raises(BusRefused, match="eventsEmit: `labels` is a mapping"):
        render_argv("eventsEmit", {"path": "p", "labels": "run_id=R"})
    with pytest.raises(BusRefused, match="status: `queue` takes a string, a number or a boolean"):
        render_argv("status", {"queue": {"name": "a"}})
    assert render_argv("ask", {"queue": "q", "blocking": False}) == ["ask", "--", "q"]
    assert render_argv("status", {"queue": Path("-q")}) == ["status", "--", "-q"]
    assert render_argv("eventsEmit", {"path": "p", "labels": {"ok": True}}) == [
        "events",
        "emit",
        "--label",
        "ok=true",
        "--",
        "p",
    ]


def test_a_repeated_binding_renders_its_flag_once_per_value(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # No capability binds a repeated flag today; the kind is the manifest's, so it renders.
    entry = Capability(
        method="tagged",
        verb=("tagged",),
        options=None,
        output=None,
        stdout="text",
        stdin=False,
        bindings=(Binding("tags", "--tag", "repeated"),),
    )
    monkeypatch.setitem(CAPABILITIES, "tagged", entry)  # type: ignore[arg-type]
    assert render_argv("tagged", {"tags": ["a", "b"]}) == ["tagged", "--tag", "a", "--tag", "b"]
    assert render_argv("tagged", {"tags": "one"}) == ["tagged", "--tag", "one"]
