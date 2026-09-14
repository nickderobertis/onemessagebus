// The version pin. Both constants are placeholders in source; scripts/pack.mjs
// stamps them in the built copy it publishes, from Cargo.toml's workspace version,
// and refuses to pack if either literal below has moved.

/** The version this package was published as; always `CLI_VERSION`, since both release as one version. */
export const SDK_VERSION: string = "0.0.0-dev";

/** The exact `onemessagebus-cli` version this SDK drives. */
export const CLI_VERSION: string = "0.0.0-dev";

/** What an unstamped constant holds. */
export const PLACEHOLDER = "0.0.0-dev";
