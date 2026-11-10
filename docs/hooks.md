# Hooks

A hook is a step a queue runs each offered message through **before anything is
appended**. Whatever the hook answers is the message from then on: it keeps the
message, changes it, reroutes it to another queue, drops it, refuses it, or says
it could not decide. Hooks replaced validators, which were removed in 0.11.0
(see "Validators, removed", below).

This document is the one source of the hook contract. `docs/contract.md`,
`docs/cli.md` and `docs/queues.md` point here rather than restating it, and `crates/onemessagebus/tests/hooks_doc.rs` drives every fenced
block below.

## The configuration file

A top-level `hooks:` list in `onemessagebus.yaml`:

<!-- fixture: hooks-config -->
```yaml
hooks:
  - {on: reports, when: {carries: verdict}, command: [python3, -m, triage_reports],
     routes: [manager, archive],
     cache: {dir: .hook-outcomes, bar_fingerprint: [scripts/bar-fingerprint.sh]}}
  - {on: reports, command: [scripts/strip-noise.sh]}
```

| key | meaning |
| --- | --- |
| `on` | the queue whose offered messages it runs on; one the configuration does not declare is refused by `Config::resolve`, naming `hooks[<i>].on` |
| `when` | which messages it runs on, in two forms: `{carries: <field path>}` — the field is there and holds something other than `null`, `""`, `[]` or `{}`; `carries` stands alone, and a `when` naming it beside another key is refused — or any queue predicate; every message when absent, and a message it does not admit is kept |
| `command` | the argv, the program first; an empty one is refused naming `hooks[<i>].command` |
| `routes` | the queues the hook may reroute to; each must be a queue the configuration declares, or resolution refuses it naming `hooks[<i>].routes[<j>]`; with no `routes` key the hook may not reroute, so a configuration can narrow what a layout allows and never widen it |
| `cache` | `{dir, bar_fingerprint}`, both required, `dir` relative to the working directory; opt-in per entry (see the cache, below) |

There is no `kind` key, and an unknown key is refused by name, as on every other
block. A Rust hook a linking consumer registers is code, not configuration
(`Bus::with_hook`, below).

## The command's protocol

The command's stdin is the message as one line of JSON, newline-terminated. Its
environment names:

- `ONEMESSAGEBUS_HOOK_QUEUE`: the queue it runs on;
- `ONEMESSAGEBUS_HOOK_CORRELATION`: the question an offer asks or answers, when
  it does;
- `ONEMESSAGEBUS_HOOK_ROUTE`: only for an entry declaring `routes`, an empty file
  the hook may write one queue name to.

| the command | the outcome |
| --- | --- |
| exits 0, printing nothing | **drop** |
| exits 0, printing the input line back byte for byte, with or without its one trailing newline | **keep** |
| exits 0, printing anything else | **change**: it must be exactly one JSON value on one line, its trailing newline optional |
| exits 0 with a change that is not one JSON value on one line — multi-line JSON, whitespace only, or not JSON at all | **unjudged**, naming why (for multi-line output, that a queue record is one line) |
| exits 0, its route file naming a queue in `routes` | **reroute** to it, carrying the kept or changed message |
| exits 0, its route file naming a queue outside `routes` | **unjudged** |
| exits 1 | **refuse**, with its stderr exactly as written as the reason; an empty stderr is reported as a refusal that wrote no reason |
| exits anything else, is ended by a signal, or cannot be started | **unjudged**, naming the exit and its stderr |

Keep and change are told apart by **bytes, never by parsed values**, because
whatever the script prints is the message: a re-formatted but equivalent value
(other spacing, `1.0` for `1`, keys in another order) is a change. A change's
bytes, less their trailing newline, are the message from then on: handed verbatim
to the next hook's stdin, and appended verbatim unless a preparation step
rewrites the record (below). A route file beside a drop is ignored. Neither an
unjudged nor a refused message is appended anywhere.

## Chaining and order within one offer

1. The layout's preparation of the offered record runs first. A refusal there
   returns as it always has, and no hook runs.
2. The offered queue's hooks run in declaration order, each one's output the next
   one's input, followed by the hooks a linking consumer registered in code
   (`Bus::with_hook`). A drop, refusal, unjudged answer or reroute ends the chain.
3. A changed or rerouted message is prepared again for the queue it now goes to,
   so the layout's grants, checks and schema apply to what the hook made, read
   from its parsed value. Where no preparation step rewrites the record — a check
   or a grant reads it and leaves it as it was — the hook's bytes are kept. Where
   a step does rewrite it — a stamp, rename, version or route step that projects
   it — the record is written from its parsed value, as any send's record is.
4. A rerouted message runs its new queue's hooks once; a second reroute is
   unjudged.
5. Each record the layout routes to another queue then runs that queue's hooks.
   A record one of them changes is prepared again for that queue, and what that
   routes to another queue runs that queue's hooks in turn. A change the layout
   would route back to a queue the record already passed through in this offer
   makes the offer unjudged, naming the changing hook and that queue, with nothing
   appended, so no layout can carry an offer round a loop.

Everything is decided before anything is appended. A refusal or an unjudged
answer anywhere appends nothing anywhere, so a split reply is refused whole, and
every record is checked against its queue's schema before any is appended. A drop
removes only the records it drops; a drop on the offered queue drops the whole
offer.

On a queue that keeps events or numbers its records, the queue's own `event`
and `id` are spliced in front of the hook's bytes, so its spelling survives;
only a record already carrying the member the queue sets is written from its
parsed value.

## The drop log

Every drop appends one line to a companion append-only log on the same
transport, `<queue>-dropped`, where `<queue>` is the queue the dropping hook ran
on:

<!-- fixture: drop-line -->
```json
{"schema_version":1,"at":1794150248611,"queue":"reports","hook":{"index":1,"command":["scripts/strip-noise.sh"]},"message":{"kind":"report","text":"all quiet"},"correlation":"c-5f0e8a2b9c4d4e1f8a7b6c5d4e3f2a1b"}
```

`hook.index` is the hook's position in that queue's chain; `command` is absent for
a hook registered in code, and `correlation` is absent when the offer asks or
answers no question. `message` is the message exactly as the dropping hook
received it, byte for byte.

- The suffix `-dropped` is reserved: a layout or configuration declaring a queue
  whose name ends in it is refused, naming the queue.
- The log is read through the existing log reads — `subscribe <queue>-dropped` on
  the command line — and `Bus::dropped(&QueueName) -> Result<Vec<Dropped>,
  BusError>` reads it in the library.
- `status` reports `dropped=<n>` in text and `"dropped": <n>` in JSON for each
  queue.
- The drop log is not redacted. No queue record is redacted (redaction applies to
  `events emit` alone), so a dropped message is recorded exactly as the queue
  would have held it, and the drop log is created with the same file mode as the
  queue's own log.

## The cache

`HookCache { dir, bar_fingerprint }` keys a record on the SHA-256 of the input
bytes and the SHA-256 of the hook's argv plus what `bar_fingerprint` prints (its
stdout, trimmed). It
records **keep**, **change** (with the changed output) and **drop**, and never a
refusal, an unjudged answer or a **reroute**: an outcome whose route file named a
destination is never cached, so a cached entry runs its command again on every
offer it reroutes, and the entry's current `routes` and the current layout always
decide where a message goes. A cache hit therefore never carries a destination.

**A cache is only for a hook whose outcome depends on nothing but the input bytes
and the fingerprinted bar.** A hook that reads the time or any other state must not
declare one, because a cached change or drop would then be wrong.

A record is one file, `<dir>/<content>.<bar>.pass.json`:

<!-- fixture: hook-record -->
```json
{"outcome": "change", "schema_version": 2, "content": "<sha-256 hex>", "bar": "<sha-256 hex>", "fingerprint": "<what the bar printed>", "command": ["<argv>"], "output": "{\"kind\":\"report\",\"text\":\"all quiet\"}"}
```

`outcome` is `keep`, `change` or `drop`, and only a change carries `output`. A
`schema_version: 1` pass record — what a validator's `PassCache` wrote under the
same key, before validators were removed — for the same argv and content is read
as **keep**, so the passes a validator recorded survive the swap to a hook. A
fingerprint that cannot be read keys nothing, and a record that cannot be written
costs a re-run, never a different outcome.

## The library surface

The items a linking consumer uses:

- `RawRecord`: one record as one line of JSON, holding the exact bytes and the value parsed from them — `RawRecord::parse(bytes)` (refusing anything that is not exactly one JSON value on one line), `RawRecord::from_value(&Value)` (compact serialization), `bytes()`, `value()`
- `trait Hook<M: Message>: Send + Sync { fn run(&self, message: &M, context: &HookContext) -> HookOutcome<M>; }`
- `enum HookOutcome<M> { Keep, Change(M), Reroute { queue, message }, Drop, Refuse { reason }, Unjudged { reason } }`
- `HookContext { queue, correlation }`
- `Hooks<M>`, the ordered chain: `new`, `with`, `push`, `len`, `is_empty`, `run`
- `CommandHook`: `new(argv)`, `with_cache(HookCache)`, `with_routes(..)`, `command()`, `cache()`, and a raw-bytes `run(&self, content: &[u8], context) -> HookOutcome<RawRecord>`; it implements `Hook<RawRecord>`
- `HookCache`: `new`, `dir`, `bar_fingerprint`, `fingerprint`
- `Bus::with_hook(self, queue, hook) -> Result<Self, BusError>`
- `Bus::hooked(&self, queue, record) -> Result<Hooked, BusError>`: what an offer becomes, appending nothing but cache records

The bus runs every chain over `RawRecord`. A typed hook (`M` not `RawRecord`) is
handed the record's value read as an `M` — a record that does not read as one is
refused, naming why — and its `Change(M)` becomes `RawRecord::from_value` of its
serialization.

`Hooked { outcome, records, dropped }` is what an offer became: `outcome` is a
`HookedOutcome` — `Keep`, `Change` (a record was changed or rerouted, or dropped
beside one that was not), `Drop` (the whole offer was dropped), `Refuse { reason }`
or `Unjudged { reason }` — `records` the `(QueueName, RawRecord)`s a send would
append, empty for the last two, and `dropped` the `Dropped` lines it would record.
`Bus::append_hooked(&Hooked)` appends each record's bytes verbatim and the drop
lines, for a consumer that decides an offer and appends it on its own path.

`Bus::send`, `Bus::reply` and `Bus::ask` run hooks as above. `send` keeps its
return type and omits dropped records (`Bus::offer` answers them beside it). A
dropped `ask` or `reply` returns `BusError::Dropped { queue }`, naming the queue
whose drop log recorded it; that log's line names the dropping hook. A question a hook
reroutes off the queue it was asked on can never be answered there, so `ask`
refuses it with `BusError::NotAskable`, appending nothing; a reply rerouted off
its answers queue is appended where it went and answers nothing. A hook's refusal is
`BusError::HookRefused { queue, hook, reason }` and its unjudged answer
`BusError::HookUnjudged { queue, hook, reason }`; `hook` is a `HookRef { index,
command }`, the drop line's `hook` object.

## The command line

- `send` prints `{"queue": "<q>", "dropped": true}` for each dropped record, beside
  the usual `{queue, position, id}` line of each record appended, and exits 0.
  `reply` prints the same line for a dropped reply, and exits 0; it answers
  nothing.
- `ask` whose question a hook drops returns at once, without waiting for its
  timeout: stdout is `{"answer": "dropped", "queue": "<q>", "hook": {"index": <i>,
  "command": [<argv>]}}`, stderr is one line saying no reply will come, and it exits
  1, because only an answered ask exits 0, as with `timeout`, `abandoned` and
  `refused`.
- A hook's refusal on `send`, `reply` or `ask` exits 1, prints nothing on stdout,
  and one line on stderr: `onemessagebus: <queue>: refused by hook <i> (`<argv>`)
  before anything was appended: <reason>`, the reason exactly as the hook wrote it
  less the newline it ended with. An unjudged answer exits 1 the same way, naming
  the hook and why.
- `validate` is the dry run: it prints `{queue, verdict, reason, outcome, records}`
  — `verdict` `pass` for keep, change and drop, else `refuse` or `unjudged` with its
  `reason`; `outcome` one of `keep`, `change`, `drop`, `refuse`, `unjudged`; and
  `records` the `{queue, record}`s a send would append, each record's bytes as it
  would append them. It exits 0 for the first three outcomes and 1 for the last two.

## The SDKs

The Python and Node SDKs type the configuration's `hooks` (`HookConfig`, written
to a file by `write_config` / `writeConfig`), `validate`'s `outcome` and `records`,
`send`'s and `reply`'s dropped result (`Dropped`), and `ask`'s `dropped` answer
(`DroppedQuestion`); `docs/sdk-parity.md` lists them for both.

## Validators, removed

Validators were removed in 0.11.0. A configuration that still declares the
`validators` key is refused when it is loaded, by every verb that loads one, at
exit 2, naming the key and the migration: declare each entry under `hooks:` with
the same `on`, `when`, `command` and `cache`, drop its `kind: command`, and make
its command print its stdin back before it exits 0. Exit 1 with a reason on
stderr still refuses.
