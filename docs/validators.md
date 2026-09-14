# Validators

A validator is a judgement a queue makes of a message **before anything is
appended**.

## Verdicts

A validator answers one verdict for one message:

| verdict | on the wire | what happens |
| --- | --- | --- |
| pass | `{"verdict": "pass"}` | the message may be sent |
| refuse | `{"verdict": "refuse", "reason": "..."}` | nothing is appended anywhere, and the caller gets the reason the validator gave, unaltered |
| unjudged | `{"verdict": "unjudged", "reason": "..."}` | the message could not be judged, so it is not sent either |

**An unjudged message never passes.** A reviewer out of quota, a command that
cannot be started and a bar that crashed all answer "not judged", which is not
"judged fine".

`Validators<M>` is an ordered list. **Every one runs**, whatever an earlier one
answered; the first refusal is the verdict; with no refusal, the first unjudged
one is; with neither, the message passes.

## Where they run

- `Queue::push` and `RawQueue::push` judge the record as it was offered, then
  validate it against the queue's schema, then append it.
- `Bus::send`, `Bus::reply` and `Bus::ask` judge an **offer**: the message by the
  validators of the queue it was offered to, and each record the layout routes to
  another queue by that queue's validators. All of it is judged before any of it
  is appended, so a refusal anywhere leaves every queue as it was — a planner
  channel reply carrying a verdict and edits is refused whole, not half-sent.
- `Bus::validate` (the command line's `validate`) judges an offer exactly as a
  send would, and appends nothing.

## Deterministic validators

A deterministic validator is a Rust type implementing
`Validator<M: Message>`. It is code, so it is registered in code rather than in
the configuration file: `Queue::with_validators` on a typed queue, and
`Bus::with_validator` on a queue a configuration declares — where a record that
does not read as an `M` is refused naming why.

## The external validator

`CommandValidator` runs a command with the message as JSON on its stdin (one
line) and `ONEMESSAGEBUS_VALIDATE_QUEUE` naming the queue:

| the command | the verdict |
| --- | --- |
| exits 0 | pass |
| exits 1 | refuse, whose reason is its stderr exactly as written (one it left empty is reported as a refusal that wrote no reason) |
| exits anything else, is ended by a signal, or cannot be started | unjudged, naming the exit and what it wrote on stderr |

Its stdout is discarded: the command line's stdout is a contract, and a
validator's chatter is not part of it.

## The pass cache

A `PassCache { dir, bar_fingerprint }` beside a command validator records each
**pass** — never a refusal, never an unjudged verdict — so a message judged again
under the same bar is passed without running the command. A record is keyed on
two digests, both SHA-256:

- the **content**: the message's JSON bytes as the validator would read them;
- the **bar**: the validator's argv and the fingerprint `bar_fingerprint` prints
  (its stdout, trimmed) when it is run at the moment of judging.

Moving the bar — a new reviewer prompt, a new criteria revision — prints another
fingerprint, so every record made under the one before misses and the command
runs again. A fingerprint that cannot be read (the command fails, or prints
nothing) keys nothing: the message is judged by the command and nothing is
recorded. A record is one file, `<dir>/<content>.<bar>.pass.json`:

```json
{"schema_version": 1, "content": "<sha-256 hex>", "bar": "<sha-256 hex>", "fingerprint": "<what the bar printed>", "command": ["<argv>"]}
```

A record that cannot be written costs the next send a run of the command, never
a different verdict.

## The configuration file

```yaml
validators:
  - {on: replies, when: {carries: commands}, kind: command, command: [uv, run, python, -m, orchestrator.plan_review, --envelope],
     cache: {dir: .validator-passes, bar_fingerprint: [scripts/llmlint-fingerprint.sh]}}
```

| key | meaning |
| --- | --- |
| `on` | the queue whose offered messages it judges; one the configuration does not declare is refused by `Config::resolve`, naming `validators[<index>].on` |
| `when` | which messages it judges; every one when absent |
| `kind` | `command`, the external validator — the one kind a file declares |
| `command` | the argv, the program first; an empty one is refused naming `validators[<index>].command` |
| `cache` | `{dir, bar_fingerprint}`, both required; `dir` is relative to the working directory |

Every block refuses an unknown key by name. `when` takes one of two forms, side
by side:

- `{carries: <field path>}` — the message holds something at that field: it is
  there and is not `null`, `""`, `[]` or `{}`. `{carries: commands}` judges a
  reply envelope that carries edits and passes one that carries only a verdict.
  `carries` stands alone; a `when` naming it beside another key is refused.
- any queue predicate: `{field, equals}`, `{field, present}`, `{field,
  non_empty}`, `{all}`, `{any}`, `{not}`.

`carries` is a separate form from the queue predicate grammar.
