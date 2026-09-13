# The command line

`onemessagebus` has two verb families: `schema`, over the registry, and
`events`, over NDJSON streams. Every verb is a capability in
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
  validated. A verb that takes no payload takes none.
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

## Exit codes

| code | meaning |
| --- | --- |
| `0` | The verb did what it was asked. |
| `1` | Well-formed input whose answer is no: a payload that violates its schema, a stream that could not be written. |
| `2` | Input the verb refuses: a malformed id or filter, an unknown profile, an unsupported language, an unregistered id, a usage error. |

Refusals go to stderr as `onemessagebus: <what is wrong>`; stdout carries
answers only.

## `schema`

### `schema list [--registry DIR] [--format json|text]`

Every registered id — the profile's and the registry directory's — and nothing
else. JSON is a list of `{id, family, version}`; text is one id per line.

### `schema check <id> [--file PATH] [--registry DIR]`

Validate the payload on stdin (or in `--file`) against the schema registered
under `<id>`. Exit 0 when it conforms; exit 1 naming the id and the JSON
pointer of the first violation when it does not.

```bash
$ echo '{"run_id":"R","round":"two"}' | onemessagebus schema check agent.labels@1
onemessagebus: agent.labels@1: at /round: "two" is not of types "null", "integer"
```

### `schema gen --lang json|rust|python|typescript <id> [--registry DIR]`

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

### `schema register <id> --file <schema.json> [--registry DIR]`

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
