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

The same reasoning is why the transcripts carry `$` prompt lines: each is the
argv the capture actually ran, printed beside the bytes running it produced — not
prose about the command.

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

The capture stages what `desk_config` stages for the end-to-end journeys, and
links the journeys' own `desk` layout document rather than a copy, so a change to
the layout moves both. **No model, no network, no credential, no cost**: the
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
- **Per-run values are normalised**, because this CLI has **no clock-override and
  no fixed-id switch, and this adoption did not add one**: a flag here is a
  capability-manifest change first (a test walks the clap tree refusing a flag
  with no binding), then a method in both SDK clients and a parity audit. So the
  correlation `ask` mints and the epoch instants the desk stamps are rewritten to
  fixed placeholders after the verb has run. Everything else in every scene —
  positions, counts, ids, orderings — is already a function of fixed content.

The workflow's two further pins are held to their sources by
`crates/onemessagebus-repo/tests/visual_docs_pins.rs`: its Rust container and
`RUSTUP_TOOLCHAIN` to `rust-toolchain.toml`'s channel, and its
`screencomp-version:` input to the reusable workflow's `uses:` ref — which is the
one `screencomp doctor --env` reconciles against the installed CLI.

## Lanes

screencomp scopes captures per CPU arch. `[capture].arches` in `screencomp.toml`
is the single place the set is declared and `host-arch.sh` the single place a lane
name is derived from `uname -m`.

One lane, `x86_64`. The SVGs' bytes do not depend on the CPU, so a second lane
would be *safe* — but its committed baseline would be one nothing here has ever
captured, and the guard is local: it classifies the lane of the host it runs on
and refuses an undeclared arch rather than guessing. To gain a lane, declare the
arch, bless it from such a host, and commit its baseline; CI fans a job out per
lane on its own.

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
`core.hooksPath` — that is the whole body of the `onemessagebus-visual-docs`
project's `bootstrap` target. **That directory carries the visual guard and
nothing else**: `just gate`, this repository's complete pre-push bar, is
deliberately not wired into it, because what the gate is and when you run it did
not change.

## Changing the screenshots

Editing the CLI surface or its renderings, the core they answer from, the `desk`
layout, or the scenes will change the SVGs. That is expected: run `just
screenshots-bless` and commit the refreshed `shots/baseline/` with
`docs/screenshots/`. `just screenshots` captures without blessing,
`just screenshots-tools` installs the renderer, `just screenshots-gif` redraws
the hero, and `screencomp doctor --env` says whether the setup is actually wired.
Bumping the renderer or the font reflows every shot; bless once.
