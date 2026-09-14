"""The capability manifest the Rust build declares, read as data.

Every verb the binary has is one entry of `_generated/capabilities.json`, and its
bindings are how an option reaches the command line. `render_argv` walks them
and nothing else, so a binding the Rust build changes changes the argv here
without an edit, and the resident request carries the same option names.
"""

from __future__ import annotations

import json
import os
import re
from collections.abc import Mapping
from dataclasses import dataclass
from importlib import resources
from typing import Any

from ._errors import BusRefused


@dataclass(frozen=True)
class Binding:
    """One option of a capability, and how it renders."""

    option: str
    flag: str
    kind: str


@dataclass(frozen=True)
class Capability:
    """One verb of the binary, as the manifest declares it."""

    method: str
    verb: tuple[str, ...]
    options: str | None
    output: str | None
    stdout: str
    stdin: bool
    bindings: tuple[Binding, ...]


def snake_case(name: str) -> str:
    """The Python spelling of a manifest name: `transportDir` → `transport_dir`."""
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def _load() -> dict[str, Capability]:
    text = (
        resources.files("onemessagebus._generated")
        .joinpath("capabilities.json")
        .read_text(encoding="utf-8")
    )
    return {
        entry["method"]: Capability(
            method=entry["method"],
            verb=tuple(entry["verb"]),
            options=entry["options"],
            output=entry["output"],
            stdout=entry["stdout"],
            stdin=entry["stdin"],
            bindings=tuple(Binding(b["option"], b["flag"], b["kind"]) for b in entry["bindings"]),
        )
        for entry in json.loads(text)
    }


#: Every capability, by its camelCase method.
CAPABILITIES: Mapping[str, Capability] = _load()


def capability(method: str) -> Capability:
    """The capability `method` names, refused by name when the manifest has none."""
    try:
        return CAPABILITIES[method]
    except KeyError:
        known = ", ".join(sorted(CAPABILITIES))
        raise BusRefused(
            f"`{method}` is not a capability of onemessagebus; it has {known}"
        ) from None


def _word(method: str, option: str, value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (str, int, float)):
        return str(value)
    if isinstance(value, os.PathLike):
        return os.fspath(value)
    raise BusRefused(
        f"{method}: `{option}` takes a string, a number or a boolean, not {type(value).__name__}"
    )


def _words(method: str, option: str, value: Any) -> list[str]:
    if isinstance(value, (list, tuple)):
        return [_word(method, option, item) for item in value]
    return [_word(method, option, value)]


def render_argv(capability_method: str, args: Mapping[str, Any]) -> list[str]:
    """The argv after the binary for one call: the verb, every flag, then `--` and positionals.

    `args` is keyed by the manifest's option names (camelCase), as a resident
    request's `args` are. An absent or `None` option renders nothing. Positionals
    go after `--`, so a dash-led value is still a positional.
    """
    entry = capability(capability_method)
    bound = {binding.option for binding in entry.bindings}
    for option in args:
        if option not in bound:
            raise BusRefused(f"`{option}` is not an option of {capability_method}")
    argv = list(entry.verb)
    positionals: list[str] = []
    for binding in entry.bindings:
        value = args.get(binding.option)
        if value is None:
            continue
        if binding.kind == "positional":
            positionals.extend(_words(capability_method, binding.option, value))
        elif binding.kind == "value":
            argv += [binding.flag, _word(capability_method, binding.option, value)]
        elif binding.kind == "repeated":
            for word in _words(capability_method, binding.option, value):
                argv += [binding.flag, word]
        elif binding.kind == "switch":
            if not isinstance(value, bool):
                raise BusRefused(
                    f"{capability_method}: `{binding.option}` is true or false, "
                    f"not {type(value).__name__}"
                )
            if value:
                argv.append(binding.flag)
        elif binding.kind == "key-value":
            if not isinstance(value, Mapping):
                raise BusRefused(
                    f"{capability_method}: `{binding.option}` is a mapping of keys to values, "
                    f"not {type(value).__name__}"
                )
            for key, item in value.items():
                argv += [binding.flag, f"{key}={_word(capability_method, binding.option, item)}"]
        else:  # pragma: no cover - the generator's manifest names no other kind
            raise AssertionError(f"{capability_method}: unknown binding kind {binding.kind!r}")
    if positionals:
        argv += ["--", *positionals]
    return argv
