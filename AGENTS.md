# AGENTS.md

Durable instructions for humans and agents working in this repo. Write for a
future maintainer, not as a session log. Deterministic steps live in `scripts/`
and the `justfile`; this file holds the judgment.

> `CLAUDE.md` is a symlink to this file — edit `AGENTS.md` only.

## What this repo is

`onemessagebus` is a typed NDJSON message bus: one envelope, one filter grammar,
payload bounds and redaction, a schema registry with version read-sets, and an
emitter and reader over streams — generic over the **vocabulary** a consumer
declares. It ships as three things: the `onemessagebus` crate (the core, which
knows nothing about agents), the `onemessagebus-agent` crate (the agent stack's
vocabulary declared over the core's public API, which `oneagentgraph`, `onevcs`
and `onepipeline` adopt in place of the envelope each copied), and the
`onemessagebus` binary (crates.io-free: the `onemessagebus-cli` wheel on PyPI,
the `onemessagebus-cli` launcher on npm, or `cargo install --git`). The Python
and TypeScript SDK packages are a later node's.

[`docs/contract.md`](docs/contract.md) is the approved contract, the one source
every consumer restates from; [`docs/wire.md`](docs/wire.md) and
[`docs/cli.md`](docs/cli.md) say the same things in this repository's own voice.

## The contract comes first, and it is not negotiable in passing

Every fenced block in `docs/contract.md` is driven by a contract test in both
library crates, and the recorded streams under
`crates/onemessagebus-agent/tests/recorded/` hold the profile's types to the
bytes each producer writes today. Two rules follow:

- **A shared interface is never changed unilaterally.** A field that turns out
  to be needed or a shape that turns out to be wrong is a proposal to the
  contract's owner, while the work continues against the agreed surface.
- **The core has no agent word in it.** Every reserved dimension, label key and
  source word of the agent vocabulary is declared in `onemessagebus-agent`; the
  core's own test drives a vocabulary with no agent key through the same
  conformance table. The profile depends on the core and never the reverse, and
  `deny.toml` refuses an edge from either published crate to a sibling of the
  stack.

## Two standing goals on every task

The user drives product features and their request is the priority — but carry
two goals into *every* task. When either is the lowest-error path to what the
user asked, fold it into the same task; otherwise surface it as a follow-up.

1. **Engineer the context for next time.** Realistic end-to-end tests for what
   the user sees (especially a bug the suite missed), scripts that automate
   repeated steps and shrink their output to signal, and terse notes here for
   what the code doesn't show.
2. **Engineer the codebase and environment.** Keep `just bootstrap` working from
   a clean clone and local/CI parity exact (same recipes, same pinned
   toolchain), so results are repeatable rather than "works on my machine."

## Stack and composition

<!-- llmlint: ignore-block[agents_md_durable_and_terse] this section is a required
artifact, not free-form prose: create-repo's check_repo_baseline.py verifies it is
present and filled in, and it is the one record of why the tooling is what it is —
which a future task cannot recover from the tree. Kept to the decision and its
rationale; the mechanics live in the files named. -->

- **Product shape:** cli, with the library guidance folded in. The composer
  (`compose_repo_plan.py --shape library --shape cli ...`) takes one shape and
  composed `cli`; `shapes/library.md` was read and applied by hand — a stable
  public surface held by contract tests, semver through release-plz, boundary
  validation, consumer docs — and its llmlint fragment is pinned in
  `llmlint.yml` beside the composed ones.
- **Language(s):** rust, plus bash for the wrappers and Node for the npm
  assembler and its tests, as the siblings. Python and TypeScript are the SDK
  node's (below); today the Python surface is the maturin wheel
  (`pyproject.toml`, no Python source) and the JavaScript surface is the npm
  launcher and the packaging tests under `npm/`.
- **References composed:** `base.md`, `project-graph.md`, `shapes/cli.md`,
  `shapes/library.md`, `languages/rust.md`, `intersections/rust-cli.md`,
  `ci.md`, `llmlint.md`, `releasing.md`.
- **What the composer printed, and what was deferred:** the composer was run
  as `compose_repo_plan.py --shape library --shape cli --language rust
  --language python --language typescript --intersection rust-cli --releasing`
  and printed `base.md, project-graph.md, shapes/cli.md, languages/rust.md,
  languages/python.md, languages/typescript.md, intersections/rust-cli.md,
  intersections/python-cli.md, ci.md, llmlint.md, releasing.md` (it takes one
  shape; `shapes/library.md` was applied by hand, above). `languages/python.md`,
  `languages/typescript.md` and `intersections/python-cli.md` are **deferred to
  the SDK node** (`bus-sdks-resident`), which adds the Python and TypeScript
  source they govern: their gates (a uv workspace with `ruff`/`ty`/`pytest`,
  bun with `tsc`) and their structural invariants judge source this tree does
  not have, and recording them as composed here would have the buildout tier
  fail over Python and TypeScript tooling for a tree with none. Their
  ongoing llmlint fragments stay pinned in `llmlint.yml` so the rules are in
  force the day that source lands.
- **Projects in the graph:** `onemessagebus` (the core; `type:contract`),
  `onemessagebus-agent` (the profile), `onemessagebus-cli` (the binary,
  unpublished), `onemessagebus-e2e` (the compiled-binary journeys, whose `test`
  depends on the binary's `build`), `onemessagebus-npm` (the launcher and the
  release-configuration drift gates), and the root `workspace` project carrying
  the aggregate coverage floor and the supply-chain check. The binary is its
  own `publish = false` crate because it links the agent profile so `--profile`
  defaults to it, a binary links only its own crate's dependencies, and the
  profile depends on the core — so it can live in neither library; the manager
  ruled this over the ask seam.
- **Excluded, and why:** **asdf / direnv** — `rust-toolchain.toml` and the
  committed lockfiles already pin everything. **A curl-pipe installer and a
  composite action** — every documented install surface is a registry or
  `cargo install --git`, so nothing constructs a release asset's name and no
  asset-naming contract can drift. **bun** — the Node use is the Nx
  orchestrator and the launcher's own `node --test` suite, which the siblings
  run with npm and a committed `package-lock.json`; bun arrives with the
  TypeScript SDK if that node wants it. **A `published-smoke` workflow** — the
  post-release registry watch the siblings carry is a follow-up once the first
  release exists to watch.
<!-- llmlint: ignore-end[agents_md_durable_and_terse] the required-artifact scope ends here.
-->

## Command surface

`just --list` is the index; do not hand-roll equivalents. What it does not tell
you:

- **`just gate` is the bar, not `just check`.** `check` is the deterministic
  tier and stays offline and credential-free; `gate` adds the diff-scoped
  llmlint tier, and that is what must be green before pushing.
- **The repo-wide verbs delegate to Nx** (`scripts/nx`), which fans the
  uniform target names across the graph. A target's *body* belongs to its
  project, never to a for-each loop here: the `_crate-*` recipes take the crate
  name their project.json passes.
- **Coverage is one aggregate.** Each `test` target runs its crate under
  `cargo llvm-cov --no-report`; the journey target first builds the binary
  instrumented, so what the journeys spawn is attributed to the crates it was
  built from; and `workspace:coverage` reports the union and fails below 95%.
  `test` targets are uncached on purpose — a replayed profile would be
  attributed to binaries that no longer exist — and `check-affected` runs the
  whole test set whenever the diff reaches a crate, because the floor is over
  the union.
- **Affected selection fails closed** (`scripts/nx-affected.sh`): with no
  derivable merge base it runs everything, because a speed optimisation that
  can silently skip a check is a correctness hole.

## Commits, releases, and merging

- **Squash-merge only, via PR, with auto-merge.** `main` is protected: merge
  commits and rebase-merging are off, so one PR is one squash commit whose
  subject is the PR title. Queue with `gh pr merge --auto --squash`; head
  branches auto-delete. Admins may bypass in a break-glass.
- **All gating checks are required**, by the context each reports: `gate`,
  `changes`, `cross (macos-latest)`, `cross (windows-latest)`, `msrv`, `deny`,
  `install (ubuntu-latest)`, `install (macos-latest)`, `install (windows-latest)`,
  `wheel`, `pr-title`, and `llmlint`. A matrix job reports one context per
  platform, and `changes` is required because the jobs it gates are skipped —
  which counts as passing — when it fails. `install-documented.yml` runs
  on a push to `main`, never on a pull request, so it cannot be required. `notignored` is deliberately
  not: it is the review artifact naming the suppressions a PR adds, and a fork's
  read-only token cannot post it.
- **The broader tier runs on the release PR.** release-plz batches merges
  behind a release PR, so the commit that ships is that PR's, and it is the
  one CI sweeps whole (`just check` over every project); a nightly run sweeps
  the same way. An ordinary pull request runs the affected tier
  (`just check-affected`, scoped against its fork point), and a push to `main`
  is gated by nothing further — the pull request that landed it was. The
  tag-triggered `release.yml` builds and publishes the swept commit and re-runs
  no gate.
- **PRs follow `.github/pull_request_template.md`** — terse **What** and **Why**;
  it becomes the squash body. `.github/CODEOWNERS` routes the review by subtree.
- **Releases are fully automated; the only human action is merging a PR.**
  release-plz is the single version driver: it computes the version from
  conventional commits, opens a release PR, and on merge tags `vX.Y.Z` and cuts
  the GitHub Release, which triggers `release.yml` to build and publish. Nobody
  hand-edits a version, hand-tags, or hand-dispatches a publish. Pre-1.0 bump
  policy: `feat` → minor, `fix`/`perf`/`refactor`/`build` → patch, `!` or
  `BREAKING CHANGE` → minor; `chore`/`docs`/`ci`/`test`/`style` do not release.
  At 1.0 the usual semver regime takes over (`!` → major). `semver_check` is on,
  so a surface break bumps whatever the type said.
- **One version source.** `Cargo.toml`'s `[workspace.package]` is it, inherited
  by every crate: both published crates move on one release, the wheel takes it
  via maturin's `dynamic = ["version"]`, and the npm packages via
  `scripts/npm-build.mjs`. Never write a version into `pyproject.toml` or
  `npm/onemessagebus-cli/package.json`.
- **What this repository publishes is declared in `release-targets.toml`, and
  answered by `scripts/release-probe.sh`.** Four targets — the two crates, the
  wheel, the npm launcher (covering its five platform packages) — at schema
  version 3. `crates/onemessagebus-e2e/tests/release_declaration.rs` holds the
  document to the schema through `onevcs`'s own reader, and
  `npm/test/release-targets.test.mjs` holds it to the release configuration in
  both directions, so a new artifact fails the gate rather than going
  undeclared. The unpublished CLI crate is not a target: nothing depends on it
  by name.

`gh-secrets.json` names the secrets a fork or a fresh clone must provision;
values live in the secret store, never in the tree.

## Invariants (non-negotiable)

- The gate is strict. No warnings-only mode: a diagnostic is an error, or it is
  suppressed at its site with a written reason.
- **Coverage is enforced at 95% line coverage** over the union of every
  crate's run. Lower it only with a documented reason here. The one exclusion
  is `conformance.rs`: test support published for profile crates, exercised by
  its callers rather than a subject of coverage.
- **Tests are realistic, not mocked**, and complete rather than minimal: drive
  the real binary the way a user does, over every verb, happy path *and*
  failure. Coverage is the floor, never the target.
- Validate external input at its trust boundary, and reject it with a message
  naming the problem: a schema id, a filter, a payload, a label, a source word.
- Security is gate-level: no secrets in the tree, least-privilege CI tokens, a
  narrow agent allowlist in `.claude/settings.json`, and redaction before an
  envelope leaves the emitter.
- **Exit codes are a contract**: `0` did it, `1` a well-formed no, `2` refused
  input.

## Scripts and output are context

Quiet on success — a line or nothing. On failure, print the exact error and a
concrete next action. `scripts/nx` preserves each run's full output at
`.logs/<label>.log` (gitignored, owner-only, credential values redacted) so a
green run owes one line and a red one still has everything.

## Keeping the allowlist current

`.claude/settings.json` holds the agent command allowlist and the tool enforces
it. Keep it current: when a command becomes part of the routine workflow, add it
there instead of re-approving it every session. Keep it narrow.
