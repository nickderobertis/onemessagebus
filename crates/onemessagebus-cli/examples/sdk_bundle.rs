//! Print the SDK schema bundle the language packages are generated from: the
//! capability manifest and every contract root over the agent profile, with
//! every message the binary registers — the profile's and the resident
//! protocol's.
//!
//! `cargo run -q -p onemessagebus-cli --example sdk_bundle` is what
//! `just node-sdk-generate`, `just python-sdk-generate` and `scripts/sdk-coverage.mjs`
//! read; it prints the bundle and nothing else.

use onemessagebus::sdk_schema;
use onemessagebus_agent::Agent;

fn main() {
    print!(
        "{}",
        sdk_schema::bundle::<Agent>(&onemessagebus_cli::registry()).to_json()
    );
}
