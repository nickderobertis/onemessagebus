"""The errors the SDK raises, each carrying the bus's own words.

A refusal the binary gave keeps its exit code and the text it wrote after
`onemessagebus: `, whichever transport carried it, so a caller branches on the
type and reads the same message a person at the command line would.
"""

from __future__ import annotations

from typing import Any

# The code the command line exits with for well-formed input whose answer is no.
FAILED = 1
# The code the command line exits with for input it refuses.
REFUSED = 2


class BusError(Exception):
    """Anything the bus, or the way to it, did not do.

    `exit` is the command line's exit code when the binary gave one, `message`
    its refusal text, and `output` the document it printed before refusing
    (`ask`'s answer, `validate`'s verdict) when it printed one.
    """

    def __init__(self, message: str, *, exit: int | None = None, output: Any = None) -> None:
        super().__init__(message)
        self.exit = exit
        self.message = message
        self.output = output


class BusFailed(BusError):
    """Exit 1: well-formed input whose answer is no."""

    def __init__(self, message: str, *, output: Any = None) -> None:
        super().__init__(message, exit=FAILED, output=output)


class BusRefused(BusError):
    """Exit 2: input the verb refuses."""

    def __init__(self, message: str, *, output: Any = None) -> None:
        super().__init__(message, exit=REFUSED, output=output)


class ContractError(BusError):
    """A response the generated models reject: the binary and the SDK disagree on a shape."""


class VersionMismatch(BusError):
    """The binary is not the onemessagebus-cli version this SDK was released with."""

    def __init__(self, message: str, *, expected: str | None, reported: str | None) -> None:
        super().__init__(message)
        self.expected = expected
        self.reported = reported


class TransportError(BusError):
    """The binary could not be spawned, or the resident core could not be reached."""


def refusal(exit: int, message: str, output: Any = None) -> BusError:
    """The typed error for a refusal the binary gave with `exit`."""
    if exit == FAILED:
        return BusFailed(message, output=output)
    if exit == REFUSED:
        return BusRefused(message, output=output)
    return BusError(message, exit=exit, output=output)
