// What every journey needs: the real binary, a scratch directory it owns, and a
// client over each transport.
import { dlopen, FFIType } from "bun:ffi";
import {
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { z } from "zod";
import { Client, type ClientConfig, defineMessage, ResidentTransport } from "../src/index.js";

export const PACKAGE = resolve(import.meta.dir, "..");
export const ROOT = resolve(PACKAGE, "../..");
/** `path`, once it is a binary to drive; a run without one fails saying how to build it. */
export function requireBinary(path: string): string {
  if (!existsSync(path)) {
    throw new Error(
      `these tests drive the real binary at ${path}, which is not built; build it with \`just nx run onemessagebus-cli:build\` and rerun`,
    );
  }
  return path;
}

export const BINARY = requireBinary(resolve(ROOT, "target/debug/onemessagebus"));

const made: string[] = [];

/**
 * Remove every directory `scratch` and `socketPath` made so far. This module is
 * imported once for the whole run, so each test file registers
 * `afterAll(removeScratch)` itself: a hook registered here would belong to
 * whichever file loaded it first.
 */
export function removeScratch(): void {
  for (const dir of made.splice(0)) rmSync(dir, { recursive: true, force: true });
}

/** A fresh directory this test file owns, removed after it. */
export function scratch(label: string): string {
  const base = process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir();
  mkdirSync(base, { recursive: true });
  const dir = mkdtempSync(join(base, `sdk-${label}-`));
  made.push(dir);
  return dir;
}

/** A path short enough for a unix socket address, in a directory removed after the test file. */
export function socketPath(): string {
  const dir = mkdtempSync(join(tmpdir(), "omb-"));
  made.push(dir);
  return join(dir, "bus.sock");
}

/** flock(2)'s exclusive lock, the one Rust's `File::try_lock` looks for on unix. */
const LOCK_EX = 2;

/**
 * A receiver bound to the spool at `dir`, as `docs/inbox.md` lays one out: its
 * declaration, and `receiver.lock` held exclusively until `release()`. That lock
 * is what `deliver` reads a taken message by — waited on while it is held, and
 * abandoned once it is not. Node has no flock binding, so libc's is called
 * directly; the SDK's journeys run on unix.
 */
export function bindSpool(dir: string, schema = "agent.note@1"): { release(): void } {
  mkdirSync(dir);
  writeFileSync(join(dir, "spool.json"), JSON.stringify({ schema_version: 1, schema }));
  const libc = dlopen(process.platform === "darwin" ? "libc.dylib" : "libc.so.6", {
    flock: { args: [FFIType.i32, FFIType.i32], returns: FFIType.i32 },
  });
  const lock = openSync(join(dir, "receiver.lock"), "w");
  if (libc.symbols.flock(lock, LOCK_EX) !== 0) {
    closeSync(lock);
    libc.close();
    throw new Error(`could not lock ${join(dir, "receiver.lock")} as the spool's receiver`);
  }
  return {
    release: () => {
      closeSync(lock);
      libc.close();
    },
  };
}

/** The message type the journeys declare in TypeScript and register at run time. */
export const Greeting = defineMessage("demo.greeting@1", z.object({ text: z.string() }));

/** A planner surface, as `agent.planner-surface@1` takes one. */
export const SURFACE = {
  kind: "finding",
  message: "the base moved",
  source: "proposal",
  blocking: false,
};

/**
 * A configuration over a local transport in `dir/channel`: the planner channel's
 * queues; `greetings`, typed `demo.greeting@1`; `judged`, whose validator refuses
 * a record that does not say `quiet`; and the onejudge codec reading its run from
 * `TEST_SERVE_RUN`.
 */
export function writeConfig(dir: string): string {
  const path = join(dir, "onemessagebus.yaml");
  const judge = "grep -q quiet || { echo 'too loud' >&2; exit 1; }";
  writeFileSync(
    path,
    [
      "version: 1",
      `transport: {kind: local, dir: ${JSON.stringify(join(dir, "channel"))}}`,
      "profile: planner-channel",
      "queues:",
      "  greetings: {schema: demo.greeting@1}",
      "  judged: {}",
      "validators:",
      `  - {on: judged, kind: command, command: [sh, -c, ${JSON.stringify(judge)}]}`,
      "codecs:",
      "  onejudge: {reply_window_seconds: 1, run_env: TEST_SERVE_RUN, asker_env: TEST_SERVE_ASKER, session_env: TEST_SERVE_SESSION}",
      "",
    ].join("\n"),
  );
  return path;
}

/** The configuration a journey's client runs with. */
export function baseConfig(dir: string): ClientConfig {
  return {
    binary: BINARY,
    config: writeConfig(dir),
    registry: join(dir, "registry"),
    cwd: dir,
    env: { TEST_SERVE_RUN: "r-7", CODEX_ALT_HOME: "/nowhere/codex-alt" },
  };
}

export interface TransportCase {
  readonly name: "cli" | "resident";
  client(config: ClientConfig): Client;
}

/** Every transport a capability journey runs over. */
export const TRANSPORTS: readonly TransportCase[] = [
  { name: "cli", client: (config) => new Client({ config }) },
  {
    name: "resident",
    client: (config) =>
      new Client({ config, transport: new ResidentTransport({ socket: socketPath() }) }),
  },
];

/** The error `run` threw or rejected with; fails the test when it did neither, or threw a non-Error. */
export async function caught(run: () => unknown): Promise<Error> {
  try {
    await run();
  } catch (error) {
    if (error instanceof Error) return error;
    throw error;
  }
  throw new Error("expected a rejection, and the call resolved");
}

/** `caught`, as the error class the test expects; any other failure fails the test with itself. */
export async function caughtAs<E extends Error>(
  kind: new (...args: never[]) => E,
  run: () => unknown,
): Promise<E> {
  const error = await caught(run);
  if (error instanceof kind) return error;
  throw error;
}
