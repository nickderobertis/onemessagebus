# The inbox

A typed channel into a running process, whose sender learns what the receiver
did with the message — the approved contract's Contract I, said in this
repository's own voice. The core declares it with no message family of its own:
a program brings its message and its disposition, and the inbox carries them.
`docs/contract.md` is the text both restate.

## The pair

- `Sender<M, D>` is the sending end: `Clone`, and usable from any thread.
  `send(message) -> Result<D, Undelivered>` blocks until the receiver has taken
  the message **and** answered it, or the inbox is closed.
- `Inbox<M, D>` is the receiving end. `take()` hands over the next message if
  one is waiting and never blocks; `take_within(timeout)` blocks up to `timeout`
  for one; `answered()` lists every message answered so far with its
  disposition, oldest first; `close(Closed)` refuses every blocked sender, and
  every later one, with the closer's reason.
- `Delivered<M, D>` is one taken message: `message()` reads it,
  `answer(disposition)` hands the disposition back to exactly the sender that
  asked, and `release()` hands the message back untaken (below).
- `M` is a `Message` — a type with a registered schema id — and `D` is a
  `Disposition`, the consumer's own type, which the core requires only to
  serialize, so a disposition crosses a spool as readily as a thread and the
  core has no vocabulary in it. `Carried: Disposition` adds `carried()`, what a
  carried message's sender is answered with.

## Offering, withdrawing and releasing

`Sender::offer(message) -> Result<Offer<D>, Undelivered>` splits `send` in two.
It returns once the message is **admitted**, meaning it sits in the receiver's
`Inbox` queue: at once in process, and over a spool once the receiver's courier
has queued it and acknowledged that. A message not admitted within the
backend's bounded wait is withdrawn and reported `Undelivered`, exactly as
`send` reports it — except one a spool courier already holds where the
receiver's declaration has no file identity, as on Windows, which is read as
its receiver's at the bound (below). `send` is `offer` followed by `wait`. A message is **taken**
when `take` or `take_within` returns it.

- `offer.wait()` blocks for the answer, as `send` does after admission.
- `offer.wait_until(deadline) -> Result<D, Unanswered>` withdraws a message
  still untaken at the deadline, atomically, and returns `Unanswered::Expired`.
  The message was never taken and never will be. A message taken before the
  deadline is waited on past it, and its answer is returned however late it
  comes, or `Unanswered::Undelivered` if the receiver closes, goes away, or
  drops its claim. `Unanswered` is `#[non_exhaustive]`.
- `offer.withdraw() -> Result<(), Offer<D>>` returns `Ok` only when the message
  was untaken, and then it is never taken. A taken message answers `Err` with
  the offer, which can still `wait` for its answer.
- Taking a message and withdrawing it **never both succeed and never both
  fail**, in process (the inbox's lock decides) and across a spool (one claim
  file decides, below).
- `Delivered::release()` is for a receiver that took a message it cannot
  present. The message goes back into the queue at the position it arrived in,
  ahead of everything that arrived after it, and to its sender it is untaken
  again: `withdraw` can succeed, and a release after a `wait_until`'s deadline
  has passed expires the message in the same operation, so no take can find
  it between the release and its sender waking. A message is answered at most once, and one released and
  then withdrawn or expired is never answered by the receiver.
- Dropping an `Offer` withdraws nothing. The message stays offered and is still
  taken and answered, and the answer goes to nobody. Over a spool, the dropped
  offer removes its taken file, so the receiver writes no answer file nobody
  would read.

`InboxBackend::offer` is a provided method, so a backend written before it still
compiles. Its default sends the message and holds the answer: the offer returns
only once the message was admitted and answered, and its `withdraw` always
reads the message as taken, because such a backend cannot guarantee a
withdrawal. `Carry` is one of these.

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
  one thing: a message that is never admitted. When it passes, the sender
  withdraws the message, so a receiver waking later does not deliver what its
  sender was told was lost, and reports the wait. An admitted message is waited
  on for as long as its receiver stays bound. `offer` returns within the bound
  in every case, no later than one poll (20 ms) after it: where the file system
  refuses the claim the withdrawal needs — the rename to `<id>.withdrawn`
  before the courier has the message, or `<id>.claimed` after — it stops there
  with `BackendError::Unclaimable`, naming the refused claim and the bound,
  apart from the `Elapsed` of a withdrawn message. That outcome alone leaves it
  possible that the receiver admits the message later, once the file system
  lets it; nothing waits for its answer. Where the receiver holds
  `<id>.claimed` at the bound with neither `<id>.admitted.json` nor an answer
  written — a receiver stopped as it refused the message — it stops there with
  `BackendError::Unsettled`, naming that claim: the message is reported neither
  admitted nor withdrawn, and nothing reads what the receiver later writes.
- **Admission and withdrawal after it.** A receiver of this build writes
  `admission.json` beside `spool.json` as it binds, holds `admission.lock`
  locked for as long as it is bound, and removes the advertisement as it
  unbinds, while it still holds both locks. The file advertises that the
  receiver acknowledges admission and honours a withdrawal after it. Its
  courier renames each offer to taken, queues the message in the inbox, and
  writes `<id>.admitted.json` under the inbox's lock, before any take can come.
  One the file system refuses to write leaves the message out of the queue, so
  no take ever finds it: the receiver answers it refused, naming the file, and
  its sender's `offer` reports that at once rather than the bounded wait.
  From then on the receiver's take and the sender's withdrawal race to create
  `<id>.claimed`, an exclusive create that only one of them can win. The loser
  learns it has lost: a take that loses drops the message as withdrawn, and a
  withdrawal that loses reads the message as taken. A release removes the
  receiver's claim, so the sender can withdraw again. A sender in
  `wait_until` publishes its deadline in `<id>.deadline.json` before it waits,
  on the monotonic clock its `Instant` was taken on (`CLOCK_MONOTONIC` on
  Linux), which it shares with the receiver (below), together with that
  clock's identity. A release decides from that document, under the inbox's
  lock and on its own reading of the same clock, whether the deadline has
  passed. The sender need not run after its deadline for the release to expire
  the message, and no step of the wall clock, forward or back, moves when it
  does: a release past it creates `<id>.expired`, drops the message from the
  inbox and keeps its claim, so no take can come before the sender reads it,
  reports `Expired`, and removes the message's files. A take that claims a
  message past its published deadline does the same. A deadline the file system
  refuses to publish is published at the sender's next look. One the receiver
  cannot read, or one on a clock it does not read, is no deadline, and the
  sender's own withdrawal at it still decides. An `<id>.expired` the file system
  refuses to create leaves the message queued with the receiver's claim, so
  neither side has it until the next take can create it. A claim the file system
  will not remove stays the receiver's, and its sender reads the message as
  taken. The receiver claims a
  message before it writes any answer (a disposition, a close, or a refusal)
  and lets the claim go only once the answer is written. So a sender whose
  claim succeeds after that finds the answer, gives its claim back, and reads
  the message as taken. An acknowledgement the sender cannot read acknowledges
  nothing: the bounded wait goes on, and a withdrawal when it passes reports
  that file by name instead of the wait. A claim the file system refuses to
  create decides nothing, so neither side has the message. The receiver's take
  leaves it queued in its place, and the next take tries again. The sender's
  withdrawal reads it as taken, because it never claims a withdrawal it cannot
  guarantee. A sender whose bounded wait passes with the message unacknowledged
  and its claim refused stops at the bound with `Unclaimable`, naming
  `<id>.claimed`: neither admitted nor withdrawn, and possibly admitted later. An answer whose message cannot be claimed (a close, or a refusal,
  of a message never taken) is not written, and its sender is told the message
  was lost.
- **A receiver that does not advertise it** (an older build) has the message
  once its courier has renamed the offer, as before. A sender then reads every
  admitted message as taken and never claims a withdrawal that receiver would
  not honour. A sender trusts an advertisement only while the receiver that
  wrote it is alive and bound, and only beside the declaration it was written
  with; it withdraws after admission only where the two also share a monotonic
  clock. The first two checks read no clock:
  - **Its lock is held.** The system releases `admission.lock` when its
    receiver's process ends, however it ends, and an older receiver never
    takes it. So one that rebinds after a newer one was killed, leaving its
    `admission.json` behind, leaves the lock free. A sender's look tries the
    lock and lets it go at once, and a binding receiver waits for the lock
    rather than trying it, so no look refuses a bind.
  - **Its declaration is the file there now.** `admission.json` records
    `spool.json`'s identity: the device and inode on Unix. Every declaration
    is a new file renamed into place, so an older receiver's first rebind
    never matches, even in the moment a dying receiver's locks take to be let
    go of. Where the standard library gives a file no identity, as on Windows,
    it records `unidentified`, which an older receiver's declaration cannot be
    told from. A sender still waits for `<id>.admitted.json` beside it, so
    `offer` returns only once the receiver's inbox has queued the message, but
    it never withdraws from that receiver or claims against it: `withdraw` on
    an admitted message answers taken, `wait_until` never reports one
    `Expired`, and when the bounded wait passes with the courier holding the
    message (its offer renamed to `<id>.taken.json`) and no acknowledgement
    written, the message is read as admitted, its receiver's, rather than
    withdrawn, since an older courier behind that advertisement honours no
    claim; its `wait` then reports the receiver's answer, or `Undelivered`.
    That is the one case where `offer` returns without the receiver's inbox
    having confirmed admission. Before the courier has it, the bounded wait
    withdraws it as on every platform.
  - **It reads the sender's monotonic clock.** `admission.json` records the
    receiver's clock as `linux-monotonic:<boot id>:<time namespace inode>`
    (`…:none` on a kernel without time namespaces): the kernel's boot id, and
    the time namespace whose offset the clock carries. A sender trusts the
    advertisement only when that is its own clock, because only then does a
    deadline it publishes mean the same moment to the receiver. Where it cannot
    verify that, it loses withdrawal and expiry after admission and nothing
    else: it still waits for `<id>.admitted.json`, so `offer` returns only once
    the receiver's inbox has queued the message, or withdraws it when the
    bounded wait passes; but it publishes no deadline, `withdraw` on an
    admitted message answers taken, and `wait_until` never reports an admitted
    message `Expired` but waits for its answer. A withdrawal when the bounded
    wait passes claims `<id>.claimed` and keeps it only while
    `<id>.admitted.json` is absent, without which the inbox queues nothing and
    which it writes under its lock before any take; finding it there, the sender gives the claim back and reads the
    message as taken. A take that lost to that claim in the moment before it
    was given back drops the message, and its sender is told it was lost:
    never answered, never expired, never withdrawn. That covers a receiver on
    another boot or in another time namespace, one whose `admission.json`
    records no clock or one this build cannot read, either side unable to read
    `/proc/sys/kernel/random/boot_id` or failing to read `/proc/self/ns/time`
    for any reason but its absence (absent, the namespace is `none`), and every
    platform other than Linux, where this build identifies no shared clock.
    Before admission the bounded wait still withdraws the message on every
    platform, since an unadmitted message is its sender's to withdraw.
- A sender reports the message **lost** when its answer document is present
  but is not an answer (naming the file), when the wait passes with the message
  never taken (naming the wait), when the wait passes with a claim its
  withdrawal needs refused (naming the claim, and the message possibly admitted
  later), when the wait passes with the receiver holding the message's claim
  unacknowledged and unanswered (naming the claim), when the receiver took it and no longer holds
  the spool's lock with no answer written (naming the file), or when the
  receiver could not read the offer (in the receiver's words).
- A receiver that **closes** records the close in the spool, so a sender
  arriving afterwards is refused before it offers anything, and every offer
  still waiting is answered closed.

#### On disk

Every document is one JSON object carrying `"schema_version": 1`
(`SPOOL_SCHEMA_VERSION`), and one naming another version is refused rather than
guessed at. Each is written beside its final name, at that name plus
`.staging`, and renamed onto it, so neither side reads half a document. Every
hand-over of a message is a rename or an exclusive create, so the courier taking
a message and its sender withdrawing it cannot both succeed, and neither can the
receiver's take and its sender's withdrawal after admission.

| file | written by | what it is |
| --- | --- | --- |
| `spool.json` | the receiver, on bind | `{schema_version, schema}`: the schema the receiver takes |
| `receiver.lock` | the receiver, on bind | locked exclusively for as long as a receiver is bound; the system releases it when that process ends, however it ends |
| `closed.json` | the receiver, on close | `{schema_version, reason}`: the closer's words |
| `<id>.offer.json` | a sender | `{schema_version, schema, message}`: a message waiting to be taken |
| `<id>.taken.json` | the courier, renaming the offer | a message the receiver has taken and not yet answered |
| `<id>.answer.json` | the courier | `{schema_version, answer}`, where `answer` is `{"disposition": ...}`, `{"closed": {"reason": ...}}` or `{"refused": {"reason": ...}}`; its sender reads it and removes it |
| `<id>.withdrawn` | a sender, renaming its offer | a message its sender took back, removed at once |
| `admission.json` | the receiver, on bind; removed on unbind | `{schema_version, declaration, clock}`: the advertisement that the receiver acknowledges admission and honours a withdrawal after it, trusted only while `admission.lock` is held, `spool.json`'s identity is the `declaration` recorded (`unix:<device>:<inode>`; or `unidentified`, written where a file has no identity, whose admission is waited for but never withdrawn from), and the `clock` (`linux-monotonic:<boot id>:<time namespace inode>`, or `…:none`; absent where the receiver identifies none) is the sender's own; unknown fields are read past |
| `admission.lock` | the receiver, on bind | locked exclusively for as long as a receiver that advertises is bound; the system releases it when that process ends, however it ends |
| `<id>.admitted.json` | the courier, as the inbox queues the message | `{schema_version}`: the message is admitted |
| `<id>.claimed` | the receiver taking it, or its sender withdrawing it, whichever comes first | empty, created exclusively; the receiver removes it on a release, and removes it with the message's other files once it is answered or withdrawn |
| `<id>.deadline.json` | a sender in `wait_until`, before it waits | `{schema_version, clock, deadline}`: the deadline in nanoseconds on the monotonic `clock` named as in `admission.json`, past which a release or take expires the message; removed with the message's other files |
| `<id>.expired` | the receiver, releasing or taking the message past its deadline | empty: the message is dropped from the inbox, and its sender removes it with the message's other files |

`<id>` is the minting instant in nanoseconds since the Unix epoch (39 digits),
the minting process's id, and a per-process counter (20 digits), joined by `-`:
the courier takes offers in the order of their minting instants, which is
the order they were made unless the system clock steps back. The exact documents are
`docs/contract.md`'s `spool-documents` fixture, which the core's contract test
holds to what a bound spool writes.

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
