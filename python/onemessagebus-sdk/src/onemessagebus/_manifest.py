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
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from importlib import resources
from typing import Any, Literal, TypeGuard, get_args

from ._errors import BusRefused, ContractError

#: How a binding renders: the closed set this SDK renders, refused past at load.
Kind = Literal["positional", "value", "repeated", "switch", "key-value"]
KINDS: frozenset[str] = frozenset(get_args(Kind))

#: How a verb's stdout reads: the closed set this SDK parses, refused past at load.
Stdout = Literal["json", "jsonl", "text"]
STDOUTS: frozenset[str] = frozenset(get_args(Stdout))


def _is_kind(word: str) -> TypeGuard[Kind]:
    return word in KINDS


def _is_stdout(word: str) -> TypeGuard[Stdout]:
    return word in STDOUTS


@dataclass(frozen=True)
class Binding:
    """One option of a capability, and how it renders."""

    option: str
    flag: str
    kind: Kind


@dataclass(frozen=True)
class Capability:
    """One verb of the binary, as the manifest declares it."""

    method: str
    verb: tuple[str, ...]
    options: str | None
    output: str | None
    stdout: Stdout
    stdin: bool
    bindings: tuple[Binding, ...]


def snake_case(name: str) -> str:
    """The Python spelling of a manifest name: `transportDir` → `transport_dir`."""
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def _binding(method: str, declared: Mapping[str, str]) -> Binding:
    kind = declared["kind"]
    if not _is_kind(kind):
        raise ContractError(
            f"{method}: the manifest binds `{declared['option']}` as a {kind!r} flag, which this "
            f"SDK does not render; it renders {', '.join(sorted(KINDS))}. Regenerate the SDK "
            "from the bundle of the binary it drives"
        )
    return Binding(declared["option"], declared["flag"], kind)


def _stdout(method: str, word: str) -> Stdout:
    if not _is_stdout(word):
        raise ContractError(
            f"{method}: the manifest reads its stdout as {word!r}, which this SDK does not parse; "
            f"it parses {', '.join(sorted(STDOUTS))}. Regenerate the SDK from the bundle of the "
            "binary it drives"
        )
    return word


def read_manifest(text: str) -> dict[str, Capability]:
    """The capabilities a manifest declares, by method.

    A binding kind this SDK cannot render, and a stdout shape it cannot parse,
    are refused here, once, so everything past the load holds a closed set.
    """
    return {
        entry["method"]: Capability(
            method=entry["method"],
            verb=tuple(entry["verb"]),
            options=entry["options"],
            output=entry["output"],
            stdout=_stdout(entry["method"], entry["stdout"]),
            stdin=entry["stdin"],
            bindings=tuple(_binding(entry["method"], b) for b in entry["bindings"]),
        )
        for entry in json.loads(text)
    }


def _generated(name: str) -> str:
    return resources.files("onemessagebus._generated").joinpath(name).read_text(encoding="utf-8")


#: Every capability, by its camelCase method.
CAPABILITIES: Mapping[str, Capability] = read_manifest(_generated("capabilities.json"))

#: The profile the generated envelope models are written over, as the bundle names it.
VOCABULARY_NAME: str = json.loads(_generated("vocabulary.json"))["name"]


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
    match value:
        case bool():
            return "true" if value else "false"
        case str() | int() | float():
            return str(value)
        case os.PathLike():
            return os.fspath(value)
        case _:
            raise BusRefused(
                f"{method}: `{option}` takes a string, a number or a boolean, "
                f"not {type(value).__name__}"
            )


def _words(method: str, option: str, value: Any) -> list[str]:
    if isinstance(value, (list, tuple)):
        return [_word(method, option, item) for item in value]
    return [_word(method, option, value)]


# A renderer turns one option's value into the flags and the positionals it contributes.
Renderer = Callable[[str, Binding, Any], tuple[list[str], list[str]]]


def _positional(method: str, binding: Binding, value: Any) -> tuple[list[str], list[str]]:
    return [], _words(method, binding.option, value)


def _value(method: str, binding: Binding, value: Any) -> tuple[list[str], list[str]]:
    return [binding.flag, _word(method, binding.option, value)], []


def _repeated(method: str, binding: Binding, value: Any) -> tuple[list[str], list[str]]:
    words = _words(method, binding.option, value)
    return [word for item in words for word in (binding.flag, item)], []


def _switch(method: str, binding: Binding, value: Any) -> tuple[list[str], list[str]]:
    if not isinstance(value, bool):
        raise BusRefused(
            f"{method}: `{binding.option}` is true or false, not {type(value).__name__}"
        )
    return ([binding.flag] if value else []), []


def _key_value(method: str, binding: Binding, value: Any) -> tuple[list[str], list[str]]:
    if not isinstance(value, Mapping):
        raise BusRefused(
            f"{method}: `{binding.option}` is a mapping of keys to values, "
            f"not {type(value).__name__}"
        )
    pairs = [f"{key}={_word(method, binding.option, item)}" for key, item in value.items()]
    return [word for pair in pairs for word in (binding.flag, pair)], []


#: The renderer of every kind the manifest may bind; `read_manifest` refuses any other.
RENDERERS: Mapping[Kind, Renderer] = {
    "positional": _positional,
    "value": _value,
    "repeated": _repeated,
    "switch": _switch,
    "key-value": _key_value,
}


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
        flags, words = RENDERERS[binding.kind](capability_method, binding, value)
        argv += flags
        positionals += words
    if positionals:
        argv += ["--", *positionals]
    return argv
