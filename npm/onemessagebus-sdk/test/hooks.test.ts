// Hooks through the real binary, over each transport: a configuration built from
// the SDK's own `HookConfig` and written by `writeConfig`, every field of an
// entry taking effect, and each result a hook gives a caller — a dropped line from
// `send` and `reply`, `validate`'s outcome and records, `ask`'s `dropped` answer.
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { z } from "zod";
import {
  BusRefused,
  type Client,
  type Config,
  type Dropped,
  type HookConfig,
  type LogRecord,
  writeConfig,
} from "../src/index.js";
import { BINARY, caughtAs, QUESTION, removeScratch, scratch, TRANSPORTS } from "./support.js";

afterAll(removeScratch);

const sh = (script: string): string[] => ["sh", "-c", script];
/** Drops every message: exit 0, nothing printed. */
const DROP = sh("cat >/dev/null");
/** Changes a message to one wrapping it. */
const STAMP = sh(`read -r line; printf '{"stamped":true,"was":%s}\\n' "$line"`);
/** Keeps a message, and reroutes it to `routed`. */
const REROUTE = sh('cat; printf routed > "$ONEMESSAGEBUS_HOOK_ROUTE"');

/** Each hook the journeys run, by the queue it runs on. */
function hooks(dir: string): HookConfig[] {
  return [
    // `notes` drops what carries a `secret`, and `when` keeps every other note from it.
    { on: "notes", when: { carries: "secret" }, command: DROP },
    { on: "stamped", command: STAMP },
    { on: "inbound", routes: ["routed"], command: REROUTE },
    // `counted` keeps what it is offered, appending a line to `runs` each time it runs,
    // and caches each keep under the bar `echo bar-1` prints.
    {
      on: "counted",
      command: sh(`echo ran >> ${JSON.stringify(join(dir, "runs"))}; cat`),
      cache: { dir: join(dir, "hook-cache"), bar_fingerprint: ["echo", "bar-1"] },
    },
    { on: "vetted", command: DROP },
    { on: "replies", command: DROP },
  ];
}

function hookedConfig(dir: string): Config {
  return {
    version: 1,
    transport: { kind: "local", dir: join(dir, "bus") },
    queues: {
      notes: {},
      stamped: {},
      inbound: {},
      routed: {},
      counted: {},
      vetted: { policy: { hold_pending: true }, answers: "replies" },
      asks: { policy: { hold_pending: true }, answers: "replies" },
      replies: { numbered: true },
    },
    hooks: hooks(dir),
  };
}

/** The lines of `queue`'s drop log, `<queue>-dropped`, read up to the first. */
async function dropLog(client: Client, queue: string): Promise<LogRecord[]> {
  const lines: LogRecord[] = [];
  for await (const line of client.subscribe(`${queue}-dropped`, {
    until: { field: "queue", equals: queue },
    timeout: 10,
  })) {
    lines.push(line);
  }
  return lines;
}

const DropLine = z.looseObject({
  schema_version: z.literal(1),
  at: z.number(),
  queue: z.string(),
  hook: z.strictObject({ index: z.number(), command: z.array(z.string()) }),
  message: z.unknown(),
  correlation: z.string().optional(),
});

function isDropped(line: unknown): line is Dropped {
  return typeof line === "object" && line !== null && "dropped" in line;
}

test("writeConfig refuses a configuration the generated schema refuses, writing nothing", async () => {
  const dir = scratch("hooks-config");
  const path = join(dir, "onemessagebus.yaml");
  // A hook entry with the deprecated validator's `kind`, arriving as JSON from outside
  // TypeScript's view: the refusal is what is under test.
  const config: Config = JSON.parse(
    JSON.stringify({
      version: 1,
      transport: { kind: "memory" },
      hooks: [{ on: "notes", kind: "command", command: ["true"] }],
    }),
  );
  const refused = await caughtAs(BusRefused, () => writeConfig(config, path));
  expect(refused.message).toStartWith("config: at /hooks/0:");
  expect(existsSync(path)).toBe(false);
});

for (const transport of TRANSPORTS) {
  describe(`hooks over the ${transport.name} transport`, () => {
    let dir: string;
    let client: Client;

    beforeAll(async () => {
      dir = scratch(`hooks-${transport.name}`);
      const config = await writeConfig(hookedConfig(dir), join(dir, "onemessagebus.yaml"));
      client = transport.client({
        binary: BINARY,
        config,
        registry: join(dir, "registry"),
        cwd: dir,
        env: {},
      });
    });
    afterAll(async () => {
      await client.transport.close();
    });

    test("`when` skips a message: a plain note is appended, one carrying a secret is dropped", async () => {
      const [kept] = await client.send("notes", { text: "plain" });
      expect(kept).toEqual({ queue: "notes", position: expect.any(Number) });
      expect(isDropped(kept)).toBe(false);

      const [dropped] = await client.send("notes", { secret: "hunter2" });
      expect(dropped).toEqual({ queue: "notes", dropped: true });
      expect(isDropped(dropped)).toBe(true);

      const [status] = await client.status("notes");
      expect([status?.records, status?.dropped]).toEqual([1, 1]);
      expect(await client.status("notes", { format: "text" })).toContain(" dropped=1");
      const [line] = await dropLog(client, "notes");
      const logged = DropLine.parse(line?.record);
      expect(logged.hook).toEqual({ index: 0, command: DROP });
      expect(logged.message).toEqual({ secret: "hunter2" });
    });

    test("a change is what is appended", async () => {
      const [sent] = await client.send("stamped", { n: 1 });
      expect(sent).toEqual({ queue: "stamped", position: expect.any(Number) });
      const claimed = await client.next("stamped");
      expect(claimed?.record).toEqual({ stamped: true, was: { n: 1 } });
    });

    test("`routes` reroutes a message to its target queue, and the offered queue holds nothing", async () => {
      const [sent] = await client.send("inbound", { text: "elsewhere" });
      expect(sent).toEqual({ queue: "routed", position: expect.any(Number) });
      const [inbound] = await client.status("inbound");
      expect(inbound?.records).toBe(0);
      const claimed = await client.next("routed");
      expect(claimed?.record).toEqual({ text: "elsewhere" });
    });

    test("`cache` keeps a message it has seen without running the command again", async () => {
      const runs = () => readFileSync(join(dir, "runs"), "utf8").split("\n").filter(Boolean);
      await client.send("counted", { n: 1 });
      expect(runs()).toHaveLength(1);
      // the same message under the same bar is a cache hit: appended, and not run
      const [again] = await client.send("counted", { n: 1 });
      expect(again).toEqual({ queue: "counted", position: expect.any(Number) });
      expect(runs()).toHaveLength(1);
      // a message it has not seen runs it
      await client.send("counted", { n: 2 });
      expect(runs()).toHaveLength(2);
    });

    test("validate answers keep, change and drop by outcome, with the records a send would append", async () => {
      expect(await client.validate("notes", { text: "plain" })).toEqual({
        queue: "notes",
        verdict: "pass",
        outcome: "keep",
        records: [{ queue: "notes", record: { text: "plain" } }],
      });
      expect(await client.validate("stamped", { n: 2 })).toEqual({
        queue: "stamped",
        verdict: "pass",
        outcome: "change",
        records: [{ queue: "stamped", record: { stamped: true, was: { n: 2 } } }],
      });
      expect(await client.validate("notes", { secret: "x" })).toEqual({
        queue: "notes",
        verdict: "pass",
        outcome: "drop",
        records: [],
      });
      // validating sends nothing
      const [notes] = await client.status("notes");
      expect([notes?.records, notes?.dropped]).toEqual([1, 1]);
    });

    test("ask answers `dropped` at once when a hook drops the question", async () => {
      const started = Date.now();
      const answer = await client.ask("vetted", QUESTION, { timeout: 30, asker: "worker-1" });
      expect(Date.now() - started).toBeLessThan(15_000);
      expect(answer).toEqual({
        answer: "dropped",
        queue: "vetted",
        hook: { index: 0, command: DROP },
      });
      const [vetted] = await client.status("vetted");
      expect([vetted?.records, vetted?.dropped]).toEqual([0, 1]);
    });

    test("a reply a hook drops answers the caller as dropped, appending nothing to the answers queue", async () => {
      const asked = client.ask(
        "asks",
        { ...QUESTION, message: "which base?" },
        { timeout: 3, asker: "worker-2" },
      );
      const Question = z.looseObject({ message: z.string(), correlation: z.string() });
      let claimed = await client.next("asks", { type: Question });
      for (let tries = 0; claimed === undefined; tries += 1) {
        expect(tries).toBeLessThan(200);
        await new Promise((wake) => setTimeout(wake, 50));
        claimed = await client.next("asks", { type: Question });
      }
      const correlation = claimed.record.correlation;
      const replied = await client.reply("asks", correlation, { completion: true, reason: "main" });
      expect(replied).toEqual({ queue: "replies", dropped: true });
      // the caller reads it as dropped: narrowed by `"dropped" in`, it has no `sent`
      if (!("dropped" in replied)) throw new Error("the reply was delivered, not dropped");
      expect(replied.queue).toBe("replies");

      const [replies] = await client.status("replies");
      expect([replies?.records, replies?.dropped]).toEqual([0, 1]);
      const [line] = await dropLog(client, "replies");
      const logged = DropLine.parse(line?.record);
      expect(logged.correlation).toBe(correlation);
      expect(logged.message).toMatchObject({ completion: true, reason: "main" });
      // nothing answered the question, so the asker waits out its timeout
      expect((await asked).answer).toBe("timeout");
    });
  });
}
