"""What a client is configured with: where the binary is, and the defaults every call takes."""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass

from ._pin import resolve_binary
from ._types import StrPath


@dataclass(frozen=True)
class ClientConfig:
    """How a client reaches the bus.

    `binary` is the onemessagebus executable, else `ONEMESSAGEBUS_BIN`, else the
    one on `PATH`. `config`, `transport_dir` and `registry` are defaults: each is
    applied to every call whose capability takes that option and whose caller did
    not set it, and a resident this client starts is started with them. `cwd` and
    `env` are the spawned binary's working directory and environment additions.
    """

    binary: StrPath | None = None
    config: StrPath | None = None
    transport_dir: StrPath | None = None
    registry: StrPath | None = None
    cwd: StrPath | None = None
    env: Mapping[str, str] | None = None

    def environment(self) -> dict[str, str]:
        """The spawned binary's environment: this process's, with `env` over it."""
        return {**os.environ, **(self.env or {})}

    def binary_path(self) -> str:
        """The executable this configuration names, refused with the next action when none is."""
        return resolve_binary(self.binary, self.environment())
