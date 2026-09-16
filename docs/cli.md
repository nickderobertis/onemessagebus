# The command line

`onemessagebus` has the `schema` verbs, over the registry; the `schemas` verbs,
over the cache of linked schema bundles; the `events` verbs,
over NDJSON streams; `deliver` and `inbox carried`, over the inbox
(`docs/inbox.md`); `send`, `next`, `reply`, `subscribe` and `status` over
queues kept on a transport, with `transports` listing the transport kinds
(`docs/queues.md`, `docs/transport.md`); `ask`, a question and the answer that
echoes its correlation (`docs/ask.md`); `validate`, a record judged by a
queue's validators (`docs/validators.md`); and `serve`, a member protocol
over a queue (`docs/codecs.md`). Every verb is a capability in
`onemessagebus::CAPABILITIES`, which is what the SDK clients are generated from
and what the clap tree is held to, so a verb or flag here exists nowhere the
manifest does not say.

## Install

```bash
pip install onemessagebus-cli          # the wheel, no toolchain needed
npm install -g onemessagebus-cli       # the npm launcher, no toolchain needed
cargo install --git https://github.com/nickderobertis/onemessagebus onemessagebus-cli --locked
```

## Rules every verb keeps

- **Payloads arrive on stdin or `--file`, never as a positional argument.** The
  clap tree admits no positional a payload could be read as, so a payload passed
  as an argument is a usage error (exit 2) rather than a document nobody
  validated. A verb that takes no payload takes none. `deliver` also takes its
  message through the named `--message` option, and takes it from exactly one
  of the three.
- **`--profile <name>` chooses the vocabulary on both `events` verbs, and it
  defaults to `agent`** — the profile this binary links. `open` is the other:
  any source word, any labels, no dimensions. The source words and each source's
  write version come from the profile, chosen the same way on `emit` and on
  `merge`.
- **`--format json|text` on every reading verb.** JSON is the contract: one
  document, or one per line. Text is a deterministic rendering of the same
  content for a person, never separate content.
- **`--registry <dir>` / `ONEMESSAGEBUS_REGISTRY`** on every `schema` verb names
  a directory of registered documents added to the profile's own.
- **`--config <path>` on every `schema` verb** names a configuration whose
  `schemas` links are resolved and every linked document registered beside the
  profile's and the directory's. A `schema` verb reads
  it from the flag alone, never from the configuration variable the queue verbs
  read, so an environment set for them changes nothing here.
- **A verb that loads a configuration resolves its `schemas` links first.** Each
  link names a schema bundle, and every document of every bundle is registered
  beside the profile's schemas and the registry directory's, as the `schemas`
  section below says.
- **The queue verbs read one configuration.** `send`, `next`, `ask`, `reply`,
  `subscribe`, `status`, `validate` and `serve` take `--config <path>` (or `ONEMESSAGEBUS_CONFIG`),
  the `onemessagebus.yaml` naming the transport, the layout, added or overridden
  queues and narrowed authors, and `--transport-dir <dir>` (or
  `ONEMESSAGEBUS_TRANSPORT_DIR`), which replaces the file's `transport.dir` for
  one invocation: the flag wins over the variable, and the variable over the
  file. With no configuration at all, `--transport-dir` names the directory the
  `planner-channel` layout is kept in over a local transport; with neither, the
  verb is refused. A file with an unknown key, or one widening an author's
  grants, is refused naming the key. Each takes `--registry <dir>` (or
  `ONEMESSAGEBUS_REGISTRY`) too: the schemas that directory holds are registered
  beside the layout's, so a queue's `schema` may name one `schema register`
  recorded — a type an SDK declared in its own language — and every record pushed
  onto that queue is validated against it.

## Exit codes

| code | meaning |
| --- | --- |
| `0` | The verb did what it was asked. |
| `1` | Well-formed input whose answer is no: a payload that violates its schema, a stream that could not be written, a message a spool's receiver did not answer. |
| `2` | Input the verb refuses: a malformed id or filter, an unknown profile, an unsupported language, an unregistered id, a usage error. |

Refusals go to stderr as `onemessagebus: <what is wrong>`; stdout carries
answers only. A usage error of `ask`, `reply` or `validate` is refused the same
way, on one line naming the verb and its `--help`:

```bash
$ onemessagebus ask surfaces --timeout soon --transport-dir runs/r1/channel
onemessagebus: ask: invalid value 'soon' for '--timeout <SECONDS>': invalid digit found in string; see `onemessagebus ask --help`
```

The other verbs print clap's usage report for a usage error, at the same exit.

## `schema`

### `schema list [--registry DIR] [--config PATH] [--format json|text]`

Every registered id — the profile's and the registry directory's — and nothing
else. JSON is a list of `{id, family, version}`; text is one id per line.

### `schema check <id> [--file PATH] [--registry DIR] [--config PATH]`

Validate the payload on stdin (or in `--file`) against the schema registered
under `<id>`. Exit 0 when it conforms; exit 1 naming the id and the JSON
pointer of the first violation when it does not.

```bash
$ echo '{"run_id":"R","round":"two"}' | onemessagebus schema check agent.labels@1
onemessagebus: agent.labels@1: at /round: "two" is not of types "null", "integer"
```

### `schema gen --lang json|rust|python|typescript <id> [--registry DIR] [--config PATH]`

Render the schema registered under `<id>`: for `json`, the document itself,
byte for byte; for `rust`, a declaration that compiles with serde and schemars
and regenerates the same document. `python` and `typescript` are rendered by
the SDK packages, not by this build, and are refused by name here.

The names a document gives become Rust identifiers: a kebab-case or camelCase
property becomes a snake_case field and an enum value a PascalCase variant, each
renamed back to its wire spelling, and a keyword is written raw (`r#type`). A
title, `$defs` name, property name or enum value that still is not an
identifier — empty, starting with a digit, holding any character but an ASCII
letter, digit or `_`, or `self`, `Self`, `super` or `crate` — is refused with
exit 2 naming the id, its JSON pointer and the name, and so are two names that
become the same identifier.

```bash
$ onemessagebus schema gen --lang rust test.bad-property@1
onemessagebus: test.bad-property@1: cannot render /properties/a.b as Rust: the property name "a.b" is not a Rust identifier: '.' is not an ASCII letter, digit or underscore
```

### `schema register <id> --file <schema.json> [--registry DIR] [--config PATH]`

Record the JSON Schema document in `--file` under `<id>`, in the registry
directory, where every later invocation over the same `--registry` /
`ONEMESSAGEBUS_REGISTRY` lists it and checks against it. Registering the same
document again is fine; a different document under an id already held — the
profile's or the directory's — is refused naming the id, and nothing is written.

**The registry directory** holds one file per schema, named by its id —
`<dir>/agent.finding@1.json` — each a document `{"id": ..., "schema": ...}`, so
a file is self-describing and readable with nothing but `cat`. A file whose id
disagrees with its name is refused, and so is a `--registry` path that exists
but is not a directory.

## `schemas`

<!-- llmlint: ignore-block[no_redundant_instruction_pointers] the node that added schema links was required to state Contract L once, in docs/schema-links.md, and to have this command-line reference point at it rather than restate it; this paragraph is that one pointer, so a reader of the verbs finds the cache's rules without a second copy that could drift. -->
The cache a configuration's `schemas` links resolve through. Contract L — the
bundle document, the link and its pin, the cache's rules, and each verb's output
— is `docs/schema-links.md`; the headings here are the verbs' synopses.
<!-- llmlint: ignore-end[no_redundant_instruction_pointers] -->

### `schemas [--format json|text]`

List the cache: its directory, and each cached location and declared version
with when its origin last confirmed it. An empty or absent cache is an empty
list, exit 0.

### `schemas clear [--format json|text]`

Remove every entry of the cache and report how many there were; exit 0, also
when there were none.

### `schemas fetch [<link>...] [--config PATH] [--format json|text]`

Resolve each named link, or every link the configuration names (`--config` or
`ONEMESSAGEBUS_CONFIG`) when none is named, revalidating whatever the cache holds
regardless of its age, and report how each ended. Exit 0 when every link resolved
to a version its pin admits; otherwise exit 1 naming each link that did not, or
exit 2 when one of them served no bundle or a version its pin does not admit.

```bash
$ onemessagebus schemas fetch --config onemessagebus.yaml --format text
https://example.org/frames.json@8 fetched 8.1
```

## `events`

### `events merge <file>... [--filter SPEC] [--profile NAME] [--format json|text]`

Merge the stream files into one stream in `(ts, stream, seq)` order, on
stdout. `--filter` is a filter document: inline JSON when it starts with `{`,
otherwise a path to a YAML file. A filter that could not be honoured is refused:
a matcher naming no field, or naming an empty field, with the list, the index
and the matcher; a field the profile does not have, by that field's name.

A file whose final line is torn is not an error: every whole record is printed,
and the torn tail is reported on stderr with the byte it starts at, for its
writer to finish. A whole line that is not an envelope of the profile is
reported the same way and left out.

Text format renders one line per envelope:
`<ts> <source> <kind> stream=<stream> seq=<n> v=<v> [<dimension>=<value>]...
[<label>=<value>]... [payload=<json>] [artifacts=<json>]`.

### `events emit <file> --kind KIND --stream ID [--source WORD] [--profile NAME] [--label KEY=VALUE]... [--file PATH] [--format json|text]`

Append one envelope to the stream file `<file>` from the payload on stdin (or
in `--file`), and print the envelope written. The envelope is numbered from the
file under its lock — `Emitter::shared` — so several processes may append to
one file at once and leave one gapless series. `--kind` must be kebab-case —
lowercase ASCII letters and digits in words joined by single hyphens — and any
other spelling is refused naming it; `events merge` carries whatever kinds a
stream holds. `--source` defaults to the
profile's default word (`pipeline` for the agent profile); a word the profile
does not admit is refused. `--label` stamps a label, typed by what the profile
says its key admits (`--label round=2` is an integer under the agent profile).

Before the envelope is stamped the emitter's rule is applied: credential-shaped
values are redacted, and every top-level text value of the payload is bounded
to 4096 bytes with `"truncated": true` stamped when one was cut.

## `deliver`

### `deliver <address> [--message JSON] [--file PATH] [--wait SECONDS]`

Send one message to the spool at `<address>` — the directory a running
receiver bound — and print, as one line of JSON, the disposition the receiver
answered it with. The verb blocks until the receiver has taken the message and
answered it, however long that takes.

The message is JSON, from exactly one of three sources: stdin, the file in
`--file`, or the named `--message` option. Giving none is refused, and so is
giving more than one, naming each source given; either way nothing is written
to the spool. `<address>` is the only positional, so a message passed as a
second one is a usage error. When the spool's receiver declared a schema this
build registers, the message is checked against it first, and one that does not
conform exits 1 naming the JSON pointer, with nothing written.

What became of the message:

- **answered** — exit 0, the disposition on stdout.
- **closed** — the receiver closed its inbox, before or after the message
  arrived: exit 1, with the closer's reason.
- **lost** — nothing took the message within `--wait` seconds (30 by default),
  and it was withdrawn so no receiver delivers it later; or the receiver took it
  and is no longer bound; or its answer document is not one: exit 1, naming the
  spool and the file or the wait.

A path that is no spool is refused with exit 2.

```bash
$ echo '{"addressee":"worker","text":"the reviewer asked for a smaller diff"}' | onemessagebus deliver run/notes
{"interrupted":{"party":"worker"}}
```

## `inbox`

### `inbox carried <store> [--format json|text]`

List every message the carry store `<store>` holds, in the order they were
carried, without draining it: JSON is one `{ts, schema, message}` per line, and
text is `<ts> <schema> <message>` per line. An empty store prints nothing and
exits 0. A path that is no carry store — nothing there, a directory, a file
without a carry store's header line — is refused with exit 2, naming it.

## Queues

A queue is a log a transport keeps, read under the policy its layout or
configuration declares (`docs/queues.md`). Under the `planner-channel` layout the
queues are `surfaces`, `replies`, `commands` and `command-outcomes`, and their
files are the ones `onepipeline` keeps in a run's `channel/` directory.

### `send <queue> [--file PATH] [--config PATH] [--transport-dir DIR] [--registry DIR]`

Append the record on stdin (or in `--file`) to `<queue>`, and print one line of
JSON per record appended: `{queue, position, id}`. The record is shaped as the
layout's writers shape it, checked against its author's grants, validated
against the queue's schema, and given an id where the queue gives one. A reply
envelope sent to `replies` under `planner-channel` is routed by its halves, as
`onepipeline` routes it: its commands to `commands`, its verdict to `replies`,
so one send can print two lines. A queue the configuration does not declare is
refused with exit 2 naming the queues it does, before stdin is read, and a
record that is not JSON, or not a JSON object on a queue whose records are
objects — one that keeps events or numbers its records — is refused with exit 2
with nothing appended; a record its author may not write, its schema refuses, or a validator refuses or cannot
judge (`validate`) exits 1 with nothing appended anywhere, the validator's reason
on stderr unaltered.

```bash
$ echo '{"kind":"finding","message":"the base moved","source":"proposal","blocking":true}' | onemessagebus send surfaces --transport-dir runs/r1/channel
{"queue":"surfaces","position":187,"id":0}
```

### `next <queue> [--consumer NAME] [--asker WORD] [--format json|text] [--config PATH] [--transport-dir DIR] [--registry DIR]`

Claim the next record of `<queue>` and print it as `{queue, position, id,
record}`. The claim is recorded before the record is printed, so two processes
claiming at once receive different records, and a claimant that dies leaves its
record claimed rather than handed out again. On a queue that holds claimed
records pending, the claim is the queue's: a blocking record first, and a
blocking record claimed stays pending until `reply` answers it. On a plain
queue it is `--consumer`'s (the `default` consumer when absent), through that
consumer's cursor. `--asker` makes the claim a listener of that asker: what an
earlier listener of the same asker abandoned is taken back first. A blank
`--asker`, or one that is not Unicode, is refused with exit 2. Nothing to claim
exits 1. Text is `<queue> <position> <record>`.

### `ask <queue> [--blocking] [--asker WORD] [--about ADDRESS] [--timeout SECONDS] [--correlation CORRELATION] [--file PATH] [--config PATH] [--transport-dir DIR] [--registry DIR]`

Ask the question on stdin (or in `--file`) on `<queue>` and wait for its answer
(`docs/ask.md`). The question is stamped with a correlation the bus mints —
printed on stderr as `correlation: <c>` the moment the question is on the queue —
and with `--blocking`, `--asker` and `--about`, then shaped, checked and judged
as `send` does. The answer is one line of JSON on stdout: `{"answer": "reply",
"correlation", "reply"}` exits 0, carrying the reply record that echoes the
correlation; `{"answer": "timeout", "correlation"}` when `--timeout` seconds pass
without one, `{"answer": "abandoned", "correlation"}` when the question is
abandoned and nobody re-attended it, and `{"answer": "refused", "reason"}` when
the bus refused the question or the reply, each exit 1 with the reason on stderr.
Only a reply carries a `reply` member. Without `--timeout` it waits for as long as
it runs. An `ask` that ends without its answer abandons its question — kept, and
still answerable — for a later listener of its asker to take back.

With `--correlation` it raises nothing and listens again for the question that
correlation minted: under the question's own `--asker` it takes the question back
and waits for its reply; under no asker or another, it attends nothing, and a
question left abandoned answers `abandoned`. A question that is not JSON or not a JSON object,
a queue that keeps no events or answers on no queue, a blank `--asker`, and a
malformed or over-long `--about` or `--correlation` are refused with exit 2:
nothing is raised, nothing is printed on stdout, and the problem is named on
stderr.

```bash
$ echo '{"kind":"planner-question","message":"which base?","source":"proposal"}' | onemessagebus ask surfaces --blocking --asker worker-1 --timeout 3000 --transport-dir runs/r1/channel
{"answer":"reply","correlation":"c-5f0e8a2b9c4d4e1f8a7b6c5d4e3f2a1b","reply":{"id":0,"reply":{"version":3,"completion":true,"reason":"main"},"at":1789300000000,"correlation":"c-5f0e8a2b9c4d4e1f8a7b6c5d4e3f2a1b"}}
```

### `reply <queue> [<position>] [--correlation CORRELATION] [--file PATH] [--config PATH] [--transport-dir DIR] [--registry DIR]`

Answer a pending ask of `<queue>` with the reply on stdin (or in `--file`), bound
one of three ways (`docs/ask.md`): `--correlation` names the ask whose
correlation `ask` printed; `<position>` names the record claimed there, as `next`
printed it; with neither, the reply binds to the queue's one pending ask. The
reply is appended to the queue `<queue>` answers on (`replies` for `surfaces`),
shaped, checked and judged as `send` does and stamped with the ask's
correlation, and a question held pending is released. It prints `{answered,
correlation, sent}`: the question answered, its correlation, and every record
appended. A reply that carries only commands answers nothing — `answered` is
`null` — and the ask stays pending. A correlation nothing pending holds (unknown,
or already answered) exits 1 naming it, as do a reply naming neither when no ask
or more than one is pending and a position the pending record was not claimed at,
each with nothing appended. `--correlation` beside `<position>` is a usage error,
and a reply that is not JSON or not a JSON object, a malformed or over-long
correlation, or a queue that answers on no queue, is refused with exit 2, before
the reply is bound and with nothing appended. Of replies racing for one pending record by position, one answers it;
each other is appended, answers nothing, and exits 1 saying another reply
answered the record first.

### `subscribe <queue> --until PREDICATE [--timeout SECONDS] [--format json|text] [--config PATH] [--transport-dir DIR] [--registry DIR]`

Print every line of `<queue>`'s log, oldest first, as `{position, record}` — on
a queue that keeps events each line is `{"event": ..., ...record}` — then each
line as it arrives, and exit 0 after the first line `--until` admits.
`--until` is a predicate: inline JSON when it starts with `{`, otherwise a path
to a YAML file — `{"field": "event", "equals": "answered"}`, with `present`,
`non_empty`, `all`, `any` and `not` as the other forms. With `--timeout`, no
admitted line within that many seconds exits 1; without it the stream waits for
as long as it runs. A predicate that does not parse is refused with exit 2. Text
is `<position> <record>` per line.

```bash
$ onemessagebus subscribe surfaces --until '{"field":"event","equals":"answered"}' --timeout 600 --transport-dir runs/r1/channel
```

### `status [<queue>] [--format json|text] [--config PATH] [--transport-dir DIR] [--registry DIR]`

Report `<queue>`, or every declared queue, as a JSON list of `{queue, events,
records, waiting, pending, pending_position, abandoned, unread, cursors}`: the
records waiting to be claimed, the record pending an answer and where it was
claimed, the records nobody is listening for any more, how many waiting records
are still owed a reading, and each declared consumer's cursor. It is the read a
wrapper otherwise takes from `queue.json` by hand. Text is one line per queue,
`<queue> records=<n> waiting=<n> pending=<id|-> abandoned=<n> unread=<n>`, and one
`cursor <consumer>=<position|->` line per consumer.

```bash
$ onemessagebus status surfaces --format text --config onemessagebus.yaml
```

### `validate <queue> [--file PATH] [--config PATH] [--transport-dir DIR] [--registry DIR]`

Judge the record on stdin (or in `--file`) exactly as `send` would judge it, and
append nothing: by the validators the configuration declares for `<queue>`, and
each record the layout routes to another queue by that queue's
(`docs/validators.md`). It prints `{queue, verdict, reason}` — `verdict` is
`pass`, `refuse` or `unjudged`, and `reason`, beside a verdict that is not a
pass, is the validator's own words, unaltered — and exits 0 for a pass and 1 for
either other verdict, saying the reason on stderr as well. An unjudged record is
never a pass. A queue the configuration does not declare is refused with exit 2
before stdin is read, and a record that is not JSON, or not a JSON object on a
queue whose records are objects, with exit 2 before any validator judges it; a
queue with no validators passes every record. A pass a
validator's cache records is recorded here as it would be on a send.

```bash
$ onemessagebus validate replies --file reply.json --config onemessagebus.yaml
{"queue":"replies","verdict":"refuse","reason":"an `add` states task prose the bar refuses\n"}
```

### `serve <queue> --codec NAME [--session-seconds SECONDS] [--asker WORD] [--file PATH] [--config PATH] [--transport-dir DIR] [--registry DIR]`

<!-- llmlint: ignore[no_redundant_instruction_pointers] Contract B explicitly requires the CLI entry to point to its single normative statement in codecs.md rather than restating it. -->
Serve a configured member protocol according to Contract B in
[`codecs.md`](codecs.md).

With `--resident --socket <path>` in place of a queue and a codec, `serve` runs
the resident core instead, described below. `--registry` is the queue verbs' own.

### `serve --resident --socket PATH [--config PATH] [--transport-dir DIR] [--registry DIR]`

Run the resident core: open the configured transport once, and answer the
resident protocol (`bus.resident-protocol@1`) on the unix socket at `PATH` until
that socket is removed — the clean way to stop it, which exits 0 once the core
notices, within a fraction of a second — or the process is killed. A request is
one line, `{"id": <n>, "verb": "<method>",
"args": {...}, "input": "<stdin>"}`: `verb` is a capability's SDK method
(`schemaList`, `send`, `next`, …), `args` its options keyed as its options root
keys them, and `input` the bytes the verb would read on stdin. It is answered by
one `{"id": <n>, "ok": <output>}` line — the document, the list of lines or the
text the one-shot verb would print — or one `{"id": <n>, "error": {"exit": 1|2,
"message": "...", "output": ...}}` line, carrying the exit code and refusal the
one-shot verb would give and, for `ask` and `validate`, the document it printed. A
`subscribe` request streams `{"id": <n>, "event": {"position", "record"}}` lines
first and ends with `"ok": "until"` when its predicate holds, or `"ok":
"cancelled"` once `{"id": <n>, "cancel": true}` names it or its connection closes.

A request naming no `config`, `transportDir` or `registry` takes the resident's
own, and runs over the transport the resident holds open. The registry directory
is read again for every request, so a schema registered after the core started is
one the next `send` validates by. A socket another live resident answers on is
refused with exit 1, naming that resident's pid, which each resident records in
`PATH.pid`; a socket nobody answers on is a stale one, and is taken over. The
resident listens on a unix socket, and a platform without one refuses
`--resident` with exit 2. `schema gen --lang json bus.resident-protocol@1` prints
the protocol's schema.

```bash
$ onemessagebus serve --resident --socket bus.sock --config onemessagebus.yaml &
$ printf '%s\n' '{"id":1,"verb":"next","args":{"queue":"surfaces"}}' | nc -U bus.sock
{"id":1,"error":{"exit":1,"message":"nothing on surfaces to claim"}}
```

## `transports`

### `transports [--format json|text]`

List every transport kind this build can open, as JSON `{kind, origin, path}`:
the built-in `local` and `memory`, then each plugin — an executable named
`onemessagebus-transport-<kind>` on `PATH` (`docs/transport.md`) — with its path.
A configuration names a plugin's kind exactly as it names a built-in one. Text is
`<kind> <origin> [<path>]` per line.
