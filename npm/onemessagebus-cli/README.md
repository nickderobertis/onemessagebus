# onemessagebus-cli

The `onemessagebus` command, as a prebuilt binary.

```bash
npm install -g onemessagebus-cli
onemessagebus --help
```

The binary ships inside a per-platform package that npm selects by `os`/`cpu`, so
there is no compile step and no Rust toolchain to install. The same binary is on
[PyPI](https://pypi.org/project/onemessagebus-cli/) (`pip install
onemessagebus-cli`), and builds from source with `cargo install --git
https://github.com/nickderobertis/onemessagebus onemessagebus-cli --locked`.

See [the repository](https://github.com/nickderobertis/onemessagebus) for what it
does and the contract it implements.
