//! Print the SDK schema bundle the language packages are generated from: the
//! capability manifest and every contract root over the open vocabulary, with
//! every message the binary registers — the transport plugin protocol's and the
//! resident protocol's.
//!
//! `cargo run -q -p onemessagebus-cli --example sdk_bundle` is what
//! `just node-sdk-generate`, `just python-sdk-generate` and `parity/sdk-coverage.mjs`
//! read; it prints the bundle and nothing else.

use onemessagebus::{sdk_schema, Open};

fn main() {
    print!(
        "{}",
        sdk_schema::bundle::<Open>(&onemessagebus_cli::registry()).to_json()
    );
}
