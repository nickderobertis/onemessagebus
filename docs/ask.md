# Asking and answering

A question asked on a queue is a record carrying a **correlation** the bus
minted, and only a reply echoing that correlation answers it.

Every rule below was an incident first, in the wrappers this replaces
(`ask-manager.sh`, `ask-manager-contract.sh` and `channel-reply.sh` in
`ai-orchestrator`). Those accounts are kept at the end, because they are why the
rules are what they are.

## Asking

`Bus::ask::<Q, R>(queue, question, AskOptions { blocking, asker, about })` raises
`question` on `queue` and hands back a `Pending<R>`, the handle its answer
arrives on. The queue is an event queue that declares the queue its answers are
appended to (`answers`); one that keeps no events, or names no answer queue, is
refused naming why.

The question is checked against `Q`'s schema and stamped with four fields the
bus owns:

| field | what it holds |
| --- | --- |
| `correlation` | the minted token only a reply echoing it answers |
| `blocking` | whether the asker waits on the answer; a blocking question is claimed first and held pending, where the queue's policy says so |
| `asker` | who asks, when an asker is named: a later listener of the same asker takes the question back |
| `about` | what the question is about, when named; the `planner-channel` layout carries it as the surface's `workstream`, where `onepipeline` reads it |

Then it is shaped by the layout, judged by the queue's validators, validated
against the queue's schema and appended. A refusal at any step appends nothing.

A `Correlation` is `c-` and 32 hex digits of the operating system's randomness,
checked against every question the queue already holds; text from outside — a
`--correlation` flag — is refused unless it is 1 to 128 ASCII letters, digits,
`.`, `_`, `:` and `-`, starting with a letter or a digit. An `Address` is one
non-blank line of up to 512 bytes.

## Waiting

`Pending::wait(timeout)` answers an `Answer<R>`, and there are exactly four:

| answer | when |
| --- | --- |
| `Reply(R)` | a reply record on the answer queue echoes this correlation, and reads as an `R` |
| `Timeout` | the wait elapsed with none; the question stands |
| `Abandoned` | no reply, and the question is marked abandoned with nobody re-attending it |
| `Refused(Refusal)` | the bus refused the question or the reply: the reply record echoing the correlation does not satisfy the answer queue's schema or `R`'s, naming the id and the JSON pointer; or the answer queue could not be read |

**There is no path from a timeout, a session bound or an abandoned listener to
an `R`.** `Reply` is only ever built from a reply record echoing the
correlation, and waiting appends nothing anywhere: a wait that elapses while
another question's reply sits on the answer queue answers `Timeout`, and that
reply stays where it is for the question it answers. The first reply echoing a
correlation is its answer; a later one is kept and ignored.

## Listeners: abandon and re-arm

A `Pending` is a listener, and a listener can go away without its answer.

- `Pending::abandon()` marks the question abandoned — an `abandoned` event on
  its queue, which `status` reads as "nobody is waiting now". The record is kept,
  readable, claimable and still answerable: a reply arriving for an abandoned
  question is matched to it, never dropped. There is no `Drop` side effect; a
  listener that ends says so.
- `Pending::rearm()` takes the question back after a lost wait: it is attended
  again, and the next wait waits for its reply.
- `Bus::listen::<R>(queue, correlation, lifetime)` re-attaches to a question
  already asked, raising nothing. A `Lifetime::Durable(asker)` naming the asker
  the question carries re-arms it; any other listener — a session, or another
  asker — attends nothing, so for it a question left abandoned answers
  `Abandoned`. A correlation no question on the queue carries is refused naming
  it.

## Replying

`Bus::reply(queue, correlation, reply)` binds a reply to a pending ask:

- An ask is **pending** from when it is queued until a reply echoing its
  correlation is on the answer queue — abandoned or not, claimed or not.
- A reply naming a correlation binds to the pending ask carrying it. One naming a
  correlation nothing pending holds — unknown, or already answered — is refused
  naming it, with nothing appended: the guard `channel-reply.sh` kept by reading
  `queue.json`, made the bus's.
- A reply naming none binds to the queue's pending ask when exactly one is
  pending, and is refused otherwise, naming how many are and which.
- A reply is a JSON object on every answer queue, since the correlation it is
  bound by is one of its members; one that is not is refused before it is bound,
  with nothing appended.

The reply is shaped by the layout as an offer to the answer queue, the record
that lands there is stamped with the correlation, the offer is judged by the
validators, and it is appended. A question holding the queue's pending slot is
released.

**Routing by shape stays where `onepipeline` put it.** Under `planner-channel`
the answer queue is `replies`, and the layout routes a reply envelope by its
halves through the `Router` the profile declares for it
(`onemessagebus_agent::channel::ReplyRouter`): one carrying a verdict and edits
reaches both `replies` — answering the ask — and `commands`; one carrying only
commands reaches `commands` alone, and the ask stays pending. The meaning of the
edits stays the consumer's.

`Bus::reply_at(queue, position, reply)` answers the record pending at a claim
position instead, the way `reply <queue> <position>` always has, stamping the
pending question's correlation on the reply where it carries one.

## The command line

- `ask <queue> [--blocking] [--asker WORD] [--about ADDRESS] [--timeout SECONDS]`
  reads the question on stdin or `--file`, prints `correlation: <c>` on stderr as
  soon as the question is on the queue, waits, and prints the answer on stdout:
  `{"answer": "reply", "correlation", "reply"}`, or `{"answer": "timeout" |
  "abandoned", "correlation"}`, or `{"answer": "refused", "reason"}`. It exits 0
  for a reply alone. **Every other answer carries the `answer` word and no
  `reply` member**, so a caller that reads only the reply reads nothing. An `ask`
  that ends without an answer abandons its question, as a listener that ended.
- `ask <queue> --correlation <c> [--asker WORD] [--timeout SECONDS]` re-attaches
  to the question `c` minted, raising nothing: under the question's own asker it
  re-arms it and waits; under none, it attends nothing, and a question left
  abandoned answers `abandoned`.
- `reply <queue> --correlation <c>` binds the reply on stdin to that ask;
  `reply <queue>` with neither a correlation nor a position binds to the one
  pending ask; `reply <queue> <position>` answers the record claimed there.
- Input either verb refuses — a document that is not JSON or not a JSON object,
  a malformed or over-long correlation, a usage error — exits 2 with the problem
  on stderr and nothing on stdout, as `docs/cli.md`'s exit codes give it; a
  well-formed question or reply the bus says no to exits 1.

## What the wrappers measured

- **A server answered its own timeout with a plausible ruling.** `onepipeline
  channel serve` printed `{"completion": false, "message": "no planner reply
  within the timeout; continue", "reason": "the channel timed out waiting for a
  verdict"}` at exit 0 when its reply window elapsed. A caller checking the exit
  status acted on it as the manager's answer, and a fabricated verdict is worse
  than none because it is actionable. `ask-manager.sh` refused that exact
  `reason` string; here the shape is unrepresentable instead — a timeout is
  `Answer::Timeout`, and there is no `R` in it.
- **A reply was claimed by whichever reader arrived next.** On a live run a
  re-ask was handed a monitor's live graph edit addressed to the engine, because
  it happened to be the next reader of the reply queue. The wrapper minted a
  correlation token per question, asked the manager to echo it inside the
  ruling's `message`, and discarded rulings that did not. Here the correlation is
  a field the bus stamps and a reply binds by, and a wait never consumes a reply
  it does not match — so the stale answer that outlived its asker is never
  drawn, and there is nothing to re-ask for.
- **Re-arm, not re-ask.** When the wrapper did draw another question's answer,
  its own question was still pending, so it re-armed a listener rather than
  raising a second blocking question — a duplicate blocking question is
  self-sustaining: the manager answers both, and the copy nobody claims is the
  stale ruling the next ask draws. `Pending::rearm` and `ask --correlation` are
  that re-arm.
- **A listener is rented, the asker is not.** A session naming the same asker as
  an earlier one takes back what that one abandoned; a session naming none takes
  nothing and is taken by nothing — which is why the wrapper named its asker from
  its own token when nothing else named one.
- **A reply sent to a question nobody had handed out reached nobody.**
  `channel-reply.sh` read `queue.json` and refused a ruling echoing the token of
  a blocking question still waiting to be read, because the manager would have
  been told `delivered` while no reader could claim it. A reply bound by
  correlation reaches its question whether or not it was claimed first, and one
  echoing a correlation nothing pending holds is refused naming it.
