# The transport

Everything durable the bus does goes through one trait, `Transport`: a queue's
records, where a consumer has read up to, the small documents kept beside a
queue, the exclusive section a claim needs, and change detection. Nothing above
it names a file. The local transport is the first transport rather than the
only one: a distributed transport — NATS JetStream is the example this document
maps — is a new crate or a plugin executable, and no consumer changes.
`docs/contract.md` (Contract T) is the text this document restates.

## The trait

```rust
pub trait Transport: Send + Sync + 'static {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError>;
    fn read(&self, queue: &QueueName, from: Option<&Position>, limit: usize) -> Result<Batch, TransportError>;
    fn cursor(&self, queue: &QueueName, consumer: &ConsumerName) -> Result<Option<Position>, TransportError>;
    fn commit(&self, queue: &QueueName, consumer: &ConsumerName, at: &Position) -> Result<(), TransportError>;
    fn exclusive(&self, queue: &QueueName, body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>) -> Result<(), TransportError>;
    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError>;
    fn wait_for_change(&self, queue: &QueueName, since: &Fingerprint, timeout: Duration) -> Result<Changed, TransportError>;
    fn document(&self, queue: &QueueName, name: &DocumentName) -> Result<Option<Vec<u8>>, TransportError>;
    fn replace_document(&self, queue: &QueueName, name: &DocumentName, bytes: &[u8]) -> Result<(), TransportError>;
}
```

| method | responsibility |
| --- | --- |
| `append` | add one record — one non-empty line of bytes — to a queue, in total order per queue, and answer the position just after it |
| `read` | the records after a position (after nothing, from the start), oldest first, at most `limit`, each with the position after it; a torn trailing record is reported in `Batch::torn`, never dropped and never fatal; a position the queue no longer reaches is `TransportError::PastEnd` |
| `cursor` / `commit` | where a named consumer has read up to, and recording it |
| `exclusive` | run `body` with nothing else appending to the queue until it returns; `body` is handed the transport to use inside the section |
| `fingerprint` / `wait_for_change` | a cheap token that moves whenever the queue's records or cursors do, and a bounded wait for it to move |
| `document` / `replace_document` | a small named document beside a queue, read whole and replaced atomically |

The trait is **object-safe**, because a transport is chosen at runtime from
configuration and held as `Arc<dyn Transport>`, and every type it names can be
built outside the core, so a transport written in another crate is a peer of
the two here. `crates/onemessagebus-e2e/tests/plugin_transport.rs` is that
proof: a directory-of-files transport with a layout of its own passes the same
queue, subscription and author table as the local and memory transports.

**Queue, consumer and document names** are validated where they enter: a queue
or consumer name is ASCII letters, digits, `-` and `_`, starting with a letter or
digit; a document name may also hold `.`, and may not end in `.jsonl`, `.torn`,
`.staging` or `.lock` or hold `-cursor.`, so a document never overwrites a
queue's own files.

## Positions and fingerprints

A `Position` is opaque to consumers: it comes from `append` and `read` and is
handed back to `read` and `commit`, never built from a number a consumer made
up. It serializes as its transport's token, so a cursor survives a process. A
`Fingerprint` is compared and nothing else. `Position::from_token` and
`Fingerprint::from_parts` exist for a transport's own implementation.

## The local transport

`LocalTransport::open(dir)` keeps every queue in one directory, laid out as
`onepipeline` lays out a run's `channel/` directory:

| what | file |
| --- | --- |
| a queue's records, one JSON line each | `<dir>/<queue>.jsonl` |
| the `default` consumer's cursor | `<dir>/<queue>-cursor.json` |
| another consumer's cursor | `<dir>/<queue>-cursor.<consumer>.json` |
| a named document | `<dir>/<name>` |
| a queue's exclusive section | `<dir>/.lock/<queue>.lock` |
| the fragments an append healed away | `<dir>/<queue>.jsonl.torn` |

- A **position** is the byte offset at a record boundary.
- A **cursor file** holds the **number of records** before the position,
  pretty-printed as one JSON number, rather than the offset: that is what
  `onepipeline` writes in `replies-cursor.json` and `commands-cursor.json` (the
  id of the last record claimed, plus one), and the transport converts between
  the two, so the files stay byte-identical while a position keeps its meaning.
  A cursor file that is not a number reads as a consumer that has read nothing,
  as `onepipeline` reads it.
- A **fingerprint** is each file's length and modification time — the queue's
  records file and its cursor files — as `onepipeline` marks its channel files;
  `wait_for_change` polls it.
- An **append** takes the queue's lock, heals a torn tail a dead writer left —
  truncating the file back to its last record boundary and recording the
  discarded bytes in `<queue>.jsonl.torn` — and writes the record and its newline
  in one write, rolled back if the write fails. An exclusive section heals the
  same way on its way in, so what it reads ends on a boundary.
- **Reads take no lock**, and a document is replaced by writing beside it and
  renaming it on.
- The lock is `<dir>/.lock/<queue>.lock`, so every writer to a directory must be
  a writer through this transport: `onepipeline` 0.28.2 locks the records file
  itself instead, and the two are not meant to append to one directory at the
  same moment. One reading the other's directory, and appending after the other
  has finished, is what is proven.

## The memory transport

`MemoryTransport::new()` keeps every promise the local transport keeps with
nothing on disk, for tests: a position's token is the number of records before
it, and clones share one store.

## Kinds, and how one is chosen

A configuration's `transport.kind` resolves in one order:

1. **built in** — `local` and `memory`;
2. **registered in-process** — `TransportKinds::register(kind, factory)`, by a
   consumer linking a transport crate;
3. **a plugin on `PATH`** — an executable named
   `onemessagebus-transport-<kind>` (`.exe` on Windows).

The first that knows the kind opens it, and a kind none of them knows is refused
naming every kind there is. `onemessagebus transports` lists them, with each
plugin's path. A built-in kind refuses a `transport` key it does not take by
name; a registered or plugin kind is handed every key.

## The plugin protocol

A plugin executable serves one transport over its stdin and stdout, one JSON
object per line in each direction, at protocol version **1**. A plugin author's
whole `main` is `onemessagebus::transport::serve(open, stdin, stdout)`;
`onemessagebus::ProcessTransport` is the client the core spawns a plugin with.

1. The client's first line is the hello, which names the protocol and its
   version and carries the `transport` block the plugin opens:
   `{"protocol": "onemessagebus-transport", "version": 1, "config": {"kind": "nats", "url": "nats://h:4222"}}`.
   The plugin answers `{"ok": {"hello": {"protocol": "onemessagebus-transport", "version": 1}}}`,
   or refuses a hello at another protocol or version, naming both.
2. Every later line is a request, discriminated by `op` — `append`, `read`,
   `cursor`, `commit`, `fingerprint`, `wait_for_change`, `document`,
   `replace_document` — each answered with one reply: `{"ok": <answer>}` or
   `{"error": {"kind", "message", ...}}`.
3. `exclusive` is `begin_exclusive`, answered once the section is held; every
   request until the matching `end_exclusive` is served inside the section; and
   the end is answered once the section is let go.
4. An error's `kind` is `past_end` or `not_a_boundary` — carrying the queue and
   the positions, so a queue reading the reply folds its log again — `refused`
   for anything else the transport refused, or `protocol` for a line the plugin
   could not read or one out of its place.
5. Records and documents travel as UTF-8 text; a record that is not UTF-8 is
   refused by the client before it is sent. A wait is sent as a succession of
   bounded waits, so another handle's request is never held behind one.

The three shapes are registered as `onemessagebus.transport-hello@1`,
`onemessagebus.transport-request@1` and `onemessagebus.transport-reply@1`, so a
client in another language validates against them.
`crates/onemessagebus-e2e/src/bin/onemessagebus-transport-dirfiles.rs` is a whole
plugin, and the journeys point the `onemessagebus` binary at it through a
configuration file alone.

## A NATS JetStream transport

Each method is one JetStream primitive, so a JetStream transport is one crate
implementing the trait — or one `onemessagebus-transport-nats` executable — and
no consumer changes:

| method | JetStream |
| --- | --- |
| `append` | publish to the queue's subject on a stream with limits retention and no discard (`Retention::Keep`); the position is the stream sequence the publish acknowledgement answers |
| `read` | a direct get, or an ordered consumer, from the sequence after `from`, for up to `limit` messages; a message is whole, so `torn` is always absent; a sequence past the stream's last is `PastEnd` |
| `cursor` / `commit` | a durable consumer per queue and consumer name, its acknowledged sequence the cursor — or a key per consumer in a KV bucket of cursors |
| `exclusive` | a per-queue lock: a key in a KV bucket created with compare-and-set on revision 0 and a TTL, released when the body returns |
| `fingerprint` | the stream's last sequence for the subject, and the cursor bucket's revision |
| `wait_for_change` | a KV watch, or a fetch with the timeout as its expiry |
| `document` / `replace_document` | a get and a put in a KV bucket of documents |

A configuration names it as it names any kind:
`transport: {kind: nats, url: nats://h:4222, stream: channel}`.
