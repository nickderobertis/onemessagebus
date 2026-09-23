# Terminal screenshots

Deterministic SVG captures of the **real** `onemessagebus` binary, gated on their
content hashes by [screencomp](https://github.com/nickderobertis/screencomp).
Informational, like `deps-check` and the llmlint tier: never part of `just
check`, `just gate`, or `ci.yml`'s gate job. `.github/workflows/visual-docs.yml`
owns the comparison on pull requests; `.githooks/pre-push` is the same gate's
local half.

## Why a capture is a rendering of the CLI surface, not a second statement of it

`docs/contract.md` is the approved contract and `docs/cli.md` the command line's
reference; this repository refuses a second spelling of either, and llmlint's
`contracts_have_one_source_or_a_drift_gate` is the rule that says so. A committed
picture of CLI output *would* be such a second spelling if a person had written
it. None of these is: every scene is produced by running the real release binary,
and CI refuses it the moment its bytes diverge from the committed digest. The
binary stays the one source, the baseline **is** the drift gate, and a shot that
has gone stale fails rather than lies.

The same reasoning is why the transcripts carry `$` prompt lines, and why they
are *rendered from* the argv each scene executes rather than written beside it: a
prompt typed by hand is a second spelling of the command line that nothing
reconciles, and it goes stale silently. What a scene prints is the command the
picture is of, shell-quoted so pasting it runs it.

## Why these scenes

Each still sits in the README section that explains its surface, so the question
each one answers is "what does this surface look like, which the prose cannot
say?" — `ask` because the waiting between the correlation line and the answer is
the whole point and a second process is what ends it; `queues` because
`status --format text` is the most structured human view the tool has; `schema`
because the refusal is what the verb is *for* and a bare listing is not;
`refusal` because the one-line shape and its exit code are a documented contract;
`events-merge` and `serve` because one line per envelope and one response per
frame are shapes a reader otherwise has to imagine.

The **hero** is none of these. `docs/screenshots/subscribe.gif` is an animated
`subscribe` tail, because watching a queue fill is what a bus looks like in use
and a still of the same text says strictly less.

A picture that says less than the prose beside it is padding, so two obvious
candidates are deliberately absent, and should stay absent:

- **`transports --format text`** renders two lines on a stock install, which is
  exactly what the sentence beside it says. A plugin row would mean photographing
  this machine's absolute path to a built plugin.
- **`onemessagebus --help`** is not colourised — this build links no colour crate
  and clap's styling is off through a pipe *and* through a pty — and its longest
  line is 219 columns. The shot would be a wrapped monochrome wall of words the
  README's sections already carry.

## The fixture: free, offline, and the journeys' own

`stage-fixture.sh` writes what `desk_config` writes for the end-to-end journeys,
and links the journeys' own `desk` layout document rather than a copy, so a change
to the layout moves both. It is one script because the stills and the animated
hero must be taken over the same bus, not two that drift. **No model, no network, no credential, no cost**: the
transport is a directory of files and the codec's frame bundle is reached by a
`file://` link, so even the link machinery runs with no HTTP. `fixture/` holds
only what the journeys do not already provide.

## Why it is byte-reproducible, and what it is pinned to

screencomp gates on the **hash** of each image, so two captures of one build must
produce identical bytes. Unlike a rasterised PNG — whose anti-aliasing drifts
across CPUs, which is why a web app captures inside a pinned browser container —
an SVG is pure layout maths. Three things are pinned and one class of value is
normalised:

- **`freeze`**, by `freeze_version` in `install-freeze.sh` — the one place it is
  stated, run by both `just screenshots-tools` and the workflow's capture step,
  so there is no second copy to keep in step. What it downloads is checked
  against a digest pinned here rather than one served beside the download.
- **The font**, vendored under `fonts/` (OFL) and embedded into each SVG as
  base64, so `freeze` fetches nothing and the file renders the same on GitHub and
  crates.io. The file is the one statement of which font this is.
- **The environment**: every ambient `ONEMESSAGEBUS_*` variable is cleared before
  a scene runs, as `tests/e2e/support.rs` clears them for the journeys. They are
  read ahead of the capture's own flags, so an exported one would steer a scene
  away from the fixture and drift its hash against a baseline captured in a clean
  shell.
- **Per-run values are normalised** after the verb has run, by `normalize.sh` —
  the one place they are written. Normalising is the answer rather than a
  `--fixed-clock`-style flag because a flag on this CLI is a capability-manifest
  change first, then a method in both SDK clients and a parity audit: the price
  of a convenience here is paid across the whole product surface. Everything a
  scene shows but those values is already a function of fixed content.

The workflow's two further pins are held to their sources by
`crates/onemessagebus-repo/tests/visual_docs_pins.rs`: its Rust container and
`RUSTUP_TOOLCHAIN` to `rust-toolchain.toml`'s channel, and its
`screencomp-version:` input to the reusable workflow's `uses:` ref — which is the
one `screencomp doctor --env` reconciles against the installed CLI.

## Lanes

screencomp scopes captures per CPU arch. `[capture].arches` in `screencomp.toml`
is the single place the set is declared and `host-arch.sh` the single place a lane
name is derived from `uname -m`.

One lane, `x86_64`. A lane is a promise that somebody has captured a baseline on
that arch, so declare one only together with a bless from such a host: the
guard is local and refuses an arch no lane declares rather than classifying
against somebody else's.

## The animated hero

`subscribe-gif.py` spawns the real `subscribe`, reads its stdout **with the
instant each line landed**, drives a sibling `send` alongside it, and replays
those lines at their real inter-arrival gaps — so the backlog lands in a burst and
then records arrive one at a time, which is what the tail really does. Nothing
redraws, so there are no frames to reconstruct and Pillow is all it needs.

Unlike the SVGs it is **not** hash-gated: a GIF is not byte-reproducible across
rendering libraries. It is regenerated on demand and committed, so regenerate it
when `subscribe`'s rendering changes.

## The strict gate, and how its local half is activated

CI runs screencomp's reusable workflow with `fail-on-drift: true`. Locally the
guard re-captures only when a `[guard].paths` file changes, and on drift it
re-blesses this host's lane, builds a review gallery and **blocks the push**, so
the refreshed baseline and README images are committed deliberately.
`crates/onemessagebus-repo/tests/visual_docs_guard.rs` drives that hook the way
git does, with the three tools it shells out to stood in for.

A committed hook that nothing activates runs nothing, so `just bootstrap` sets
`core.hooksPath`. **That directory carries the visual guard and nothing else**:
wiring `just gate` into a push is a change to everyone's development loop that
this adoption deliberately did not make.

## Changing the screenshots

Editing the CLI surface or its renderings, the core they answer from, the `desk`
layout, or the scenes changes the SVGs. That is the gate working, not a
regression: `just screenshots` recaptures, and `just screenshots-bless` does that
and blesses the new bytes into this host's lane, which is what an intended change
owes. Commit the refreshed `shots/baseline/` together with `docs/screenshots/` —
an image the README embeds and a digest that no longer matches it are the one
state this arrangement cannot survive. Bumping the renderer or the font reflows
every shot, so bless once rather than per scene.
