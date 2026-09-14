// Which `onemessagebus` a client runs, and whether it is the one this SDK drives.
import { execFile } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { TransportError, VersionMismatch } from "./errors.js";
import { CLI_VERSION, PLACEHOLDER } from "./version.js";

/** A program and the arguments that come before a verb's. */
export interface Binary {
  readonly command: string;
  readonly prefix: readonly string[];
}

/** How a client finds and runs the bus. */
export interface ClientConfig {
  /** The `onemessagebus` executable; resolved as `resolveBinary` says when absent. */
  readonly binary?: string | undefined;
  /** The configuration file every queue verb reads, unless a call names its own. */
  readonly config?: string | undefined;
  /** The transport directory every queue verb uses, unless a call names its own. */
  readonly transportDir?: string | undefined;
  /** The registry directory every verb that takes one uses, unless a call names its own. */
  readonly registry?: string | undefined;
  /** The working directory the binary runs in. */
  readonly cwd?: string | undefined;
  /** Variables added to the binary's environment. */
  readonly env?: Readonly<Record<string, string>> | undefined;
}

/** What a user reads for `binary`: the command and anything before the verb. */
export function describeBinary(binary: Binary): string {
  return [binary.command, ...binary.prefix].join(" ");
}

/**
 * `config.binary`, then `ONEMESSAGEBUS_BIN`, then the `onemessagebus-cli` package's
 * launcher run under this runtime, then `onemessagebus` on `PATH`.
 */
export function resolveBinary(config: ClientConfig = {}): Binary {
  if (config.binary) return { command: config.binary, prefix: [] };
  const fromEnv = config.env?.ONEMESSAGEBUS_BIN ?? process.env.ONEMESSAGEBUS_BIN;
  if (fromEnv) return { command: fromEnv, prefix: [] };
  try {
    const launcher = createRequire(import.meta.url).resolve(
      "onemessagebus-cli/bin/onemessagebus.js",
    );
    return { command: process.execPath, prefix: [launcher] };
  } catch {
    return { command: "onemessagebus", prefix: [] };
  }
}

/** The environment a spawned binary runs with. */
export function childEnv(config: ClientConfig): NodeJS.ProcessEnv {
  return { ...process.env, ...config.env };
}

/** The workspace version in the nearest `Cargo.toml` above `from` that declares one. */
export function checkoutVersion(from: string): string | undefined {
  let dir = from;
  while (true) {
    const manifest = join(dir, "Cargo.toml");
    if (existsSync(manifest)) {
      const text = readFileSync(manifest, "utf8");
      const section = /^\[workspace\.package\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/mu.exec(text)?.[1];
      const version = section && /^version\s*=\s*"([^"]+)"/mu.exec(section)?.[1];
      if (version) return version;
    }
    const parent = dirname(dir);
    if (parent === dir) return undefined;
    dir = parent;
  }
}

/**
 * The CLI version this SDK drives: the stamped pin, or — in a development checkout,
 * where the pin is still the placeholder — the checkout's own workspace version.
 */
export function pinnedVersion(
  stamped: string = CLI_VERSION,
  from: string = dirname(fileURLToPath(import.meta.url)),
): string {
  if (stamped !== PLACEHOLDER) return stamped;
  const version = checkoutVersion(from);
  if (version === undefined) {
    throw new VersionMismatch(
      `this onemessagebus SDK carries no CLI version pin (it was not packed with scripts/pack.mjs) and sits in no onemessagebus checkout to read one from; install a published @onemessagebus/sdk`,
      PLACEHOLDER,
      "unknown",
    );
  }
  return version;
}

function run(binary: Binary, args: readonly string[], config: ClientConfig): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      binary.command,
      [...binary.prefix, ...args],
      { cwd: config.cwd, env: childEnv(config), encoding: "utf8" },
      (error, stdout, stderr) => {
        if (error && typeof error.code === "string") {
          reject(
            new TransportError(
              `could not run ${describeBinary(binary)}: ${error.message}; install onemessagebus-cli, or name the binary with ClientConfig.binary or ONEMESSAGEBUS_BIN`,
              error,
            ),
          );
        } else if (error) {
          reject(
            new TransportError(
              `${describeBinary(binary)} --version failed: ${stderr.trim() || error.message}; check that it is an onemessagebus binary`,
              error,
            ),
          );
        } else resolve(stdout);
      },
    );
  });
}

/** Resolves when `binary` reports exactly the version this SDK drives. */
export async function verifyVersion(
  binary: Binary,
  config: ClientConfig = {},
  expected: string = pinnedVersion(),
): Promise<void> {
  const printed = (await run(binary, ["--version"], config)).trim();
  const actual = /^onemessagebus (\S+)$/u.exec(printed)?.[1];
  if (actual === undefined) {
    throw new VersionMismatch(
      `${describeBinary(binary)} --version printed ${JSON.stringify(printed)}, not \`onemessagebus <version>\`; it is not an onemessagebus binary. Install onemessagebus-cli@${expected}`,
      expected,
      printed,
    );
  }
  if (actual !== expected) {
    throw new VersionMismatch(
      `this onemessagebus SDK drives onemessagebus-cli ${expected}, and ${describeBinary(binary)} reports ${actual}; install onemessagebus-cli@${expected}`,
      expected,
      actual,
    );
  }
}
