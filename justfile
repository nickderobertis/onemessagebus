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
check: fmt-check lint typecheck test doc coverage
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
# when it reaches none, only the affected non-Rust tests run. The SDK install
# journey is never among them: it resolves the SDKs' third-party dependencies from
# the public registries, so pull requests run it from CI's own `sdk-install` job
# and `check` sweeps it with everything else. The cross-language journey is not part
# of the coverage union either: when the diff reaches a crate it runs after the floor,
# and it is affected whenever a crate is, since it drives the CLI. (`nx affected` has
# no `--projects` filter; it hands an unknown flag to every target's command.) Fails
# closed — with no derivable merge base it runs everything.
# Deterministic quality gate, affected projects only.
check-affected:
    @bash scripts/nx-affected.sh -t format-check lint typecheck doc build
    @rm -f {{profraw-root}}/*.profraw
    @if [ "$(just affected-crate)" = "true" ]; then bash scripts/nx run workspace:coverage \
        && bash scripts/nx run onemessagebus-cross-language-e2e:test; \
      else bash scripts/nx-affected.sh -t test --exclude=onemessagebus-sdk-install-e2e; fi
    @echo "check-affected: ok"

# `true` when this branch's diff can reach a Rust crate project, so CI can skip
# the cross-platform and install matrices on a change that cannot touch one.
# Fails closed.
# Whether the Rust crates are affected by this branch.
affected-crate:
    @bash scripts/nx-affected.sh --affects onemessagebus-cli

# `true` when this branch's diff can reach either SDK package — the binary they
# drive included — so CI can skip their install journey on a change that cannot
# touch one. Fails closed.
# Whether the SDK packages are affected by this branch.
affected-sdk:
    @bash scripts/nx-affected.sh --affects onemessagebus-sdk-install-e2e

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

# Every project with a type checker of its own beside its compiler: ty and tsc.
# Type-check every project that has one.
typecheck:
    @bash scripts/nx run-many -t typecheck

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
    @cargo llvm-cov --no-report nextest -p onemessagebus-e2e --locked -E 'not binary(cross_language)' --status-level fail --final-status-level fail \
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

# Both SDKs packed at this revision's version and installed the way a user
# installs them, beside the binary built from it: the Python SDK with the wheel
# `onemessagebus-pypi:build` left in dist/wheels, the Node SDK with the launcher
# and host platform package scripts/npm-build.mjs assembles around a release
# build. Then each smoke program under sdk-install/ drives what was installed.
_sdk-install-test:
    #!/usr/bin/env bash
    set -euo pipefail
    root="$PWD"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    fail() { echo "onemessagebus-sdk-install-e2e: $1" >&2; exit 1; }
    version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
    mkdir -p "$work/wheels" "$work/npm" "$work/tarballs" "$work/app"
    cp dist/wheels/*.whl "$work/wheels/" 2>/dev/null \
      || fail "no binary wheel in dist/wheels — run 'just nx run onemessagebus-pypi:build' first"
    just python-sdk-dist "$work/wheels" || fail "the Python SDK did not build — its output is above"
    uv venv --quiet "$work/venv" || fail "cannot create a virtualenv — install uv (https://docs.astral.sh/uv/)"
    # The binary's wheel by path first, so the index's release of the same version
    # cannot stand in for this revision's; then the SDK, which pins it exactly.
    VIRTUAL_ENV="$work/venv" uv pip install --quiet "$work"/wheels/onemessagebus_cli-*.whl \
      || fail "the binary's wheel did not install into a fresh virtualenv"
    VIRTUAL_ENV="$work/venv" uv pip install --quiet --find-links "$work/wheels" "onemessagebus==$version" \
      || fail "the Python SDK $version did not install beside onemessagebus-cli $version"
    (cd "$work" && PATH="$work/venv/bin:$PATH" "$work/venv/bin/python" "$root/sdk-install/smoke.py" "$version") \
      || fail "the installed Python SDK failed its smoke run — its output is above"
    cargo build --release --locked --quiet -p onemessagebus-cli
    target="$(rustc -vV | sed -n 's/^host: //p')"
    platform="$(node scripts/npm-build.mjs platform --target "$target" --binary target/release/onemessagebus --out "$work/npm")"
    launcher="$(node scripts/npm-build.mjs launcher --out "$work/npm")"
    just node-sdk-dist "$work/tarballs" || fail "the Node SDK did not build — its output is above"
    bun run --cwd npm/onemessagebus-sdk test:package \
      || fail "the Node SDK's packed tarball did not install and run — its output is above"
    for dir in "$platform" "$launcher"; do
      (cd "$dir" && npm pack --silent --pack-destination "$work/tarballs" >/dev/null) || fail "cannot pack $dir"
    done
    (cd "$work/app" && npm init -y >/dev/null && npm install --silent --no-audit --no-fund "$work"/tarballs/*.tgz) \
      || fail "the Node SDK $version did not install beside onemessagebus-cli $version"
    # Run from inside the app: a module resolves a bare package name from where the
    # module file sits, and only the app has the SDK installed.
    cp "$root/sdk-install/smoke.mjs" "$work/app/smoke.mjs"
    (cd "$work/app" && node smoke.mjs "$version") \
      || fail "the installed Node SDK failed its smoke run — its output is above"

# The cross-language journey, apart from the Rust journeys because it also runs
# uv, Python, bun and both SDKs: the binary built instrumented, then only
# crates/onemessagebus-e2e/tests/cross_language.rs over it.
_cross-language-test:
    @cargo llvm-cov --no-report run -p onemessagebus-cli --bin onemessagebus --locked -- --version >/dev/null
    @cargo llvm-cov --no-report nextest -p onemessagebus-e2e --locked --test cross_language --status-level fail --final-status-level fail \
      || { echo "onemessagebus-cross-language-e2e: the journey failed — fix the failures named above (it needs uv and bun on PATH, and both SDKs bootstrapped)" >&2; exit 1; }

# The aggregate report over every project's profiles, enforced once. The
# conformance table is test support published for vocabulary crates, exercised by
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
    @bash scripts/nx run onemessagebus-node-sdk:build
    @cargo nextest run --workspace --locked --status-level fail --final-status-level fail
    @cargo test --doc --workspace --locked --quiet
    @node --test npm/test/*.test.mjs npm/e2e/*.test.mjs

# The language SDKs are packages of their own, each with its Nx project; these
# are the verbs a person reaches for by name. What each tier runs is the
# package's project.json's to say.
# Regenerate the TypeScript SDK's generated contract from the Rust bundle.
node-sdk-generate:
    @bun run --cwd npm/onemessagebus-sdk generate

# The TypeScript SDK's tiers: generate-check, format, lint, typecheck, tests, build.
node-sdk-check:
    @bash scripts/nx run onemessagebus-node-sdk:check

# Regenerate the Python SDK's generated models from the Rust bundle.
python-sdk-generate:
    @bash python/onemessagebus-sdk/scripts/run python scripts/generate.py

# The Python SDK's tiers: generate-check, format, ruff, ty, tests with the coverage floor, build.
python-sdk-check:
    @bash scripts/nx run onemessagebus-python-sdk:check

# Re-resolve uv.lock, the Python workspace's lockfile the SDK's environment syncs from.
python-sdk-lock:
    @uv lock --quiet

# The publishable copy is stamped by scripts/pack.py from Cargo.toml's version;
# release.yml and the SDK install journey both build it through here.
# Build the Python SDK's publishable sdist and wheel, at the workspace version, into OUT.
python-sdk-dist out:
    #!/usr/bin/env bash
    set -euo pipefail
    fail() { echo "python-sdk-dist: $1" >&2; exit 1; }
    pack="$(uv run --no-project python python/onemessagebus-sdk/scripts/pack.py | tail -n1)" \
      || fail "the Python SDK did not pack — run 'uv run --no-project python python/onemessagebus-sdk/scripts/pack.py' to see why"
    uv build --quiet --out-dir "{{out}}" "$pack" \
      || fail "the packed Python SDK at $pack did not build — the uv output above says why"

# The publishable copy is stamped by scripts/pack.mjs from Cargo.toml's version;
# release.yml and the SDK install journey both build it through here.
# Build the Node SDK's publishable npm tarball, at the workspace version, into OUT.
node-sdk-dist out:
    #!/usr/bin/env bash
    set -euo pipefail
    fail() { echo "node-sdk-dist: $1" >&2; exit 1; }
    [ -e node_modules/.bin/tsc ] || npm ci --silent --no-audit --no-fund \
      || fail "the npm workspace did not install — run 'npm ci' to see why"
    bun run --cwd npm/onemessagebus-sdk build >/dev/null || fail "the Node SDK did not build — run 'just node-sdk-check'"
    sdk="$(node npm/onemessagebus-sdk/scripts/pack.mjs | tail -n1)" \
      || fail "the Node SDK did not pack — run 'node npm/onemessagebus-sdk/scripts/pack.mjs' to see why"
    mkdir -p "{{out}}"
    out="$(cd "{{out}}" && pwd)"
    (cd "$sdk" && npm pack --silent --pack-destination "$out" >/dev/null) || fail "cannot pack $sdk into $out"

# Every capability, one method in each SDK client, and no method beside them.
sdk-coverage:
    @node parity/sdk-coverage.mjs

# Regenerate docs/sdk-parity.md from the capability manifest and the SDK clients.
parity-audit:
    @node parity/parity-audit.mjs
    @echo "parity-audit: docs/sdk-parity.md regenerated"

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
