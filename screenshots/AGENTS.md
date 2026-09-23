# Terminal screenshots

Deterministic SVG captures of the real `onemessagebus` binary, gated on their
content hashes by [screencomp](https://github.com/nickderobertis/screencomp) and
informational: outside `just check`, `just gate` and CI's gate job, beside
`deps-check` and the llmlint tier.

## A capture is a rendering of the CLI surface, not a second statement of it

This repository refuses a second spelling of its contract, and a committed picture
of CLI output would be one if a person had written it. None of these is: each is
produced by running the real release binary, and CI refuses it the moment its bytes
leave the committed digest. The binary stays the one source and the baseline **is**
the drift gate. Same reasoning for the `$` prompts — they are rendered from the argv
each scene runs, because a prompt typed beside the command goes stale in silence.

## Why these scenes

Each still answers *what does this surface look like, which the prose cannot say?*
`ask`, because the wait between the correlation and the answer is the point and a
second process is what ends it. `queues`, because `status --format text` is the most
structured human view here. `schema`, because the refusal is what the verb is for
and a bare listing is not. `refusal`, because its one-line shape and exit code are
contract. `events-merge` and `serve`, because one line per envelope and one response
per frame are shapes a reader otherwise has to imagine. The hero is none of them:
watching a queue fill is what a bus looks like in use, and a still of that text says
strictly less.

A picture that says less than the prose beside it is padding, and two obvious
candidates are out on that ground — `transports` renders the two lines its sentence
already says, and `--help` is uncoloured through a pipe *and* a pty at 219 columns.
A new scene earns its place the same way.

## What it is pinned to, and why that is the whole contract

Two captures of one build must produce identical bytes. An SVG is layout maths
rather than pixels, so no container is needed — but nothing may vary:

- **the renderer**, by `freeze_version` in `install-freeze.sh`, the one place it is
  stated, and its download is checked against a digest pinned in this repository
  rather than one served beside it;
- **the font**, vendored so nothing is fetched and embedded so nothing is loaded;
- **the environment**: every ambient `ONEMESSAGEBUS_*` variable is cleared, because
  the queue verbs read them ahead of this capture's own flags;
- **the per-run values**, rewritten by `normalize.sh`. Normalising is the answer
  rather than a fixed-clock flag because a flag on this CLI is a capability-manifest
  change first, then a method in both SDK clients and a parity audit: the price of a
  convenience here is paid across the whole product surface.

Everything else a scene shows is already a function of fixed content, and the
fixture links the journeys' own layout rather than copying it, so a change there
moves both.

## Lanes

One lane, `x86_64`. A lane is a promise that somebody has captured a baseline on
that arch, so declare one only together with a bless from such a host: the guard is
local, and refuses an arch no lane declares rather than classifying against
another's.

## The hero is not hash-gated

A GIF is not byte-reproducible across rendering libraries, so it is regenerated on
demand and committed. Regenerate it when `subscribe`'s rendering changes.

## The guard, and why it is active

A committed hook that nothing activates runs nothing, so `just bootstrap` sets
`core.hooksPath`. **That directory carries the visual guard and nothing else**:
wiring `just gate` into a push is a change to everyone's development loop that this
adoption deliberately did not make.

## When the output changes

Expected, not a regression. An intended change owes `just screenshots-bless`;
`just screenshots` is for looking first, when you are not yet sure the change was
intended. Commit `shots/baseline/` together with `docs/screenshots/`: an image
the README embeds beside a digest that no longer matches it is the one state this
arrangement cannot survive. Bumping the renderer or the font reflows every shot,
so bless once.
