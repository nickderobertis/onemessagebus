# The inbox

A typed channel into a running process, whose sender learns what the receiver
did with the message — the approved contract's Contract I, said in this
repository's own voice. The core crate declares it with no message family of
its own; the agent note contract (`onemessagebus_agent::note`, Contract N) is the
first family carried over it. `docs/contract.md` is the text both restate.

## The pair

- `Sender<M, D>` is the sending end: `Clone`, and usable from any thread.
  `send(message) -> Result<D, Undelivered>` blocks until the receiver has taken
  the message **and** answered it, or the inbox is closed.
- `Inbox<M, D>` is the receiving end. `take()` hands over the next message if
  one is waiting and never blocks; `take_within(timeout)` blocks up to `timeout`
  for one; `answered()` lists every message answered so far with its
  disposition, oldest first; `close(Closed)` refuses every blocked sender, and
  every later one, with the closer's reason.
- `Delivered<M, D>` is one taken message: `message()` reads it, and
  `answer(disposition)` hands the disposition back to exactly the sender that
  asked.
- `M` is a `Message` — a type with a registered schema id — and `D` is a
  `Disposition`, the consumer's own type, which the core requires only to
  serialize, so a disposition crosses a spool as readily as a thread and the
  core has no vocabulary in it. `Carried: Disposition` adds `carried()`, what a
  carried message's sender is answered with.

## What a sender is told

A sender is never answered with a disposition its receiver did not give: no
timeout makes one up. What `send` returns is decided by evidence the backend
holds, and the three states are distinct:

| state | when | `send` |
| --- | --- | --- |
| pending | the receiver is bound and has neither answered nor closed | stays blocked, however long that takes |
| closed | the receiver closed the inbox, before or after the message arrived | `Undelivered::Closed`, with the closer's reason |
| lost | the answer cannot be had | `Undelivered::Backend`, naming the spool and the file or the wait |

A second close changes nothing: the first reason is the one every sender is
told. An inbox dropped without being closed closes itself, and a taken message
dropped without an answer tells its sender so, so a receiver that goes away —
cleanly, by an error, or by a panic — leaves no sender in its own process
blocked.

The routing of a taken message — which party of a conversation is live,
whether a decision is re-taken — belongs to whoever holds the inbox. The inbox
promises only that a message reaches `take` or its sender learns why not, and
that a disposition reaches exactly the sender that asked.

## Backends

Each backend is an `InboxBackend<M, D>`, the one trait a `Sender` is built over
(`Sender::over`), so a consumer writes against the pair whichever backend
carries the message.

### `InProcess`

One process: `Sender::channel()`, or `inbox.sender()`, meets the inbox in
memory. There is no bounded wait; a sender waits for as long as the inbox is
open.

### `Spool`

A directory a receiver's process binds, for a sender in another process.

- `Spool::bind(dir, &inbox)` creates the directory, takes its lock, declares the
  schema the inbox takes, and starts a courier thread that moves each offered
  message into the inbox and writes the receiver's answer back beside it. A
  second receiver is refused while the first is bound, and a receiver binding a
  spool an earlier one closed reopens it. `spool.address()` is the path a
  consumer records for a sender elsewhere to find. Dropping the `Spool` stops
  the courier and releases the lock.
- `Spool::connect(address)`, or `Spool::connect_within(address, wait)`, is a
  sender into it. `Spool::deliver(address, message, wait)` offers a JSON message
  under the schema the spool declares, and answers the disposition as JSON: it
  is what `onemessagebus deliver` calls.
- The **bounded wait** — `SPOOL_WAIT`, 30 seconds, or `deliver --wait` — covers
  one thing: a message that is never taken. When it passes, the sender
  withdraws the message, so a receiver waking later does not deliver what its
  sender was told was lost, and reports the wait. A message that has been taken
  is waited on for as long as its receiver stays bound.
- A sender reports the message **lost** when its answer document is present
  but is not an answer (naming the file), when the wait passes with the message
  never taken (naming the wait), when the receiver took it and no longer holds
  the spool's lock with no answer written (naming the file), or when the
  receiver could not read the offer (in the receiver's words).
- A receiver that **closes** records the close in the spool, so a sender
  arriving afterwards is refused before it offers anything, and every offer
  still waiting is answered closed.

#### On disk

Every document is one JSON object carrying `"schema_version": 1`
(`SPOOL_SCHEMA_VERSION`), and one naming another version is refused rather than
guessed at. Each is written beside its final name, at that name plus
`.staging`, and renamed onto it, so neither side reads half a document; and
every hand-over of a message is a rename, so the courier taking a message and
its sender withdrawing it cannot both succeed.

| file | written by | what it is |
| --- | --- | --- |
| `spool.json` | the receiver, on bind | `{schema_version, schema}`: the schema the receiver takes |
| `receiver.lock` | the receiver, on bind | locked exclusively for as long as a receiver is bound; the system releases it when that process ends, however it ends |
| `closed.json` | the receiver, on close | `{schema_version, reason}`: the closer's words |
| `<id>.offer.json` | a sender | `{schema_version, schema, message}`: a message waiting to be taken |
| `<id>.taken.json` | the courier, renaming the offer | a message the receiver has taken and not yet answered |
| `<id>.answer.json` | the courier | `{schema_version, answer}`, where `answer` is `{"disposition": ...}`, `{"closed": {"reason": ...}}` or `{"refused": {"reason": ...}}`; its sender reads it and removes it |
| `<id>.withdrawn` | a sender, renaming its offer | a message its sender took back, removed at once |

`<id>` is the minting instant in nanoseconds since the Unix epoch (39 digits),
the minting process's id, and a per-process counter (20 digits), joined by `-`:
the courier takes offers in the order they were made. The exact documents are
`docs/contract.md`'s `spool-documents` fixture, which the profile's contract
test holds to what a bound spool writes.

### `Carry`

For a receiver that is not running now. `Carry::sender(store)` appends each
message to a durable store and answers `D::carried()` at once, since nothing
has read it yet; the receiver's next session drains the store as it opens, with
`inbox.adopt_carried(store)`, and takes each message exactly once.
`Carry::read(store)` lists a store without draining it: it is what
`onemessagebus inbox carried` calls.

A store is one NDJSON file: a header line
`{"schema_version":1,"kind":"onemessagebus-carry-store"}`, then one
`{ts, schema, message}` per carried message, in the order they were carried.
Every writer and the draining receiver hold an exclusive lock on the file across
what they do with it. A drain reads every record as a message of its inbox
before it takes any, and the inbox takes them in the same step the store is
emptied, so a message is in the store or in the inbox and never in both. A path
with nothing at it, a directory, or a file without that header is no carry
store and is refused by name; a store nothing was ever carried into adopts as
empty.

## The agent note family

`onemessagebus_agent::note` declares `Note`, the message `agent.note@1`, and
`Accepted`, its disposition, which carries as `Queued`: a note carried to a
conversation that is not running is queued for that conversation's next turn.
`Notes` and `NoteInbox` are `Sender<Note, Accepted>` and `Inbox<Note, Accepted>`.
On the wire an `Accepted` is `"queued"`, `{"interrupted": {"party": ...}}` or
`{"judged_with": {"completion_reason": ...}}`.

A conversation closing its inbox with a note refusal closes it with
`Closed::from(&refusal)`, and its caller reads the same refusal back with
`note::Undelivered::from(undelivered)`; a close in anyone else's words reads back
as `MemberSettled`, and a backend that could not produce an answer as
`NoConversation`.
