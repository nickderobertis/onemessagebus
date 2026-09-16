# Schema links

A program that owns a protocol publishes its grammar as a **schema bundle**, and a
bus configuration names that bundle by a **link** — a URL or a path, pinned to a
version — rather than transcribing the grammar or linking the program. A pinned
remote link resolves through an on-disk **cache**, so a serving session never
waits on the network when the cache holds what it needs.

This is Contract L, stated once: every consumer that adopts a link restates it
from this page.

## The bundle document

A link serves one JSON object:

```json
{"version": "8", "description": "optional text", "schemas": [{"id": "agent.example-frame.hello@8", "schema": {"type": "object"}}]}
```

- `version` (required): a string of one to three dot-separated non-negative
  integers (`^\d+(\.\d+){0,2}$`) — the version the document **declares**, which a
  pin is asserted against and a cache entry is keyed by.
- `schemas` (required, non-empty): each entry is exactly a registry document,
  `{"id": <schema id>, "schema": <JSON Schema object>}` — the shape one file of a
  `--registry` directory has. Ids within one bundle are unique.
- `description` (optional): text. Any other top-level key is refused, naming it.

Each refusal names what is wrong: an unknown key, a malformed `version`, an empty
`schemas`, an id declared twice (`schemas[1].id: … is already declared by
schemas[0]`), or an entry that is not a registry document (`schemas[<index>]`).

The type is `onemessagebus::SchemaBundle` in the core crate (serde and
`JsonSchema`): a publisher in another repository generates its bundle through
that one declaration — `SchemaBundle::new` refuses what a reader would — rather
than restating the shape.

## The link

A link is a string `<location>` or `<location>@<pin>`. The pin is the text after
the last `@` that follows the location's last `/`, when that text matches
`^\d+(\.\d+){0,2}$`; otherwise the link is unpinned and the `@` is part of the
location — `https://example.org/@scope/frames.json` names no pin, and
`https://example.org/pkg@2/frames.json@3` pins `3`. Locations:

- `https://…` — any host.
- `http://…` — only on the host `localhost`, `127.0.0.1` or `[::1]`; any other
  `http://` host is refused naming the link. It exists so suites can prove
  revalidation against a loopback server with no network.
- `file:///absolute/path`, or a bare path — absolute, or relative to the
  directory of the configuration file that names it. Read on every resolution
  and never cached; the pin is still asserted.

Anything else — another scheme, a URL with no host, a `file://` URL whose path is
not absolute — is refused naming the link. The type is
`onemessagebus::SchemaLink`.

## The pin

The pin is a prefix range over the declared version, a missing declared
component read as 0:

| pin | admits | refuses |
| --- | --- | --- |
| `@8` | `8`, `8.1`, `8.1.2` | `9`, `80` |
| `@8.1` | `8.1`, `8.1.7` | `8`, `8.2` |
| `@8.0` | `8`, `8.0.3` | `8.1` |
| `@8.1.2` | `8.1.2` | `8.1.3`, `8.1` |

A resolved document whose declared version the pin does not admit is a **hard
error** naming the link, the pin and the declared version — never a silent
fallback to something else:

```text
onemessagebus: schemas: https://example.org/frames.json@8: the bundle declares version 9, which the pin @8 does not admit
```

## The cache

The cache lives at `$ONEMESSAGEBUS_SCHEMA_CACHE_DIR`, else
`$XDG_CACHE_HOME/onemessagebus/schemas`, else `$HOME/.cache/onemessagebus/schemas`.
An entry is keyed by the location without its pin and the version the bundle
declares, and records the body, the validators the origin answered with (`ETag`,
`Last-Modified`) and when it was last confirmed. With no directory to name, a
pinned remote link is fetched on every resolution, as an unpinned one is.

Resolving a **pinned remote** link:

1. With no cached entry the pin admits: fetch, store under the version the body
   declares, and use it. A fetch that fails refuses, naming the link and why.
2. With an admitted entry confirmed within the freshness window
   (`ONEMESSAGEBUS_SCHEMA_TTL` seconds, default `3600`; `0` revalidates on every
   resolution): use the newest admitted entry, with no request.
3. With an admitted entry older than the window: a conditional request
   (`If-None-Match`, `If-Modified-Since`). A `304` re-confirms it. A `200` whose
   body the pin admits is stored as its own entry — beside the older one — and
   used. A `200` whose body the pin does not admit is the hard error above. A
   revalidation that cannot be made — offline, a transport failure, a timeout, a
   non-success status, or a `200` that is not a bundle at all — reuses the cached
   entry and says so on stderr: the cache is a speed-up, never a network
   dependency.
4. `ONEMESSAGEBUS_SCHEMA_REFRESH=1` forces an unconditional fetch that replaces
   what the cache holds for that location; a fetch that fails then refuses.

An **unpinned remote** link is fetched on every resolution and never cached.

**HTTP** is ureq over rustls with the ring provider — no system OpenSSL and no
spawned `curl` — trusting the platform's root certificates as
`rustls-native-certs` reads them: the system store, or, when `SSL_CERT_FILE` or
`SSL_CERT_DIR` is set, only the certificates those name. A request to an `https://` link goes through
`HTTPS_PROXY` and one to an `http://` link through `HTTP_PROXY` (either
spelling, the upper case first), unless `NO_PROXY` names the host — exactly, as a
`.example.org` or `*.example.org` suffix, or `*`. The bounds are fixed:

- **connect timeout, 5 seconds**: establishing the connection — TCP, a proxy's
  `CONNECT`, and the TLS handshake together.
- **read timeout, 15 seconds**: for the response's headers once the request is
  sent, and again for its body once they have arrived.

A fetch that crosses one is refused naming it: `the connection was not
established within the 5-second connect timeout`, `nothing was received within
the 15-second read timeout`. A bundle, fetched or read from a file, is at most 16
MiB.

## Where links are named

The configuration file has one optional top-level key for them:

```yaml
schemas:
  - "https://example.org/frames.json@8"
```

`Config::load` parses each link and refuses a malformed one, naming it, but
**resolves nothing**: resolution is the explicit library call
`Config::resolve_links(&LinkResolver::from_env()?, freshness)` (or
`LinkResolver::resolve` per link), so a library consumer that loads a
configuration for its queues never touches the network or the cache for links it
does not use.

Every command-line verb that loads a configuration makes that call, resolving
each link before it does anything else, and registers every entry of every
resolved bundle into the registry it uses — beside the profile's schemas and any
`--registry` directory. An entry whose id is already registered with a different
document is refused, naming the link and the id, as the registry refuses it. The
queue verbs (`send`, `next`, `ask`, `reply`, `subscribe`, `status`, `validate`,
`serve`) load one from `--config` or `ONEMESSAGEBUS_CONFIG`; the `schema` verbs
from `--config` alone, never from the variable.

`serve` resolves before it reads its first frame, and for `serve` alone an
admitted cached entry is used **without revalidation**, whatever its age, and
whatever `ONEMESSAGEBUS_SCHEMA_TTL` or `ONEMESSAGEBUS_SCHEMA_REFRESH` say: only a
link with nothing cached is fetched, and with that origin down `serve` refuses
before reading a frame, naming the link. `serve --resident` resolves the same
way when it starts; each request it answers resolves as its one-shot verb would.

A link that cannot be fetched or read, or a cache that cannot be written, exits
1; a malformed link, a bundle that is not one, a pin the bundle does not admit,
an unusable variable or a conflicting id exits 2. `--registry <dir>` keeps
working unchanged for local documents.

## The verbs

Each is a capability in `onemessagebus::CAPABILITIES`, with a method in both SDK
clients (`schemas`, `schemasClear` / `schemas_clear`, `schemasFetch` /
`schemas_fetch`).

- **`schemas [--format json|text]`** lists the cache: its directory, then one
  entry per cached location and declared version with its confirmation time
  (RFC 3339, UTC). JSON is `{"cache": <dir>, "entries": [{"url", "version",
  "confirmed_at"}]}`, by location then version; text is `cache <dir>` and then
  `<url> <version> <confirmed_at>` per line. An empty or absent cache is an empty
  list, exit 0.
- **`schemas clear [--format json|text]`** removes every entry and reports how
  many: `{"cache": <dir>, "removed": <n>}`, or `removed <n> from <dir>`. Exit 0,
  also when there were none.
- **`schemas fetch [<link>...] [--config PATH] [--format json|text]`** resolves
  each named link — or, when none is named, every link the configuration names
  (`--config` or `ONEMESSAGEBUS_CONFIG`) — **revalidating regardless of the
  freshness window**, conditionally where an entry exists, and reports per link
  how it ended: `{"links": [{"link", "outcome", "version", "reason"}]}`, where
  `outcome` is `fetched`, `confirmed`, `reused` (after a failed revalidation, with
  its `reason`), `read` (a file link) or `failed` (with its `reason` and no
  `version`). Text is `<link> <outcome> <version|-> [(<reason>)]` per line. Exit 0
  when every link ended resolved to a version its pin admits — a reused entry
  included; otherwise, after the report, exit 1 naming each link that did not,
  or exit 2 when any of them served a document that is not a bundle or a version
  its pin does not admit.
  This is the verb a host runs at session setup to warm the cache.

```bash
$ onemessagebus schemas fetch --config onemessagebus.yaml --format text
https://example.org/frames.json@8 fetched 8.1
```
