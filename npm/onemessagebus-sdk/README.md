# @onemessagebus/sdk

The typed TypeScript client for [`onemessagebus`](https://github.com/nickderobertis/onemessagebus):
every capability of the `onemessagebus` binary as one method, over a subprocess
per call or over the resident core on a unix socket. Every option type, output
type and registered message type — and the Zod schema each response is parsed
by — is generated from the Rust build's schema bundle, so nothing here restates
a wire shape by hand.

## Install

```bash
npm install @onemessagebus/sdk
```

The package depends on `zod` and on `onemessagebus-cli` **at exactly its own
version**, which carries the prebuilt binary for your platform. Node 20 or later,
or Bun.

## The client

A queue verb opens the queues a configuration file declares, such as this
`onemessagebus.yaml`:

```yaml
version: 1
transport: {kind: local, dir: runs/r1/bus}
queues:
  questions: {policy: {hold_pending: true, blocking_first: true}, answers: answers}
  answers: {numbered: true}
  notes: {schema: agent.note@1}
```

```ts
import { Client } from "@onemessagebus/sdk";

await using client = new Client({ config: { config: "onemessagebus.yaml" } });

const [sent] = await client.send("questions", { message: "which base?", blocking: true });
const claimed = await client.next("questions");           // undefined when there is nothing to claim
const statuses = await client.status("questions");
const text = await client.status("questions", { format: "text" }); // a string
```

`new Client({ config?, transport? })`, where `config` is a `ClientConfig`:

| key | meaning |
| --- | --- |
| `binary` | the `onemessagebus` executable |
| `config`, `transportDir`, `registry` | defaults for every call whose capability takes that option and whose caller left it out; `transportDir` only moves the configuration's transport, so a queue verb given no `config` is refused |
| `cwd`, `env` | the binary's working directory, and variables added to its environment |

The binary is `config.binary`, else `ONEMESSAGEBUS_BIN`, else the installed
`onemessagebus-cli` package's launcher (run under the current runtime), else
`onemessagebus` on `PATH`.

There is exactly one method per capability, named as the capability manifest
names it; the generated
[parity table](https://github.com/nickderobertis/onemessagebus/blob/main/docs/sdk-parity.md)
lists every one beside its command. Each takes its generated options type
(camelCase) and, where the verb reads stdin, a payload that is sent as JSON. The
shapes worth knowing:

```ts
await client.send(queue, message, options?);                  // Sent[]
await client.next(queue, { consumer, asker, type });          // Claimed<T> | undefined
await client.reply(queue, correlation, reply, options?);      // Replied
await client.validate(queue, message, options?);              // Validated: a refusal is a verdict, not a throw
const answer = await client.ask(queue, question, { blocking: true, asker: "worker-1", timeout: 300 });
switch (answer.answer) {                                      // "reply" | "timeout" | "abandoned" | "refused"
  case "reply": console.log(answer.reply); break;
}
for await (const record of client.subscribe("questions", { until: { field: "event", equals: "answered" }, timeout: 600 })) {
  console.log(record.position, record.record);                // leaving the loop stops the subscription
}
```

`client.schema` registers, checks and lists schemas: `register(messageOrSchema, id?)`,
`check(idOrMessage, payload)` and `list()`.

## Message types

```ts
import { z } from "zod";
import { defineMessage, type MessageType } from "@onemessagebus/sdk";

const Greeting = defineMessage("demo.greeting@1", z.object({ text: z.string() }));
type Greeting = MessageType<typeof Greeting>;

await client.schema.register(Greeting);                       // its JSON Schema, under its id
await client.send("greetings", { text: "hi" }, { type: Greeting }); // validated here, and again by the core
const claimed = await client.next("greetings", { type: Greeting });  // claimed.record is a Greeting
```

A queue whose `schema` names `demo.greeting@1` in the configuration, with the
same `registry`, validates every record sent to it against that schema in Rust. A
violation is a `BusFailed` naming the id and the JSON pointer:
`demo.greeting@1: at /text: ...`. `.jsonSchema()` is the canonical draft 2020-12
document (from `z.toJSONSchema`), `.parse(value)` validates.

The messages the Rust registry already holds are generated too, as definitions
you pass as `type`. `schemas` names each by its family without the namespace, at
its latest version and at every version:

```ts
import { schemas, type MessageType } from "@onemessagebus/sdk";

const claimed = await client.next("notes", { type: schemas.Note }); // agent.note@1
type Note = MessageType<typeof schemas.Note>;
schemas.EventEnvelope;    // agent.event-envelope@2; schemas.EventEnvelopeV1 is @1
schemas.Note.schema;      // the Zod schema itself
```

`type` also takes a bare Zod schema; a violation is then reported as the
payload's (`payload: at /text: ...`). `messages.AgentNoteV1`,
`messages.AgentNoteV1Schema` and `messages.MESSAGES` (by id) are the
same definitions by their full names.

## Transports

The client never spawns or connects itself; a `Transport` does.

- **`CliTransport`** (the default) runs the binary once per call, its argv
  rendered from the capability manifest (`renderArgv(capability, args)` is
  exported), and maps the exit code to a result or a typed error.
- **`ResidentTransport({ socket, start = true })`** speaks the resident protocol
  (`bus.resident-protocol@1`) to `onemessagebus serve --resident` over one unix
  socket connection, shared by concurrent calls and running subscriptions. When
  nothing answers on the socket and `start` is true it starts a resident with the
  client's `config`, `transportDir` and `registry`, and `close()` stops only a
  resident it started (by removing its socket). Leaving a subscription early
  sends the protocol's cancel line.

```ts
await using client = new Client({
  config: { config: "onemessagebus.yaml" },
  transport: new ResidentTransport({ socket: "/tmp/bus.sock" }),
});
```

Dispose of the client (`await using`, or `await client.transport.close()`) when
done.

## Errors

Every error is a `BusError`, carrying the CLI's own refusal text as `message`,
the exit code as `exit`, and any document printed before the refusal as `output`.

| error | when |
| --- | --- |
| `BusFailed` | exit 1: well-formed input whose answer is no (a schema violation, a subscribe timeout) |
| `BusRefused` | exit 2: refused input (an undeclared queue, a malformed option — checked by the generated options schema before any call) |
| `ContractError` | a response the generated schema rejects |
| `VersionMismatch` | the binary is not the version this SDK drives; it names both |
| `TransportError` | a binary that will not start, a socket nothing answers on — with what to do |

`next` answers `undefined` for an empty queue, and `ask` and `validate` answer
their printed document for an exit 1 rather than throwing.

## The version pin

A published SDK drives exactly one CLI version, its own: `SDK_VERSION` and
`CLI_VERSION` are stamped at pack time from the repository's `Cargo.toml`. Before
its first call a client runs `<binary> --version`, and rejects with
`VersionMismatch` when the versions differ:

```
this onemessagebus SDK drives onemessagebus-cli 0.3.0, and /usr/local/bin/onemessagebus reports 0.2.0; install onemessagebus-cli@0.3.0
```

In a development checkout the constants are still the `0.0.0-dev` placeholder,
and the pin is the checkout's own workspace version.

## Developing

From the repository root, through its command surface:

```bash
just bootstrap      # installs this package's locked dependencies with the rest of the tree
just node-sdk-generate   # rewrite src/generated from the Rust bundle (runs cargo)
just node-sdk-check      # every tier of this package: generate-check, format, lint, typecheck, tests, build
```

The tests drive the real binary (`cargo build -p onemessagebus-cli --locked`
builds it). Packing and the installed-package journey belong to the
`onemessagebus-sdk-install-e2e` project, which stamps this package with
`scripts/pack.mjs`, installs the tarball beside the CLI and drives it under node.

In a checkout the package is a member of the root npm workspace, installed from
its one `package-lock.json`, and declares no `onemessagebus-cli` dependency — it
drives the built binary — so installing never fetches an unreleased version;
`scripts/pack.mjs` adds an exact dependency on the stamped version.
A generated-schema construct the Zod generator does not enforce fails generation
by keyword and JSON pointer rather than being dropped.
