# Terminal screenshots

Deterministic SVG captures of the **real** `onemessagebus` binary's output, gated
on their content hashes by [screencomp](https://github.com/nickderobertis/screencomp).
Informational, like `deps-check` and the llmlint tier: **never part of `just
check`, `just gate`, or `ci.yml`'s gate job.** `.github/workflows/visual-docs.yml`
is a workflow of its own and owns the comparison on pull requests;
`.githooks/pre-push` is the same gate's local half.

## Why a capture is a rendering of the CLI surface, not a second statement of it

`docs/contract.md` is the approved contract and `docs/cli.md` is the command
line's reference; the repository refuses a second spelling of either, and
`llmlint`'s `contracts_have_one_source_or_a_drift_gate` is the rule that says so.
A committed picture of CLI output *would* be such a second spelling if a person
had written it. None of these is: every scene here is produced by running the
real release binary and is refused by CI the moment its bytes diverge from the
committed digest. The binary stays the one source, the baseline **is** the drift
gate, and a shot that has gone stale fails rather than lies.

The same reasoning is why the transcripts carry `$` prompt lines. Each one is the
argv `capture.sh` actually ran, printed beside the bytes that running it
produced — not prose about the command.

## The scenes, and what each documents

One scene per surface, each placed in the README section that explains it.
`scripts` is the wrong word for them: `screenshots/capture.sh` is the single
script, and each scene is a block within it.

- **`ask`** — the two-process rendezvous. `ask` prints `correlation: <c>` on
  stderr the moment the question is on the queue and then *waits*; the capture
  spawns it with piped stdio exactly as `tests/e2e/ask.rs` holds one open, reads
  that correlation, and answers from a second `reply` invocation over the same
  transport directory. The waiting state between the two lines is the thing the
  README's prose describes and cannot show.
- **`queues`** — `send`, `next`, then `status --format text`: the most structured
  human view the tool has, one counts line per queue with its consumers' cursors
  indented beneath it. `next` runs first so a cursor in that view has moved.
- **`events-merge`** — two committed NDJSON stream fixtures merged into one
  `(ts, stream, seq)` order and rendered one line per envelope, then the same
  merge narrowed by a `--filter`. The one-line envelope render and the filter
  grammar are the two things a stream reader needs to picture.
- **`schema`** — `schema list` over the linked bundle, then a `schema check`
  refusal naming the id and the JSON pointer, with the `1` a shell then reports.
  The listing alone would say little; the refusal is what the verb is for.
- **`serve`** — a `--codec` session: one frame in, the response document out,
  built from the frame's own fields.
- **`refusal`** — the documented one-line usage shape,
  ``onemessagebus: <verb>: … ; see `onemessagebus <verb> --help` ``, and the `2` the
  exit-code table gives refused input. The refusal shape is a contract, so it is
  worth a picture.

The **hero** is not one of these: `docs/screenshots/subscribe.gif` is an animated
`subscribe` tail (below), because watching a queue fill is what a bus looks like
in use and a still of the same text says strictly less.

### Two candidate scenes that are deliberately absent

A picture that says less than the prose it sits beside is padding, so it is not
taken:

- **`transports --format text`** renders two lines on a stock install (`local
  builtin` / `memory builtin`), which is exactly what the README sentence beside
  it says. Showing a plugin row instead would mean putting a built plugin on
  `PATH` and photographing this machine's absolute path to it — or normalising
  that path to something no run ever printed.
- **`onemessagebus --help`** is not colourised: this build links no colour crate
  and clap's own styling is off through a pipe *and* through a pty, and its
  longest line is 219 columns. The shot would be a wrapped monochrome wall of the
  same words the README's sections already carry. (The plan that commissioned
  this adoption permits a `--help` shot in a README body but never as the hero;
  here it earns neither place.)

## The fixture: free, offline, and the one the journeys use

`capture.sh` stages what `desk_config` stages for the end-to-end journeys — a
temporary directory, a `bus.yaml` naming a `local` transport in it, and the bus's
own `desk` layout bundle (`crates/onemessagebus-e2e/tests/layouts/desk.json`)
linked at `@1` — plus one codec whose frame bundle is reached by a `file://…@1`
link, so even the link machinery runs with no HTTP. The transport is a directory
of files. **No model, no network, no credential, no cost.** The layout document
is the journeys' own rather than a copy, so a change to it moves both.

`screenshots/fixture/` holds only what the journeys do not already provide: two
fixed NDJSON streams for the merge, the malformed record `schema check` refuses,
and the codec's frame bundle.

## Why it is byte-reproducible, and what it is pinned to

screencomp gates on the **hash** of each image, so two captures of one build must
produce identical bytes. Unlike a rasterised PNG (whose anti-aliasing drifts
across CPUs — which is why a web app captures inside a pinned browser container),
an SVG is pure layout maths. Three inputs are pinned and one class of value is
normalised:

- **`freeze` is version-pinned in exactly one place**: `freeze_version` in
  `screenshots/install-freeze.sh`. Both `just screenshots-tools` and the
  Visual-docs workflow's capture step run that script, so there is no second copy
  to keep in step, and the archive it downloads is checked against a digest
  pinned in `screenshots/freeze.sha256` rather than one served beside the
  download.
- **The font is vendored** (`fonts/JetBrainsMono-Regular.ttf`, OFL — see
  `fonts/JetBrainsMono-OFL.txt`) and passed with `--font.file`, so `freeze` never
  fetches one, and it is embedded into each SVG as base64 so the file renders the
  same on GitHub and crates.io with nothing external to load. The file itself is
  the one statement of which font this is.
- **The environment is cleared**: `capture.sh` unsets every ambient
  `ONEMESSAGEBUS_*` variable before it captures, as `tests/e2e/support.rs` does
  for the journeys. `ONEMESSAGEBUS_CONFIG`, `ONEMESSAGEBUS_TRANSPORT_DIR` and
  `ONEMESSAGEBUS_REGISTRY` are read ahead of this capture's own flags, so an
  exported one would steer a scene away from the fixture and drift its hash
  against a baseline captured in a clean shell.
- **Per-run values are normalised**, because this CLI has **no clock-override and
  no fixed-id switch and this adoption did not add one**: a flag here is a
  capability-manifest change first (a test walks the clap tree refusing a flag
  with no binding), then a method in both SDK clients and a parity audit. So
  `capture.sh` rewrites the 32-hex correlation `ask` mints and the
  epoch-millisecond instants the desk layout stamps to fixed placeholders, after
  the verb has run and before the scene is rendered. Everything else in every
  scene — positions, counts, ids, orderings — is already a function of the
  fixture's fixed content.

The Visual-docs workflow's two further pins — the Rust container it builds in and
the `screencomp-version:` it installs — are held to their sources by
`crates/onemessagebus-repo/tests/visual_docs_pins.rs`: the container and
`RUSTUP_TOOLCHAIN` to `rust-toolchain.toml`'s channel, and the
`screencomp-version:` input to the reusable workflow's `uses:` ref, which is the
one `screencomp doctor --env` reconciles against the installed CLI.

The result: identical bytes on every machine and runner. A shot changes only when
the tool's output or its formatting changes, which is exactly what the gate is
for.

## Lanes: one per arch, each with its own baseline

screencomp scopes captures per CPU arch (a *lane*). `[capture].arches` in
`screencomp.toml` is the single place the set is declared; `screenshots/host-arch.sh`
is the single place a lane name is derived from `uname -m`, shared by the capture,
the guard and `just screenshots-bless`.

This repository declares **one lane, `x86_64`**. The SVGs' bytes do not depend on
the CPU, so a second lane would be safe — but its committed baseline would be one
nothing here has ever captured, and the pre-push guard is local: it classifies the
lane of the host it runs on and refuses an undeclared arch loudly rather than
guessing. To gain a lane, add the arch to `[capture].arches`, run `just
screenshots-bless` on such a host, and commit `shots/baseline/<arch>.json`; CI
fans a job out per lane on its own.

## The animated hero (`docs/screenshots/subscribe.gif`)

The stills are static; the README hero is an animated GIF of a `subscribe` tail
filling. `screenshots/subscribe-gif.py` spawns the **real** `subscribe` over the
same `desk` fixture, reads its stdout line by line **with the instant each line
landed**, drives a sibling `send` process alongside it, and replays those lines at
their real inter-arrival gaps — so the backlog lands in a burst and then records
arrive one at a time, which is what the tail really does. Nothing redraws, so
there are no frames to reconstruct; Pillow draws the growing transcript with the
same vendored font, and nothing else is needed (no `ttyd`, no `ffmpeg`).

Unlike the SVGs it is **not** hash-gated — a GIF is not byte-reproducible across
rendering libraries — so it is regenerated on demand (`just screenshots-gif`) and
committed. Regenerate it when `subscribe`'s rendering changes.

## Outputs

- `shots/current/<arch>/captures.json` and the SVGs — the capture screencomp
  reads (gitignored; regenerated). `$SHOTS_OUT` overrides the directory; the
  reusable workflow exports it per lane.
- `shots/baseline/<arch>.json` — the committed digest baseline (no images).
- `docs/screenshots/` — the committed copies the README embeds, and the GIF.

## Commands

- `just screenshots-tools` — install the pinned `freeze` into `~/.local/bin`.
  screencomp itself is installed separately (see its README); CI installs both.
- `just screenshots` — capture. Builds the release binary, writes the shots and
  the README copies. Quiet on success.
- `just screenshots-gif` — regenerate the animated hero. Pillow comes from `uv`,
  so nothing has to be installed first.
- `just screenshots-bless` — after an **intended** output change, recapture and
  refresh **this host's** lane baseline. Commit `shots/baseline/` alongside
  `docs/screenshots/`.
- `screencomp doctor --env` — says whether the setup is actually wired: the guard
  active, the workflow's pin matching the installed CLI.

## The strict gate, and how its local half is activated

CI runs screencomp's reusable workflow with `fail-on-drift: true`: a capture that
diverges from the committed baseline fails.

Locally, `.githooks/pre-push` re-captures **only** when a `[guard].paths` file
changes (`screencomp.toml`), and on drift it regenerates this host's lane
baseline, builds a review gallery at `shots/review/index.html`, and **blocks the
push** so the refreshed baseline and README images are committed deliberately.

A committed hook that nothing activates runs nothing, so `just bootstrap` sets
`core.hooksPath` to `.githooks` — that is the whole body of the
`onemessagebus-visual-docs` project's `bootstrap` target, and running it in a
clean clone leaves the guard active. **That directory carries this hook and
nothing else**: `just gate`, this repository's complete pre-push bar, is
deliberately not wired into it — what the gate is and when you run it did not
change. Bypass a single push with `git push --no-verify`.

## Changing the screenshots

Editing the CLI surface or its renderings (`crates/onemessagebus-cli/src/`), the
core they answer from, the `desk` layout the fixture links, or the scenes in
`capture.sh` will change the SVGs. That is expected — run `just
screenshots-bless` and commit the new baseline with `docs/screenshots/`. Bumping
`freeze_version` or the vendored font reflows every shot; bless once.
