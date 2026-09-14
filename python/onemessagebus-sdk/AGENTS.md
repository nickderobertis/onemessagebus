# onemessagebus (the Python SDK)

A typed async client over the `onemessagebus` binary. What touches the wire is
generated, and the hand-written code is the transports, the typed errors, the
`Message` base, the async iterator and the version check.

- **`_generated/` and `models.py` are the generator's.** `scripts/generate.py`
  renders them from the sdk_bundle example; `just python-sdk-generate` rewrites
  them, and this project's `lint` fails on a stale copy. Roots and messages are
  taken from the bundle's own keys, so one Rust adds is generated or refused by
  name, never skipped.
- **The development environment is `scripts/run`, not `uv run`.** The package is
  a member of the root uv workspace, and its `onemessagebus-cli` resolves to the
  root distribution; `scripts/run` syncs the root `.venv` from `uv.lock` with
  `--locked` and leaves that distribution unbuilt, since building it compiles the
  binary the tests take from `target/debug`. Re-resolve the lock with
  `just python-sdk-lock`.
- **Versions are stamped, never written.** `__version__`, `CLI_VERSION` and the
  CLI pin stay at the placeholder; `scripts/pack.py` stamps the workspace version
  into a publishable copy, and an unstamped checkout pins the checkout's own
  `Cargo.toml` version.
- **One `async def` per capability, and no other public method on `Client`.** The
  parity gate reads `class Client` that way: lifecycle is `async with`, and
  `client.schema` is an attribute.
- **Tests drive the built binary** (`cargo build -p onemessagebus-cli`) over both
  transports. A resident a test starts is stopped by closing the transport that
  started it, which removes its socket.
