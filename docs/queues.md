# Queues

A queue is a log kept on a transport (`docs/transport.md`) and a policy saying
what reading it means. Queues, subscriptions, authors and the configuration file
are the approved contract's Contract Q and configuration shape, said in this
repository's own voice; `docs/contract.md` is the text both restate.

## Plain queues and event queues

A policy that asks for nothing is a **plain** queue: its records are read in
order through each consumer's cursor, and a claim is the cursor moving past a
record. A policy that holds a claimed record pending, claims blocking records
first, supersedes waiting records, or keeps a projection is an **event** queue:
every state a record reaches — `queued`, `claimed`, `answered`, `abandoned`,
`attended` — is one more line of the log, `{"event": <word>, ...record}`, and
what is waiting, what is pending and what nobody is listening for any more is
that log folded.

Nothing accepted is lost and nothing claimed is handed out twice: a claim is
recorded — an event, or a cursor — under the queue's exclusive section before the
record is handed over, so a claimant that dies afterwards leaves its record
claimed, and two claimants at once are handed different records.

## The policy

| field | meaning |
| --- | --- |
| `delivery: at-least-once` | nothing accepted is lost; a claim is a record |
| `ordering: per-queue` | records are totally ordered within one queue, and across queues nothing is promised |
| `supersede_on: {key, when}` | when a record `when` admits is queued (any record, where `when` is absent), every **waiting** record whose `key` field equals the new one's is removed from waiting; a claimed record is never replaced |
| `hold_pending` | a claimed blocking record is pending until answered, one at a time; a later blocking claim takes the slot |
| `blocking_first` | a claim hands out a blocking record before any non-blocking one, in arrival order within each |
| `retention: keep` | append-only; nothing is deleted, and the log is the record |
| `projection` | keep the fold as a document under this name, stamped and sealed |

A predicate — `when`, a declaration's `claims`, `subscribe --until` — is one JSON
object with exactly one form: `{"field": P, "equals": V}`, `{"field": P,
"present": true|false}`, `{"field": P, "non_empty": true|false}`, `{"all": [...]}`,
`{"any": [...]}` or `{"not": {...}}`, where `P` is object keys joined by `.`.

## What sits beside the policy

A layout or a configuration declares each queue with keys beside its policy,
which are declaration keys rather than policy fields (a manager's ruling):

| key | meaning |
| --- | --- |
| `schema` | the schema id every record pushed onto the queue is validated against, as it will be written |
| `answers` | the queue a reply to one of its pending records is appended to |
| `claims` | a predicate a claim passes over a record by |
| `consumers` | the consumers whose cursors `status` reports (`default` when absent) |
| `numbered` | a push stamps each record's `id` with the number of records before it, under the queue's exclusive section |

## What a queue owns of a record

An event queue owns four fields of its records and names nothing else in them:
`id`, allocated one past the highest id the log has queued; `blocking`;
`abandoned`, omitted while false; and `asker`, who raised it. A question asked
through `Bus::ask` carries two more the bus stamps: `correlation`, which the reply
answering it echoes, and `about`, what it is about (`docs/ask.md`). Every other field
is the consumer's, and a typed queue (`Queue<M>`) writes each record in `M`'s
field order. A record is written in the field order it was given, and a fold of
the log hands it back in that order.

## Operations

- `push(record)` validates the record against `M::SCHEMA` (a typed queue) or the
  declaration's schema, gives it its id where the queue gives one, and appends it.
- `claim(consumer)` on an event queue is queue-wide: a blocking record nobody
  abandoned first where the policy says so, then the oldest nobody abandoned,
  then an abandoned one — its text is still what a reader reads it for. On a
  plain queue it is the consumer's: the first record after its cursor that
  `claims` admits.
- `answer(claimed, reply_position)` releases the pending slot `claimed` holds,
  once the reply is at `reply_position` on the `answers` queue; a position no
  record there ends at, or a queue that declares no `answers`, is refused with
  nothing recorded (`QueueError::NoReply`). The reply is
  appended first and the slot released after, where `onepipeline` 0.28.2 releases
  first; the planner channel's typed `Channel::answer` keeps 0.28.2's order.
- `pending(consumer)` is the record waiting for an answer, abandoned ones passed
  over; `held()` is whatever the slot holds.
- `waiting()` and `unread_count()`: every waiting record, and how many of them
  somebody is still owed a reading of.
- `abandon(ids)` marks records nobody is listening for now — kept, uncounted,
  still readable and claimable, the pending slot keeping what it holds — and
  `attend(asker)` takes back every abandoned record that asker raised.
- `status()`, `log(from)`, `fingerprint()` and `wait_for_change(since, timeout)`
  are the reads the command line's `status` and `subscribe` are made of.

## Subscriptions and askers

A `Subscription { queue, consumer, lifetime }` is a listener. What it raises and
claims is its own, and `abandon()` marks all of it that is still outstanding.

- `Lifetime::Durable(asker)`: a listener of an asker. It stamps its asker on what
  it raises, and on opening takes back — `attend` — everything an earlier listener
  of the **same** asker abandoned. A listener of another asker takes nothing.
- `Lifetime::Session`: this listener alone. It adopts nothing, and nothing adopts
  what it raised.

An `Asker` is a non-blank Unicode word compared for equality. A blank one names
every listener carrying it and one that is not Unicode collapses onto every
other such value, so both are refused where the value enters, naming where it
came from: `<source> is set to a blank value, which names no asker; leave it
unset for a session that listens on its own, or set it to the one value every
session of this asker carries`, and `<source> is set to a value this host cannot
read as text; an asker is compared to other askers as one word, and two values
that are not text read as the same word — set it to a name in Unicode, or leave
it unset for a session that listens on its own`.

## Authors and allowlists

An `Author` is an open word in the core. An `Allowlist<Op>` over a profile's
operation vocabulary is exhaustive: `grant(author, op)` grants, `refuse(author,
op, reason)` records why an op is not granted, and `allows(author, op)` refuses
an op nothing granted **by omission**, naming the author, the op and the recorded
reason (`nothing grants it to this author` where none was recorded). A
configuration may narrow an author's grants, and an op it narrows away is refused
with `the configuration does not grant it`; one naming an op the author is not
granted, an op that does not exist, or an author the layout does not declare is
refused naming the key.

## The projection: `accounted` and `seal`

An event queue's projection is **a projection of its log, and never the truth
about it**. It used to be the truth in `onepipeline`: read, modified and written
back whole by every writer and every reader. That lost data — a push landing
between a reader's read and its write-back was overwritten by the reader's stale
copy, and a worker's blocking question was destroyed by the manager's own act of
reading the channel. As a projection, a lost write costs the next reader a fold
and never a record. It earns its place by being cheap where the log is not: the
unread count is one read of it, and its modification stamp is what lets a reader
skip an unchanged queue without opening the log.

- **`accounted`** is how many bytes of the log the projection accounts for, at a
  record boundary. A log longer than that holds records the projection has not
  folded, and a reader folds them before answering; every mutation writes the fold
  as it stands the moment its own record is appended.
- **`seal`** is FNV-1a 128 over the projection's own claims — its waiting records,
  its pending one and its `next_id`, serialized — continued over `accounted`'s
  eight little-endian bytes, written as 32 lowercase hex digits. Every writer
  seals what it writes, and every reader recomputes the seal from the document
  alone. A document whose `waiting` was emptied, or whose `next_id` was reset,
  with its stamp intact is one nothing here wrote; trusting the stamp would hide
  a logged question for good and hand its id out again. So **a stamped document
  that does not seal is read as no document at all, and the whole log is
  folded**. An integrity check against accidents, not a security boundary: a
  rewrite crafted to match is not a failure anybody here meets.
- A document with **no stamp** is an older writer's, taken at its word below its
  `next_id`, with every logged id at or past it — a record whose write-back that
  writer lost — folded in from the log, and stamped the first time it is written.
- A log **shorter than the stamp** was replaced, and is folded whole. A **torn**
  trailing record ends the fold, and the stamp stays before it until its writer
  finishes it. A whole line the queue cannot read still advances the stamp, so it
  is not re-read for ever.
- A read that folded anything writes the repaired projection back. The repair is
  a cache write: one that fails costs the next reader a fold, never an answer.

## The `planner-channel` layout

`onemessagebus_agent::channel` declares `onepipeline`'s channel directory as the
layout `planner-channel`, so a directory `onepipeline` 0.28.2 wrote is read by this
crate, and one this crate writes is read by 0.28.2 — proven on the recorded
channel directories under `crates/onemessagebus-agent/tests/recorded/channel/` and
by the 0.28.2 binary itself in `crates/onemessagebus-e2e/tests/e2e/onepipeline.rs`.

| queue | policy and keys | files |
| --- | --- | --- |
| `surfaces` | `hold_pending`, `blocking_first`, `supersede_on: {key: source, when: source == check-in}`, projection `queue.json`; schema `agent.planner-surface@1`; answers on `replies` | `surfaces.jsonl`, `queue.json` |
| `replies` | plain, numbered; claims pass over a commands-only envelope; schema `agent.queued-reply@1` | `replies.jsonl`, `replies-cursor.json` |
| `commands` | plain, numbered; schema `agent.queued-commands@1` | `commands.jsonl`, `commands-cursor.json` |
| `command-outcomes` | plain; schema `agent.command-outcome@1` | `command-outcomes.jsonl` |

- **Supersede on `source`, not `kind`** (a manager's ruling): Contract Q's wording
  is `kind == check-in`, and 0.28.2 supersedes on `source == "check-in"` — an
  observer's frame of kind `check-in` carries source `proposal` and is not
  superseded there. Byte compatibility wins over the wording, and the
  `Supersede { key, when }` shape is unchanged.
- A reply offered as a bare envelope is **routed by its halves**, as 0.28.2 routes
  it: its commands to `commands` as `{id, author, commands}`, its verdict to
  `replies` as `{id, reply, at}` — and an envelope with commands and no verdict to
  `commands` alone. A version this build reads (`[3, 2]`) is read at 3, and an
  edit envelope at any other is refused: `an edit envelope requires version 3`.
- The ops are `add`, `drop`, `reparent`, `retry`, `cancel`, `requeue`, `complete`,
  `attest`, `finding`, `amend`, `note` and `settle`. The planner is granted every
  op; the monitor `retry`, `requeue`, `cancel`, `finding` and `add`. Every other op
  is refused the monitor in `onepipeline`'s words — `'<op>' is not an op the
  monitor may issue: <reason>. Surface it to the planner instead` — and a verdict
  carrying `completion: true` is granted or refused as `complete` is.

## The configuration file

```yaml
version: 1
transport: {kind: local, dir: runs/r1/channel}  # kind: local | memory | a registered or plugin kind
profile: planner-channel                         # a layout a linked profile declares; optional
queues:                                          # additions, or overrides of a layout's queue by name
  findings: {policy: {hold_pending: false}}
authors:                                         # may narrow a layout author's grants, never widen them
  monitor: {capabilities: [retry, requeue, cancel, finding]}
```

Reading it is two steps, and the types keep them apart:

1. `onemessagebus::Config::load(path)` reads the file and refuses what the file
   alone decides, each by the key it is at: YAML that is not one document, an
   unknown key, a `version` other than 1, and a queue name, consumer name, schema
   id, document name or predicate that does not parse. A `Config` opens nothing.
2. `Config::resolve(&layouts, &kinds)` binds it to the layouts a process links and
   the transport kinds it can open, and refuses what only those decide, each by
   the key it is at: a `profile` no layout declares; a widened grant, an op that
   does not exist or an author the layout does not declare
   (`authors.<author>.capabilities`); a `schema` the layout does not register or
   an `answers` naming no declared queue (`queues.<queue>.<key>`); and a transport
   its kind refuses. What it answers, a `Bus`, is the one type that opens a queue
   or authors a record.

Two more blocks sit beside these: `validators`, what a queue judges a message by
before anything is appended (`docs/validators.md`), and `codecs`, what a host
configures for each codec `serve` runs (`docs/codecs.md`). Each refuses an
unknown key by name at `Config::load`, and `validators[<index>].on` naming no
declared queue is refused by `Config::resolve`.

The binary reads the file from `--config <path>` or `ONEMESSAGEBUS_CONFIG`, and
`--transport-dir <dir>` or `ONEMESSAGEBUS_TRANSPORT_DIR` replaces `transport.dir`
for one invocation — the flag over the variable, and the variable over the file.
The file's JSON Schema is the SDK bundle's `config` root, generated from the one
reader's type.
