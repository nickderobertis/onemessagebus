# onemessagebus

A typed async Python client for [onemessagebus](https://github.com/nickderobertis/onemessagebus):
one method per verb of the `onemessagebus` binary, carried either by the binary
once per call or by its resident core over a unix socket, with every wire shape a
Pydantic model generated from the Rust build.

## Install

```bash
pip install onemessagebus
```

The package pins the exact `onemessagebus-cli` release it was built with, so the
binary arrives with it. A client refuses a binary of any other version before its
first call (see [Version pin](#version-pin)).

## The client

A queue verb opens the queues a configuration file declares, such as this
`onemessagebus.yaml`:

```yaml
version: 1
transport: {kind: local, dir: runs/r1/bus}
queues:
  questions: {policy: {hold_pending: true, blocking_first: true}, answers: answers}
  answers: {numbered: true}
```

```python
import asyncio
from onemessagebus import Client, ClientConfig


async def main() -> None:
    config = ClientConfig(config="onemessagebus.yaml")
    async with Client(config) as client:
        [sent] = await client.send("questions", {"message": "which base?", "blocking": False})
        claimed = await client.next("questions")  # None when there is nothing to claim
        [status] = await client.status("questions")
        print(sent.position, claimed and claimed.record, status.records)


asyncio.run(main())
```

`Client` has exactly one async method per capability the binary declares, named
as the capability manifest names it in snake_case; the generated
[parity table](https://github.com/nickderobertis/onemessagebus/blob/main/docs/sdk-parity.md)
lists every one beside its command. Each takes that verb's options as snake_case
parameters (required ones positional) and, where the verb reads stdin, the payload:
a Pydantic model, a JSON-able value, or JSON text.
`format="text"` makes a reading method return the binary's text rendering.

`ClientConfig(binary, config, transport_dir, registry, cwd, env)`: `binary` is the
executable, else `ONEMESSAGEBUS_BIN`, else `onemessagebus` on `PATH`; `config`,
`transport_dir` and `registry` are defaults applied to every call whose verb takes
them and whose caller left them unset. `transport_dir` only moves the
configuration's transport: a queue verb given no `config` is refused.

A few answers are data rather than exceptions:

- `ask(...)` answers `Reply | Timeout | Abandoned | Refused`, told apart by
  `answer`; only a question the bus refuses as input raises.
- `validate(...)` answers `ValidatedPass | ValidatedRefuse | ValidatedUnjudged`;
  a refusal verdict is returned, not raised.
- `next(...)` answers `None` when the queue has nothing to claim.
- `subscribe(queue, until=...)` is an async iterator of `LogRecord`s that ends
  when the predicate holds. Close it early (`contextlib.aclosing`, or `aclose()`)
  and the stream stops: the resident is told to cancel, a spawned process ends.

## Message types

```python
from onemessagebus import Message


class Greeting(Message, schema="demo.greeting@1"):
    text: str


async with Client(ClientConfig(config="onemessagebus.yaml", registry="registry")) as client:
    await client.schema.register(Greeting)  # its JSON Schema, under its id
    await client.send("greetings", Greeting(text="hi"))
    claimed = await client.next("greetings", type=Greeting)
    assert claimed is not None and claimed.record == Greeting(text="hi")
```

The class keyword is the schema id, refused at class creation when malformed.
`Greeting.json_schema()` is the draft 2020-12 document registered. The bus
validates every payload in Rust against the registered schema; one that violates
it is refused as `BusFailed`, naming the id and the JSON pointer.

Every message the binary registers has a generated model in
`onemessagebus.models`, by its family's name: `TransportHello` is
`onemessagebus.transport-hello@1` at its latest version, `TransportHelloV1` that
version, and `ResidentProtocol` is `bus.resident-protocol@1`.
`onemessagebus.messages.MESSAGES` maps each id to its model.

## Transports

- `CliTransport()` (the default) spawns the binary once per call, with the argv
  the capability manifest renders (`onemessagebus.render_argv`).
- `ResidentTransport(socket, start=True)` speaks the resident protocol
  (`bus.resident-protocol@1`) over one unix socket connection shared by every
  call and subscription. When nothing answers on the socket, its first call
  starts `onemessagebus serve --resident --socket <socket>` with the config's
  `config`, `transport_dir` and `registry`, and leaving the client stops that
  resident, and only a resident it started, by removing its socket.

```python
async with Client(config, transport=ResidentTransport("bus.sock")) as client:
    ...
```

`Transport` is the protocol both implement (`open`, `call`, `stream`, `close`).

## Errors

Every error is a `BusError` with `exit`, `message` and `output`. `message` is the
binary's refusal text without its `onemessagebus: ` prefix, whichever transport
carried the call.

| error | when |
| --- | --- |
| `BusFailed` | exit 1: well-formed input whose answer is no |
| `BusRefused` | exit 2: input the verb refuses |
| `ContractError` | an answer the generated models reject |
| `VersionMismatch` | the binary is not the version this package pins |
| `TransportError` | the binary cannot be run, or the resident cannot be reached or started |

## Version pin

The package and `onemessagebus-cli` release together as one version. Before its
first call a client runs `<binary> --version` and raises `VersionMismatch`
naming both versions when they differ. A development checkout carries the
placeholder `0.0.0.dev0`, and pins the workspace version of the `Cargo.toml` it
sits in.

## Developing

The package's work goes through the repository's command surface, from its root:

```bash
just python-sdk-generate   # regenerate _generated/ and models.py from the Rust bundle
just python-sdk-check      # generate --check, format, ruff, ty, tests with the coverage floor, build
just typecheck             # ty here, beside every other project's type check
```

Each recipe runs the package's pinned environment: the repository's uv workspace,
synced from the root `uv.lock` into the root `.venv`, with this package installed
editable and its `onemessagebus-cli` dependency — the workspace's own
distribution — left unbuilt. The tests drive the checkout's
`target/debug/onemessagebus`, which `python-sdk-check` builds first. Re-resolving
that environment is a deliberate dependency change, made with
`just python-sdk-lock` and reviewed as one.

`_generated/` is never edited by hand: it is datamodel-code-generator's rendering
of the schema bundle the Rust build prints, plus the capability manifest as data.
`scripts/pack.py` writes a publishable copy under `dist/pack/` with every
placeholder version stamped from `Cargo.toml`, and prints its directory last.
