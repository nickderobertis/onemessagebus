// The SDK schema bundle, from the Rust build that owns it.
//
// Every script that needs the contract — the TypeScript generator, the SDK
// coverage gate, the parity audit — reads it through here rather than running
// cargo each its own way: `cargo run -p onemessagebus-cli --example sdk_bundle`,
// into the clone's one target directory (.cargo/config.toml), so the example
// shares every dependency the workspace build already compiled, with a failure
// reported as which script needed the bundle, the tail of what cargo said, and
// the command to see all of it.
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** The repository root this script lives in. */
export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

/** The cargo invocation that prints the bundle and nothing else. */
export const BUNDLE_ARGS = [
  "run",
  "-q",
  "--locked",
  "-p",
  "onemessagebus-cli",
  "--example",
  "sdk_bundle",
];

/** A cargo failure can run to hundreds of lines; its tail is where the error is. */
const CAUSE_LINES = 20;

function tail(text) {
  const lines = String(text ?? "")
    .split("\n")
    .filter((line) => line.trim() !== "");
  if (lines.length <= CAUSE_LINES) return lines;
  return [
    `… ${lines.length - CAUSE_LINES} earlier line(s) omitted; run the command below for all of it`,
    ...lines.slice(-CAUSE_LINES),
  ];
}

/**
 * The parsed bundle. On failure this prints a bounded diagnostic naming `script`
 * and `rerun`, and exits 1: every caller is a gate or a generator, whose stack
 * trace is noise to the person reading it.
 *
 * @param {{script: string, rerun: string}} spec
 * @returns {any}
 */
export function schemaBundle({ script, rerun }) {
  let text;
  try {
    text = execFileSync("cargo", BUNDLE_ARGS, {
      cwd: ROOT,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (error) {
    console.error(
      `${script}: the Rust schema bundle did not build, so there is no contract to read.`,
    );
    const cause = tail(error.stderr);
    if (cause.length > 0) {
      console.error("  cargo said:");
      for (const line of cause) console.error(`    ${line}`);
    } else if (error.code === "ENOENT") {
      console.error("  cargo is not on PATH; install the pinned toolchain with `just bootstrap`.");
    }
    console.error(
      `  fix: make the bundle build, then rerun \`${rerun}\`.\n       See it in full with: cargo ${BUNDLE_ARGS.join(" ")}`,
    );
    process.exit(1);
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    console.error(`${script}: the bundle example ran but did not print JSON (${error.message}).`);
    console.error(
      `  fix: \`sdk_bundle\` must print the bundle and nothing else — a stray print in the example or in sdk_schema::bundle corrupts it. Then rerun \`${rerun}\`.`,
    );
    process.exit(1);
  }
}
