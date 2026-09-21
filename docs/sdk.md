# SDKs

Two language packages put the bus in reach of a program that is not Rust: the
Python SDK (`pip install onemessagebus`, imported as `onemessagebus`) and the
TypeScript SDK (`npm install @onemessagebus/sdk`). Neither reimplements the bus.
Each is a typed client over the `onemessagebus` binary — the Rust core — which it
reaches over IPC: a one-shot verb spawns the binary, and subscriptions and hot
paths speak to a resident core over a unix socket. Dispatch, validation, storage
and delivery stay the core's, whichever language a message type was declared in.

## One client, one method per capability

Every capability of the binary — `onemessagebus::CAPABILITIES` — is exactly one
client method, named as the manifest names it: camelCase in TypeScript
(`schemaList`, `eventsEmit`, `send`, `next`, …) and snake_case in Python
(`schema_list`, `events_emit`, `send`, `next`, …). A method takes its verb's
options, spelled as the verb's options root spells them, and renders them to argv
through the manifest's bindings, so a method can set every flag its verb takes and
no flag its verb does not. `parity/sdk-coverage.mjs` fails the build when a
capability has no method in either client, or a client has a method no capability
names.

```python
from onemessagebus import Client, ClientConfig

async with Client(ClientConfig(config="onemessagebus.yaml")) as client:
    sent = await client.send("greetings", {"text": "hi"})
    claimed = await client.next("greetings", consumer="reader")
```

```ts
import { Client } from "@onemessagebus/sdk";

const client = new Client({ config: { config: "onemessagebus.yaml" } });
const sent = await client.send("greetings", { text: "hi" });
const claimed = await client.next("greetings", { consumer: "reader" });
```

A client's configuration names the binary (or `ONEMESSAGEBUS_BIN`, or the
`onemessagebus-cli` package the SDK depends on, or `PATH`), and the
configuration file, transport directory and registry directory every call takes
when it names none of its own.

What each method answers is typed by the generated contract:

- `send` answers where each record landed, `next` the record claimed — or
  nothing when the queue has nothing to claim — and `reply` what it answered and
  appended.
- `ask` answers a tagged union discriminated on `answer`: `reply` (carrying the
  reply record), `timeout`, `abandoned` or `refused` (carrying why). A question
  that timed out is an answer, not an exception.
- `validate` answers the verdict — `pass`, `refuse` or `unjudged` with its reason
  — as data, whichever it is.
- `subscribe` is an async iterator of log records, ending when its predicate
  admits one; leaving the loop early stops the subscription.
- `client.schema.register(...)`, `.check(...)` and `.list()` are the registry's
  verbs under one name.

Every refusal is a typed error carrying the command line's own words: `BusRefused`
for input the verb refuses (exit 2), `BusFailed` for well-formed input whose
answer is no (exit 1), both `BusError`s with the exit code, the message and any
document the verb printed; `ContractError` for an answer the generated contract
rejects; `TransportError` for a binary that cannot be spawned or a socket that
cannot be reached; and `VersionMismatch`, below.

## Message types in the language of choice

A message type is declared once, in whichever language the program is written
in, and registered with the core's registry through `schema register`. From then
on the core validates every record pushed onto a queue whose `schema` names it —
in Rust — and a program in another language reads the same message typed by its
own declaration of the same id.

```python
import onemessagebus

class Greeting(onemessagebus.Message, schema="demo.greeting@1"):
    text: str

await client.schema.register(Greeting)
await client.send("greetings", Greeting(text="hello"))
claimed = await client.next("greetings", type=Greeting)  # claimed.record is a Greeting
```

```ts
import { defineMessage } from "@onemessagebus/sdk";
import { z } from "zod";

const Greeting = defineMessage("demo.greeting@1", z.object({ text: z.string() }));

await client.schema.register(Greeting);
await client.send("greetings", Greeting.parse({ text: "hello" }));
const claimed = await client.next("greetings", { type: Greeting }); // typed record
```

A Python message is a Pydantic model and a TypeScript one a Zod schema; each
emits its JSON Schema (draft 2020-12), which is the document the core registers.
A malformed id is refused where the type is declared. A record the registered
schema refuses is refused by the core naming the schema id and the JSON pointer,
and is appended nowhere. The registry directory is read again on every request,
so a type registered while a resident core runs is one the next `send` validates
by.

Every schema the binary itself registers — the agent profile's messages, and the
resident protocol's — is generated into both packages as a model, so a message
Rust declared with `schemars` reads typed in Python and TypeScript too.

## Transports

The client never spawns a process or opens a socket itself; a transport does,
behind one interface, so a later in-process backend is another transport rather
than another client.

- **The CLI transport** (the default) runs `onemessagebus <verb> ...` once per
  call, writes the payload to its stdin, and reads its stdout and exit code.
- **The resident transport** speaks to `onemessagebus serve --resident --socket
  <path>`. Given a socket nothing answers on, it starts the resident core itself
  with the client's configuration, and stops only a core it started when it is
  closed. Many calls and a running subscription share its one connection.

## The resident protocol

The resident core holds the configured transport open and answers one JSON
object per line on its socket. Its schema is a registered document,
`bus.resident-protocol@1` (`onemessagebus schema gen --lang json
bus.resident-protocol@1`), and both SDKs generate their protocol types from it.

- A request is `{"id": <n>, "verb": "<method>", "args": {...}, "input": "..."}`:
  `verb` is a capability's method, `args` its options keyed as its options root
  keys them, and `input` the bytes the verb would read on stdin. The verb set is
  `CAPABILITIES` and no other, so the parity gate that holds the clients holds the
  socket too.
- It is answered by one `{"id": <n>, "ok": <output>}` — the document, the list of
  lines or the text the verb prints — or one `{"id": <n>, "error": {"exit",
  "message", "output"}}`, carrying the exit code the command line's table gives
  the refusal, its words, and any document the verb printed first.
- A `subscribe` request streams `{"id": <n>, "event": {"position", "record"}}`
  lines, then ends with `"ok": "until"` when its predicate holds, or `"ok":
  "cancelled"` once `{"id": <n>, "cancel": true}` names it or the connection
  closes.
- A request naming no `config`, `transportDir` or `registry` takes the resident's
  own. A second resident on a socket a live one answers on is refused with exit 1
  naming the live one's pid; removing the socket stops a resident cleanly.

## The version each package drives

Each package pins the exact `onemessagebus-cli` release it drives — the version of
the release that published it — and refuses to run against any other before its
first call, naming both: the version it drives and the version the binary it found
reports. In a development checkout the pin is the checkout's own `Cargo.toml`
version, so the SDK drives the binary built beside it.

## The generated contract

No wire shape is restated by hand in either package. Both generate from the SDK
bundle `onemessagebus-cli`'s `sdk_bundle` example prints — the capability
manifest, every contract root (among them `config`, the configuration file;
`schema_bundle`, the document a configuration's `schemas` links serve; and
`layout`, one layout that document declares as data), every options root and
every registered message:
Python as Pydantic v2 models through `datamodel-code-generator`, TypeScript as
declarations through `json-schema-to-typescript` with Zod schemas emitted from the
same documents. Each package's generate-check fails when the committed output is
not a fresh generation.

| to | run |
| --- | --- |
| regenerate the TypeScript contract | `just node-sdk-generate` |
| regenerate the Python contract | `just python-sdk-generate` |
| run a package's tiers | `just node-sdk-check`, `just python-sdk-check` |
| check parity | `just sdk-coverage` |
| regenerate `docs/sdk-parity.md` | `just parity-audit` |
