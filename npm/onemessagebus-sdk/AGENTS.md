# @onemessagebus/sdk (the TypeScript SDK)

A typed client over the `onemessagebus` binary. What touches the wire is
generated, and the hand-written code is the
transports, the typed errors, `defineMessage`, the async iterator and the version
check.

- **`src/generated/` is the generator's.** `scripts/generate.mjs` renders the
  declarations (`json-schema-to-typescript`) and the Zod schemas from the
  sdk_bundle example; `just sdk-generate` rewrites them, and this project's
  `lint` fails on a stale copy. A construct the Zod generator does not cover
  fails generation by name.
- **Casts are typed away, not asserted.** Outputs are parsed by their generated
  schema, caught errors narrowed with `instanceof`; the one cast left is the
  generated runtime's `contract<T>`, sound because a schema and its type come
  from one document.
- **Versions are stamped, never written.** `SDK_VERSION`, `CLI_VERSION` and the
  manifest's version stay at `0.0.0-dev`, and the manifest names no
  `onemessagebus-cli`; `scripts/pack.mjs` stamps the workspace version and adds
  the exact CLI dependency in a publishable copy, which
  `onemessagebus-sdk-install-e2e` installs and drives.
- **One npm workspace.** The package resolves from the root `package-lock.json`
  (`npm install` at the root after changing its dependencies); bun runs its
  scripts and tests, and `nx.includedScripts` is empty so its `package.json`
  scripts add no targets beside `project.json`'s.
- **One method per capability, and no other public method on `Client`.** The
  parity gate reads `export class Client`'s members at two spaces: `client.schema`
  is a field, private helpers are `#private`, and disposal is
  `[Symbol.asyncDispose]`.
- **Tests drive the built binary** (`cargo build -p onemessagebus-cli`) over both
  transports under bun; the package is built with `tsc` and must run under node.
