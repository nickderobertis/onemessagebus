"""The installed Python SDK, driven against the binary installed beside it.

Run from a fresh virtualenv holding only what `pip install onemessagebus` puts
there, at the version a release would stamp: the SDK finds the binary its
`onemessagebus-cli` dependency installed, holds it to the version it pins, and
answers once over each transport.
"""

import asyncio
import sys
import tempfile
from pathlib import Path

import onemessagebus
from onemessagebus import Client, ClientConfig, ResidentTransport

SURFACE = {"kind": "finding", "message": "installed", "source": "proposal", "blocking": False}


async def main(expected: str) -> None:
    if onemessagebus.__version__ != expected:
        raise SystemExit(
            f"the installed SDK is {onemessagebus.__version__}, and this revision packs {expected}"
        )
    with tempfile.TemporaryDirectory() as scratch:
        config = ClientConfig(transport_dir=Path(scratch, "channel"))
        async with Client(config) as client:
            if not any(kind.kind == "local" for kind in await client.transports()):
                raise SystemExit("the installed binary lists no local transport")
            await client.send("surfaces", SURFACE)
        resident = ResidentTransport(Path(scratch, "bus.sock"))
        async with Client(config, transport=resident) as client:
            statuses = await client.status("surfaces")
            if statuses[0].records != 1:
                raise SystemExit(f"the resident core reads {statuses[0]} for what was sent")
    print(
        f"onemessagebus {expected}: the installed Python SDK drove the installed binary "
        "one-shot and through a resident core"
    )


asyncio.run(main(sys.argv[1]))
