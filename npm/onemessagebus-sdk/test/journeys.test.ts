// Every capability through the real binary, over each transport: what a user
// calls, happy path and refusal, with the typed error each refusal maps to.
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { existsSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { z } from "zod";
import {
  type Answer,
  BusError,
  BusFailed,
  BusRefused,
  type Client,
  ContractError,
  messages,
  schemas,
} from "../src/index.js";
import {
  BINARY,
  baseConfig,
  bindSpool,
  caught,
  caughtAs,
  Greeting,
  removeScratch,
  SURFACE,
  scratch,
  TRANSPORTS,
} from "./support.js";

afterAll(removeScratch);

const refusedWith = caughtAs;

/** What the ask journey reads of a surface it claims: whether it is the question, and its correlation. */
const Question = z.looseObject({ kind: z.string(), correlation: z.string().optional() });
/** What it reads of the reply record the answer carries. */
const QueuedReply = z.looseObject({ reply: z.looseObject({ reason: z.string() }) });

for (const transport of TRANSPORTS) {
  describe(`over the ${transport.name} transport`, () => {
    let dir: string;
    let client: Client;

    beforeAll(() => {
      dir = scratch(transport.name);
      client = transport.client(baseConfig(dir));
    });
    afterAll(async () => {
      await client.transport.close();
    });

    test("transports lists the kinds this build opens, as JSON or text", async () => {
      const kinds = await client.transports();
      expect(kinds.map((kind) => kind.kind)).toContain("local");
      expect(await client.transports({ format: "text" })).toContain("local builtin");
      const refused = await refusedWith(BusRefused, () =>
        client.transports(JSON.parse('{"format":"yaml"}')),
      );
      expect(refused.message).toStartWith("transports: `format`");
    });

    test("schema.register records a defineMessage type, and a different document under a held id is refused", async () => {
      expect(await client.schema.register(Greeting)).toBe("demo.greeting@1");
      // the same document again is fine
      expect(await client.schema.register(Greeting)).toBe("demo.greeting@1");
      expect(existsSync(join(dir, "registry", "demo.greeting@1.json"))).toBe(true);
      const conflict = await refusedWith(BusError, () =>
        client.schema.register({ type: "object" }, "demo.greeting@1"),
      );
      expect(conflict.exit === 1 || conflict.exit === 2).toBe(true);
      expect(conflict.message).toContain("demo.greeting@1");
      expect(await caught(() => client.schema.register({ type: "object" }))).toBeInstanceOf(
        TypeError,
      );
      expect(await caught(() => client.schema.register({ type: "object" }, "nope"))).toBeInstanceOf(
        TypeError,
      );
    });

    test("schemaList lists the profile's ids and the registered ones", async () => {
      const ids = (await client.schema.list()).map((entry) => entry.id);
      expect(ids).toContain("demo.greeting@1");
      expect(ids).toContain("bus.resident-protocol@1");
      const text = await client.schemaList({ format: "text" });
      expect(text.split("\n")).toContain("agent.note@1");
      writeFileSync(join(dir, "not-a-directory"), "");
      const refused = await refusedWith(BusError, () =>
        client.schemaList({ registry: join(dir, "not-a-directory") }),
      );
      expect(refused.message).toContain("not-a-directory");
    });

    test("schemaCheck passes a conforming payload and names the pointer of a violation", async () => {
      await client.schema.check(Greeting, { text: "hi" });
      expect(await client.schemaCheck({ id: "demo.greeting@1" }, { text: "hi" })).toBe("");
      const violation = await refusedWith(BusFailed, () =>
        client.schema.check("demo.greeting@1", { text: false }),
      );
      expect(violation.message).toContain("demo.greeting@1");
      expect(violation.message).toContain("/text");
      const unregistered = await refusedWith(BusRefused, () =>
        client.schemaCheck({ id: "demo.nothing@1" }, {}),
      );
      expect(unregistered.message).toContain("demo.nothing@1");
    });

    test("schemaGen renders a registered document, and refuses a language this build leaves to the SDKs", async () => {
      const rendered = JSON.parse(
        await client.schemaGen({ id: "bus.resident-protocol@1", lang: "json" }),
      );
      expect(rendered.title).toBe("ResidentLine");
      const refused = await refusedWith(BusRefused, () =>
        client.schemaGen({ id: "bus.resident-protocol@1", lang: "typescript" }),
      );
      expect(refused.message).toContain("typescript");
    });

    test("send appends a typed record, and the core refuses a violation naming the id and pointer", async () => {
      const [sent] = await client.send("greetings", Greeting.parse({ text: "hello" }));
      expect(sent?.queue).toBe("greetings");
      expect(typeof sent?.position).toBe("number");
      const typed = await client.send("greetings", { text: "again" }, { type: Greeting });
      expect(typed).toHaveLength(1);

      const violation = await refusedWith(BusFailed, () => client.send("greetings", { text: 7 }));
      expect(violation.message).toContain("demo.greeting@1");
      expect(violation.message).toContain("/text");
      // the same payload stopped in the SDK, by the same schema, in the same words
      const early = await refusedWith(BusFailed, () =>
        client.send("greetings", JSON.parse('{"text":7}'), { type: Greeting }),
      );
      expect(early.message).toStartWith("demo.greeting@1: at /text:");

      const undeclared = await refusedWith(BusRefused, () => client.send("nowhere", SURFACE));
      expect(undeclared.message).toContain("`nowhere` is not a queue this configuration declares");
    });

    test("validate answers a pass and a refusal as verdicts, and throws on an undeclared queue", async () => {
      expect(await client.validate("judged", { say: "quiet please" })).toEqual({
        queue: "judged",
        verdict: "pass",
      });
      const refused = await client.validate("judged", { say: "LOUD" });
      expect(refused.verdict).toBe("refuse");
      expect(refused.reason).toContain("too loud");
      await refusedWith(BusRefused, () => client.validate("nowhere", {}));
    });

    test("next claims a record typed by its message, and answers undefined on an empty queue", async () => {
      const claimed = await client.next("greetings", { type: Greeting });
      expect(claimed?.record.text).toBe("hello");
      expect(await client.next("greetings", { format: "text" })).toMatch(
        /^greetings \d+ \{"text":"again"\}\n$/u,
      );
      expect(await client.next("greetings")).toBeUndefined();
      await refusedWith(BusRefused, () => client.next("nowhere"));
      // a record that is not the type asked for is a contract error, not a silent cast
      await client.send("judged", { say: "quiet" });
      const mistyped = await refusedWith(ContractError, () =>
        client.next("judged", { type: Greeting }),
      );
      expect(mistyped.message).toContain("demo.greeting@1");
    });

    test("status reports a queue as JSON or text, and refuses an undeclared one", async () => {
      const [status] = await client.status("greetings");
      expect(status?.queue).toBe("greetings");
      expect(await client.status("greetings", { format: "text" })).toStartWith("greetings ");
      expect((await client.status()).length).toBeGreaterThan(1);
      await refusedWith(BusRefused, () => client.status("nowhere"));
    });

    test("subscribe streams log records until its predicate holds", async () => {
      const seen = [];
      for await (const line of client.subscribe("greetings", {
        until: { field: "text", equals: "again" },
        timeout: 10,
      })) {
        seen.push(line.record);
      }
      expect(seen).toEqual([{ text: "hello" }, { text: "again" }]);
      const text = [];
      for await (const line of client.subscribe("greetings", {
        until: '{"field":"text","equals":"hello"}',
        format: "text",
      })) {
        text.push(line);
      }
      expect(text[0]).toEndWith('{"text":"hello"}');
    });

    test("subscribe with nothing admitted within its timeout throws BusFailed, and a bad predicate BusRefused", async () => {
      const lapsed = await refusedWith(BusFailed, async () => {
        for await (const _ of client.subscribe("commands", {
          until: { field: "x", present: true },
          timeout: 1,
        })) {
          // commands is empty
        }
      });
      expect(lapsed.message).toBe("commands: no record --until admits arrived within 1 seconds");
      await refusedWith(BusRefused, async () => {
        for await (const _ of client.subscribe("greetings", { until: '{"nonsense":1}' })) {
          // refused before a line
        }
      });
    });

    test("leaving a subscription early stops it, and the client goes on", async () => {
      const started = Date.now();
      for await (const line of client.subscribe("greetings", {
        until: { field: "text", equals: "never" },
      })) {
        expect(line.record).toEqual({ text: "hello" });
        break;
      }
      // an unbounded subscription only ends this promptly because it was stopped
      expect(Date.now() - started).toBeLessThan(10_000);
      expect((await client.status("greetings"))[0]?.queue).toBe("greetings");
    });

    test("ask answers a timeout when nobody replies", async () => {
      const answer = await client.ask(
        "surfaces",
        { ...SURFACE, message: "anyone?" },
        { timeout: 1, asker: "nobody" },
      );
      expect(answer.answer).toBe("timeout");
      if (answer.answer === "timeout") expect(answer.correlation).toStartWith("c-");
    });

    test("ask resolves the reply a concurrent claim and reply give it", async () => {
      const asked = client.ask(
        "surfaces",
        { ...SURFACE, kind: "planner-question", message: "which base?", blocking: true },
        { blocking: true, asker: "worker-1", timeout: 30, about: "task-7" },
      );
      let claimed = await client.next("surfaces", { type: Question });
      let correlation: string | undefined;
      for (let tries = 0; correlation === undefined; tries += 1) {
        expect(tries).toBeLessThan(200);
        if (claimed?.record.kind === "planner-question") correlation = claimed.record.correlation;
        else {
          await new Promise((wake) => setTimeout(wake, 50));
          claimed = await client.next("surfaces", { type: Question });
        }
      }
      const replied = await client.reply("surfaces", correlation, {
        version: 3,
        completion: true,
        reason: "main",
      });
      expect(replied.correlation).toBe(correlation);
      expect(replied.sent.length).toBeGreaterThan(0);
      const answer: Answer = await asked;
      expect(answer.answer).toBe("reply");
      if (answer.answer === "reply") {
        expect(answer.correlation).toBe(correlation);
        expect(QueuedReply.parse(answer.reply).reply.reason).toBe("main");
      }
    });

    test("ask throws BusRefused for a question the bus refuses outright", async () => {
      const refused = await refusedWith(BusRefused, () =>
        client.ask("surfaces", ["not an object"]),
      );
      expect(refused.message.length).toBeGreaterThan(0);
    });

    test("reply refuses a correlation nothing pending holds, and a queue that answers on none", async () => {
      const unknown = await refusedWith(BusFailed, () =>
        client.reply("surfaces", "c-nothing", { version: 3, completion: true, reason: "x" }),
      );
      expect(unknown.message).toContain("c-nothing");
      const noAnswers = await refusedWith(BusRefused, () =>
        client.reply("greetings", undefined, {}),
      );
      expect(noAnswers.message).toContain("greetings declares no queue its replies");
    });

    test("eventsEmit appends an envelope and eventsMerge reads the stream back", async () => {
      const path = join(dir, "events.ndjson");
      const envelope = await client.eventsEmit(
        { path, kind: "change-merged", stream: "s1", labels: { run_id: "R", round: "2" } },
        { branch: "main" },
      );
      expect(envelope.seq).toBe(1);
      expect(envelope.labels?.round).toBe(2);
      const merged = await client.eventsMerge({ files: [path] });
      expect(merged).toHaveLength(1);
      expect(await client.eventsMerge({ files: [path], format: "text" })).toContain(
        "change-merged",
      );
      const badKind = await refusedWith(BusRefused, () =>
        client.eventsEmit({ path, kind: "Not_Kebab", stream: "s1" }, {}),
      );
      expect(badKind.message).toContain("Not_Kebab");
      await refusedWith(BusRefused, () =>
        client.eventsMerge({ files: [path], filter: '{"all":[{"field":"nope","equals":1}]}' }),
      );
    });

    test("deliver hands a message to a bound spool's receiver and waits out its answer", async () => {
      const spool = join(dir, `spool-${transport.name}`);
      const receiver = bindSpool(spool);
      // The receiver's courier takes an offer by renaming it, and answers several of
      // the sender's polls later: while the receiver stays bound, a taken message
      // is waited on rather than abandoned.
      const courier = setInterval(() => {
        for (const name of readdirSync(spool)) {
          if (!name.endsWith(".offer.json")) continue;
          const offer = name.slice(0, -".offer.json".length);
          renameSync(join(spool, name), join(spool, `${offer}.taken.json`));
          setTimeout(() => {
            const staging = join(spool, `${offer}.answer.json.staging`);
            writeFileSync(staging, '{"schema_version":1,"answer":{"disposition":{"queued":true}}}');
            renameSync(staging, join(spool, `${offer}.answer.json`));
          }, 100);
        }
      }, 20);
      try {
        expect(
          await client.deliver({ address: spool, wait: 5 }, { addressee: "worker", text: "hi" }),
        ).toEqual({ queued: true });
      } finally {
        clearInterval(courier);
        receiver.release();
      }
      const nowhere = await refusedWith(BusRefused, () =>
        client.deliver({ address: join(dir, "nowhere"), message: "{}", wait: 1 }),
      );
      expect(nowhere.message).toContain("is not a spool");
    });

    test("inboxCarried lists a carry store, and refuses a path that is none", async () => {
      const store = join(dir, "carried.ndjson");
      const note = { addressee: "worker", text: "carried while nobody ran" };
      writeFileSync(
        store,
        `{"schema_version":1,"kind":"onemessagebus-carry-store"}\n${JSON.stringify({ ts: "2026-09-13T00:00:00.000Z", schema: "agent.note@1", message: note })}\n`,
      );
      const entries = await client.inboxCarried({ store });
      expect(entries).toHaveLength(1);
      expect(entries[0]?.message).toEqual(note);
      expect(await client.inboxCarried({ store, format: "text" })).toContain("agent.note@1");
      const refused = await refusedWith(BusRefused, () =>
        client.inboxCarried({ store: join(dir, "nowhere") }),
      );
      expect(refused.message).toContain("is not a carry store");
    });

    test("serve answers each frame of a codec session, and refuses an operation it does not serve", async () => {
      expect(await client.serve({ queue: "surfaces", codec: "onejudge" }, "")).toEqual([]);
      const [response] = await client.serve({ queue: "surfaces", codec: "onejudge" }, [
        {
          op: "supervisor",
          task: "onepipeline run `r-7`.\nWatch the build.",
          persona: "A careful monitor.",
          done_when: "the watch is kept",
          worktree: "/repo",
          history_name: "r-7-monitor",
          messages: [
            { role: "user", content: "watch" },
            { role: "assistant", content: "nothing drifted this turn" },
          ],
          session: "r-7-user",
        },
      ]);
      expect(response?.completion).toBe(false);
      const refused = await refusedWith(BusRefused, () =>
        client.serve({ queue: "surfaces", codec: "onejudge" }, [
          { op: "assess", prompt: "Identify follow-up work.", messages: [] },
        ]),
      );
      expect(refused.message).toContain("assess");
    });

    test("a profile's Rust-registered message round-trips through its generated schema", async () => {
      const channel = scratch(`${transport.name}-profile`);
      const profile = transport.client({ binary: BINARY, transportDir: channel, cwd: channel });
      try {
        // The core stamps the surface's `id`, so what is sent is not yet a whole surface.
        await profile.send("surfaces", { ...SURFACE, message: "typed" });
        const claimed = await profile.next("surfaces", { type: schemas.PlannerSurface });
        expect(claimed?.record.message).toBe("typed");
        expect(messages.AgentPlannerSurfaceV1Schema.parse(claimed?.record).kind).toBe("finding");
        expect(messages.MESSAGES["agent.planner-surface@1"].id).toBe("agent.planner-surface@1");
        expect(schemas.PlannerSurface).toBe(schemas.PlannerSurfaceV1);
        expect(schemas.EventEnvelope.id).toBe("agent.event-envelope@2");
        // a bare Zod schema is a type too
        await profile.send("surfaces", { ...SURFACE, message: "bare" });
        const bare = await profile.next("surfaces", { type: schemas.PlannerSurface.schema });
        expect(bare?.record.message).toBe("bare");
        const refused = await refusedWith(BusFailed, () =>
          profile.send("surfaces", JSON.parse('{"kind":7}'), {
            type: schemas.PlannerSurface.schema,
          }),
        );
        expect(refused.message).toStartWith("payload: at /");
        expect(messages.AgentPlannerSurfaceV1.jsonSchema()).toHaveProperty("$defs");
      } finally {
        await profile.transport.close();
      }
    });
  });
}
