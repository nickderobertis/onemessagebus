# npm distribution

`onemessagebus-cli` on npm is a **launcher** that carries no binary: the prebuilt
binary ships in a per-platform package (`onemessagebus-cli-<platform>-<arch>`)
that npm selects by `os`/`cpu`, and `bin/onemessagebus.js` resolves it and execs
it with the caller's argv.

Five places name that platform matrix and must move together:

1. `bin/onemessagebus.js`'s `PACKAGES` map,
2. the `optionalDependencies` of the launcher `scripts/npm-build.mjs`
   assembles — generated from `TARGETS` and never committed, because pins in
   the committed `package.json` would name packages `npm ci` cannot resolve
   before a release, and the launcher is a member of the root npm workspace,
3. `scripts/npm-build.mjs`'s `TARGETS` table,
4. the `upload`, `build-wheels` and `build-npm` matrices in
   `.github/workflows/release.yml`,
5. `rust-toolchain.toml`'s `targets`, the standard libraries rustup installs
   so every one of those triples builds from the pinned toolchain.

`test/platform-matrix.test.mjs` holds the five to each other.

The committed `package.json` carries `0.0.0-managed`, not a real version. The
version has exactly one source — `Cargo.toml`'s `[workspace.package]`, written
by release-plz — and `scripts/npm-build.mjs` stamps it into the launcher and
every platform pin at publish time. Never hand-edit a version here.

The journeys that assemble the packages around the built binary, install them
and run what npm put on PATH live in `e2e/`, their own `onemessagebus-npm-e2e`
project: that tier builds and installs, and `test/` stays the fast checks over
the manifests and the scripts.

`test/` also carries the checks that are about the release rather than about
npm — the release-target declaration's drift against what a release really
publishes, and the release probe's not-answered answer — because this is the
packaging project and they read the same `release.yml` the matrix gate does.
Whether that declaration is a *shape* its schema allows is not asked here:
`crates/onemessagebus-repo/tests/release_declaration.rs` hands it to `onevcs`'s
own reader, and `test/support/declaration.mjs` merely parses the document the
way any consumer with a TOML parser does.

Nothing in this directory is published from a developer's machine:
`.github/workflows/release.yml` assembles, packs, and publishes it, and
`scripts/publish-npm.sh` makes that publish idempotent.
