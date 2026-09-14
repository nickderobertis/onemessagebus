// What every journey needs: the real binary, a scratch directory it owns, and a
// client over each transport.
import { afterAll } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { z } from "zod";
import { Client, type ClientConfig, defineMessage, ResidentTransport } from "../src/index.js";

export const PACKAGE = resolve(import.meta.dir, "..");
export const ROOT = resolve(PACKAGE, "../..");
export const BINARY = resolve(ROOT, "target/debug/onemessagebus");

if (!existsSync(BINARY)) {
  throw new Error(
    `these tests drive the real binary at ${BINARY}, which is not built; build it with \`cargo build -p onemessagebus-cli --locked --quiet\` and rerun`,
  );
}

const made: string[] = [];
afterAll(() => {
  for (const dir of made.splice(0)) rmSync(dir, { recursive: true, force: true });
});

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

/** What `run` threw or rejected with; fails the test when it did neither. */
export async function caught(run: () => unknown): Promise<unknown> {
  try {
    await run();
  } catch (error) {
    return error;
  }
  throw new Error("expected a rejection, and the call resolved");
}
