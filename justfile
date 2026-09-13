# Canonical command surface for onemessagebus.
#
# `just bootstrap` works from a clean clone; `just check` is the deterministic
# quality gate and `just gate` is the complete pre-push bar (`check` plus the
# diff-scoped llmlint tier). Recipes are quiet on success and specific on failure.
#
# The repo-wide verbs delegate to Nx, which fans the uniformly-named target out
# across every project rather than looping over projects by hand. What a target
# *does* stays with its project — the `_crate-*` recipes below are one crate's
# own tools, invoked by that crate's project.json with its name.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# llmlint: ignore-file[tool_output_is_signal] recipes that hand straight to cargo,
# clippy, rustdoc, or cargo-deny inherit those tools' diagnostics, which already name
# the exact problem and its fix; a wrapper message would bury them. The recipes whose
# failure needs project-level context (_crate-bootstrap, _crate-test, _e2e-test,
# _crate-fmt-check, _coverage, msrv, the lint-llm tier) add one explicitly.

# The MSRV has one source of truth — Cargo.toml's `rust-version` — so `just msrv`
# cannot promise a floor the manifest no longer declares. CI reads the same field.
msrv-version := `sed -n 's/^rust-version *= *"\([^"]*\)".*/\1/p' Cargo.toml`

# Keep the gate's own output to signal: successes are silent, failures are not.
export CARGO_TERM_QUIET := "true"

# A compiler warning is an error in every build the gate makes, tests and the
# journeys' binary included. Cargo caps lints in registry dependencies, so
# this reaches the workspace's own crates and nothing else.
export RUSTFLAGS := "-D warnings"

# Where cargo-llvm-cov keeps the instrumented build and every project's raw
# profiles; the aggregate report reads them all. Cleared before a test sweep so
# a profile from a binary that no longer exists is never merged in.
profraw-root := "target/llvm-cov-target"

# List available recipes.
default:
    @just --list

# Every project's `bootstrap` target, so one clean-clone command provisions the
# whole graph. Serialized: the targets share installers, and two of them running
# at once race the same directory.
# Set up the project from a clean clone.
bootstrap:
    @bash scripts/nx run-many -t bootstrap --parallel=1

# The Rust workspace's provisioning (every crate's `bootstrap` target; one
# resolve covers them all).
_crate-bootstrap:
    @rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install
    @rustup component add rustfmt clippy llvm-tools >/dev/null \
      || { echo "cannot add toolchain components — install rustup (https://rustup.rs/) and re-run" >&2; exit 1; }
    @just _ensure-tool cargo-nextest
    @just _ensure-tool cargo-llvm-cov
    @cargo fetch --locked --quiet

# These are test runners, not rules: their version cannot change the gate's
# verdict, so both here and CI take the latest rather than keeping two pins that
# drift apart.
# Install a cargo dev tool if it is missing. Quiet when already present.
_ensure-tool tool:
    @command -v {{tool}} >/dev/null 2>&1 || cargo install {{tool}} --locked --quiet

# The tiers run in fail-fast order as dependencies, each fanned across every
# project by Nx, then the aggregate coverage floor over every project's run.
# Deterministic quality gate, every project.
check: fmt-check lint test doc coverage
    @echo "check: ok"

# The complete pre-push bar: the deterministic gate plus the LLM-judge tier scoped
# to this branch's diff. `check` stays offline and credential-free; this is the one
# that needs a harness.
# Full pre-push gate: `check` plus the diff-scoped llmlint tier.
gate base="origin/main": check (lint-llm-diff base)
    @echo "gate: ok"

# What PR CI runs: the same tiers, scoped to the projects this branch's diff can
# reach. The coverage floor is over the union of every crate's run, so when the
# diff reaches a crate at all every test target runs and the floor is enforced;
# when it reaches none, only the affected non-Rust tests run. Fails closed —
# with no derivable merge base it runs everything.
# Deterministic quality gate, affected projects only.
check-affected:
    @bash scripts/nx-affected.sh -t format-check lint doc build
    @rm -f {{profraw-root}}/*.profraw
    @if [ "$(just affected-crate)" = "true" ]; then bash scripts/nx run workspace:coverage; \
      else bash scripts/nx-affected.sh -t test; fi
    @echo "check-affected: ok"

# `true` when this branch's diff can reach a Rust crate project, so CI can skip
# the cross-platform and install matrices on a change that cannot touch one.
# Fails closed.
# Whether the Rust crates are affected by this branch.
affected-crate:
    @bash scripts/nx-affected.sh --affects onemessagebus-cli

# Escape hatch for Nx itself, e.g. `just nx show projects` or `just nx graph`.
# Run an arbitrary Nx command against this workspace.
nx *ARGS:
    @bash scripts/nx {{ARGS}}

# Verify formatting without modifying files.
fmt-check:
    @bash scripts/nx run-many -t format-check

# Format the codebase in place.
format:
    @bash scripts/nx run-many -t format

# Lint every project with its own linter; any warning is an error.
lint:
    @bash scripts/nx run-many -t lint

# Every project's test suite, each writing its coverage profile for `coverage`.
test:
    @rm -f {{profraw-root}}/*.profraw
    @bash scripts/nx run-many -t test

# Build every project that has a build target (the binary).
build:
    @bash scripts/nx run-many -t build

# Build every project's docs; warnings are errors.
doc:
    @bash scripts/nx run-many -t doc

# 95% line coverage over the union of every project's run; lower it only with a
# documented reason in AGENTS.md.
# The aggregate coverage floor over every project's test run.
coverage:
    @bash scripts/nx run workspace:coverage

# Verify one crate's formatting without modifying files.
_crate-fmt-check crate:
    @cargo fmt -p {{crate}} -- --check || { echo "formatting drift above — run 'just format'" >&2; exit 1; }

# Format one crate in place.
_crate-format crate:
    @cargo fmt -p {{crate}}

# Lint one crate with clippy; any warning is an error.
_crate-lint crate:
    @cargo clippy -p {{crate}} --all-targets --locked --quiet -- -D warnings

# Build one crate's binary, which the journeys and the npm packaging spawn.
_crate-build crate:
    @cargo build -p {{crate}} --locked --quiet

# One crate's unit and integration tests, instrumented, with its raw profiles
# left for the aggregate report rather than reported on their own. nextest does
# not run doctests, and the crate READMEs are compiled as doctests so a sample
# naming a removed item fails here; they run uninstrumented beside it.
_crate-test crate:
    @cargo llvm-cov --no-report nextest -p {{crate}} --locked --status-level fail --final-status-level fail \
      || { echo "{{crate}}: tests failed — fix the failures named above" >&2; exit 1; }
    @cargo test --doc -p {{crate}} --locked --quiet \
      || { echo "{{crate}}: doctests failed — fix the sample named above, or the README it is compiled from" >&2; exit 1; }

# The compiled-binary journeys: the binary built instrumented in the coverage
# target directory, so what the journeys spawn is attributed to the crates it
# was built from, then the journey crate's tests over it.
_e2e-test:
    @cargo llvm-cov --no-report run -p onemessagebus-cli --bin onemessagebus --locked -- --version >/dev/null
    @cargo llvm-cov --no-report nextest -p onemessagebus-e2e --locked --status-level fail --final-status-level fail \
      || { echo "onemessagebus-e2e: journeys failed — fix the failures named above" >&2; exit 1; }

# Pinned to the maturin CI's `wheel` job builds with, so a wheel that builds here
# is the one that job would build.
_wheel-build:
    @rm -rf dist/wheels
    @RUSTFLAGS="-D warnings" uvx --from 'maturin==1.14.1' maturin build --release --locked --out dist/wheels \
      || { echo "onemessagebus-pypi: the wheel did not build — fix the error above (maturin reads pyproject.toml), or install uv (https://docs.astral.sh/uv/)" >&2; exit 1; }

# The built wheel installed into a fresh virtualenv the way `pip install
# onemessagebus-cli` installs it, then the published smoke script over what it put
# on PATH, holding it to the workspace version.
_wheel-test:
    #!/usr/bin/env bash
    set -euo pipefail
    venv="$(mktemp -d)"
    trap 'rm -rf "$venv"' EXIT
    uv venv --quiet "$venv" \
      || { echo "onemessagebus-pypi: cannot create a virtualenv — install uv (https://docs.astral.sh/uv/) and a Python 3.9+" >&2; exit 1; }
    VIRTUAL_ENV="$venv" uv pip install --quiet --no-index --find-links dist/wheels onemessagebus-cli \
      || { echo "onemessagebus-pypi: no installable wheel in dist/wheels — run 'just nx run onemessagebus-pypi:build' first" >&2; exit 1; }
    version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
    PATH="$venv/bin:$PATH" bash scripts/smoke-published.sh --expect-version "$version" --label "the wheel built from this revision"

# The aggregate report over every project's profiles, enforced once. The
# conformance table is test support published for profile crates, exercised by
# its callers rather than a subject of coverage.
_coverage:
    @cargo llvm-cov report --fail-under-lines 95 --ignore-filename-regex 'conformance\.rs' \
      || { echo "coverage fell below 95% — cover the lines the table above counts as missed" >&2; exit 1; }

# Drives the compiled binary as a subprocess — never an in-process `main()`.
# The end-to-end binary journeys in isolation (also run by `test`/`check`).
test-e2e:
    @bash scripts/nx run onemessagebus-e2e:test

# Coverage instrumentation is measured on Linux only, so the cross-platform CI
# legs run the same suites through this instead of `test`.
# Every project's tests, the npm install journeys included, without coverage instrumentation.
test-uninstrumented:
    @cargo build -p onemessagebus-cli --locked --quiet
    @cargo nextest run --workspace --locked --status-level fail --final-status-level fail
    @cargo test --doc --workspace --locked --quiet
    @node --test npm/test/*.test.mjs npm/e2e/*.test.mjs

# Build one crate's docs with warnings denied (kept in the gate so doc links don't rot).
_crate-doc crate:
    @RUSTDOCFLAGS="-D warnings" cargo doc -p {{crate}} --no-deps --locked --quiet

# Run the CLI, e.g. `just run schema list`.
run *ARGS:
    cargo run --locked --quiet -p onemessagebus-cli --bin onemessagebus -- {{ARGS}}

# Upgrade dependencies, then re-run the deterministic gate.
upgrade:
    @cargo update --quiet
    @npm update --silent --no-audit --no-fund
    @just check

# Separate from `check`: `cargo deny` needs a network-fetched advisory DB.
# Advisory + license audit, the sibling ban, and the unused-dependency check.
deps-check:
    @bash scripts/nx run workspace:deps-check

_deps-check:
    @command -v cargo-deny >/dev/null || { echo "cargo-deny not installed: cargo install cargo-deny --locked" >&2; exit 1; }
    @command -v cargo-machete >/dev/null || { echo "cargo-machete not installed: cargo install cargo-machete --locked" >&2; exit 1; }
    @cargo deny --log-level error check
    @# machete prints the unused deps it finds on stdout, so keep it: hiding them
    @# would leave a failing gate with no actionable detail.
    @cargo machete

# Reads the floor from Cargo.toml's `rust-version`; that toolchain must be
# installed (`rustup toolchain install <version>`). Warnings are errors here too.
# Build under the declared MSRV.
msrv:
    @cargo +{{msrv-version}} check --workspace --locked --all-targets --quiet \
      || { echo "the {{msrv-version}} floor no longer builds — install that toolchain, or raise rust-version in Cargo.toml (and clippy.toml)" >&2; exit 1; }

# Ensures `just`, verifies the rest, then runs setup-llmlint. Runs automatically
# via the Claude Code SessionStart hook; this is the manual entry point.
# Provision the dev toolchain for a session. Idempotent, no-ops in CI.
session-setup:
    ./scripts/session-setup.sh

# Install/refresh the llmlint toolchain (oneharness + llmlint). Idempotent.
setup-llmlint:
    ./scripts/setup-llmlint.sh

# Kept OUT of `check` on purpose: the deterministic gate stays offline and
# credential-free. Config is the composed `llmlint.yml`.
# LLM-judge lint — the non-deterministic, harness-backed tier.
lint-llm *paths:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint {{paths}}

# CI runs this before the model tier so a broken config fails in milliseconds
# instead of spending a harness call.
# Fast, deterministic llmlint gate — no model calls, no harness credential.
lint-llm-validate *args:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint validate {{args}}

# The blocking `llmlint` PR check; `just gate` runs it before you push.
# llmlint scoped to the files this branch changed since it forked from main.
lint-llm-diff base="origin/main" *args:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint --diff --diff-base "{{base}}" {{args}}
