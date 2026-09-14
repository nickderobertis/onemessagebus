// The manifest is how a call is rendered: every binding of every capability must
// reach the command line from the client method that owns it.
import { afterAll, describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  type Args,
  BusRefused,
  CAPABILITIES,
  type CapabilityMethod,
  Client,
  METHODS,
  renderArgv,
  type Transport,
} from "../src/index.js";
import { caught, PACKAGE, removeScratch } from "./support.js";

afterAll(removeScratch);

/** A value for every option any capability binds, each one the option's schema admits. */
const VALUES: Record<string, unknown> = {
  queue: "surfaces",
  id: "demo.greeting@1",
  file: "record.json",
  registry: "registry",
  format: "json",
  lang: "rust",
  files: ["a.ndjson", "b.ndjson"],
  filter: '{"all":[]}',
  profile: "agent",
  path: "events.ndjson",
  kind: "change-merged",
  stream: "s1",
  source: "pipeline",
  labels: { run_id: "R", round: "2" },
  address: "spool",
  message: "{}",
  wait: 3,
  store: "carried.ndjson",
  config: "onemessagebus.yaml",
  transportDir: "channel",
  consumer: "reader",
  asker: "worker-1",
  position: 187,
  correlation: "c-1",
  until: '{"field":"event","equals":"answered"}',
  timeout: 30,
  blocking: true,
  about: "task-7",
  codec: "onejudge",
  sessionSeconds: 60,
};

class Stop extends Error {}

/** Records what the client hands the transport, and answers nothing. */
class Recording implements Transport {
  readonly calls: { capability: CapabilityMethod; args: Args }[] = [];
  async call(capability: CapabilityMethod, args: Args): Promise<unknown> {
    this.calls.push({ capability, args });
    throw new Stop();
  }
  // biome-ignore lint/correctness/useYield: a stream that ends before it yields
  async *stream(capability: CapabilityMethod, args: Args): AsyncGenerator<unknown> {
    this.calls.push({ capability, args });
    throw new Stop();
  }
  async close(): Promise<void> {}
}

/** Each method, called with every option its capability binds. */
const EVERY_OPTION: Record<CapabilityMethod, (client: Client, v: typeof VALUES) => unknown> = {
  schemaList: (c, v) => c.schemaList({ registry: v.registry as string, format: "json" }),
  schemaCheck: (c, v) =>
    c.schemaCheck({ id: v.id as string, file: v.file as string, registry: v.registry as string }),
  schemaGen: (c, v) =>
    c.schemaGen({ id: v.id as string, lang: "rust", registry: v.registry as string }),
  schemaRegister: (c, v) =>
    c.schemaRegister({
      id: v.id as string,
      file: v.file as string,
      registry: v.registry as string,
    }),
  eventsMerge: (c, v) =>
    c.eventsMerge({
      files: v.files as string[],
      filter: v.filter as string,
      profile: v.profile as string,
      format: "json",
    }),
  eventsEmit: (c, v) =>
    c.eventsEmit({
      path: v.path as string,
      kind: v.kind as string,
      stream: v.stream as string,
      source: v.source as string,
      profile: v.profile as string,
      labels: v.labels as Record<string, string>,
      file: v.file as string,
      format: "json",
    }),
  deliver: (c, v) =>
    c.deliver({
      address: v.address as string,
      message: v.message as string,
      file: v.file as string,
      wait: v.wait as number,
    }),
  inboxCarried: (c, v) => c.inboxCarried({ store: v.store as string, format: "json" }),
  send: (c, v) =>
    c.send(v.queue as string, undefined, {
      file: v.file as string,
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    }),
  next: (c, v) =>
    c.next(v.queue as string, {
      consumer: v.consumer as string,
      asker: v.asker as string,
      format: "json",
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    }),
  reply: (c, v) =>
    c.reply(v.queue as string, v.correlation as string, undefined, {
      position: v.position as number,
      file: v.file as string,
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    }),
  subscribe: async (c, v) => {
    for await (const _ of c.subscribe(v.queue as string, {
      until: v.until as string,
      timeout: v.timeout as number,
      format: "json",
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    })) {
      // the recording transport ends the stream before a line
    }
  },
  status: (c, v) =>
    c.status(v.queue as string, {
      format: "json",
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    }),
  transports: (c) => c.transports({ format: "json" }),
  validate: (c, v) =>
    c.validate(v.queue as string, undefined, {
      file: v.file as string,
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    }),
  ask: (c, v) =>
    c.ask(v.queue as string, undefined, {
      blocking: true,
      asker: v.asker as string,
      about: v.about as string,
      timeout: v.timeout as number,
      correlation: v.correlation as string,
      file: v.file as string,
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    }),
  serve: (c, v) =>
    c.serve({
      queue: v.queue as string,
      codec: "onejudge",
      sessionSeconds: v.sessionSeconds as number,
      asker: v.asker as string,
      file: v.file as string,
      config: v.config as string,
      transportDir: v.transportDir as string,
      registry: v.registry as string,
    }),
};

function expectRendered(capability: CapabilityMethod, args: Args): void {
  const argv = renderArgv(capability, args);
  const declared = CAPABILITIES[capability];
  expect(argv.slice(0, declared.verb.length)).toEqual([...declared.verb]);
  const separator = argv.indexOf("--");
  const flags = separator === -1 ? argv : argv.slice(0, separator);
  const positionals = separator === -1 ? [] : argv.slice(separator + 1);
  const expectedPositionals: string[] = [];
  for (const binding of declared.bindings) {
    const value = args[binding.option];
    expect(value, `${capability} did not pass \`${binding.option}\``).toBeDefined();
    switch (binding.kind) {
      case "positional":
        expectedPositionals.push(...(Array.isArray(value) ? value : [value]).map(String));
        break;
      case "value": {
        const at = flags.indexOf(binding.flag);
        expect(at, `${capability} rendered no ${binding.flag}`).toBeGreaterThan(-1);
        expect(flags[at + 1]).toBe(String(value));
        break;
      }
      case "switch":
        expect(flags).toContain(binding.flag);
        break;
      case "key-value":
        for (const [key, item] of Object.entries(value as object)) {
          const at = flags.findIndex(
            (word, i) => word === binding.flag && flags[i + 1] === `${key}=${item}`,
          );
          expect(at, `${capability} rendered no ${binding.flag} ${key}=${item}`).toBeGreaterThan(
            -1,
          );
        }
        break;
      default:
        throw new Error(
          `the manifest binds a kind this test does not know: ${(binding as { kind: string }).kind}`,
        );
    }
  }
  expect(positionals).toEqual(expectedPositionals);
}

describe("every capability renders every option it binds", () => {
  for (const method of METHODS) {
    test(method, async () => {
      const transport = new Recording();
      const client = new Client({ transport });
      expect(await caught(() => EVERY_OPTION[method](client, VALUES))).toBeInstanceOf(Stop);
      const [call] = transport.calls;
      expect(call?.capability).toBe(method);
      expectRendered(method, call?.args ?? {});
    });
  }
});

describe("renderArgv", () => {
  test("puts positionals after `--`, so a dash-led value is still a positional", () => {
    expect(renderArgv("schemaCheck", { id: "-x", registry: "r" })).toEqual([
      "schema",
      "check",
      "--registry",
      "r",
      "--",
      "-x",
    ]);
  });

  test("renders nothing for an absent, undefined or null option, and no `--` without positionals", () => {
    expect(renderArgv("transports", {})).toEqual(["transports"]);
    expect(renderArgv("status", { queue: undefined, format: null })).toEqual(["status"]);
  });

  test("renders a false switch as nothing, and a numeric positional as its digits", () => {
    expect(renderArgv("ask", { queue: "q", blocking: false })).toEqual(["ask", "--", "q"]);
    expect(renderArgv("reply", { queue: "q", position: 12 })).toEqual(["reply", "--", "q", "12"]);
  });

  test("refuses an option the capability does not bind, and a value no flag can carry", () => {
    const unknown = renderArgvError("status", { queues: "a" });
    expect(unknown).toBeInstanceOf(BusRefused);
    expect(unknown.message).toBe("`queues` is not an option of status");
    expect(renderArgvError("status", { queue: { name: "a" } }).message).toBe(
      "status: `queue` takes a string, a number or a boolean, not object",
    );
    expect(renderArgvError("ask", { queue: "q", blocking: "yes" }).message).toBe(
      "ask: `blocking` is true or false, not string",
    );
    expect(renderArgvError("eventsEmit", { labels: "run_id=R" }).message).toBe(
      "eventsEmit: `labels` is an object of keys to values, not string",
    );
    expect(renderArgvError("eventsEmit", { labels: ["a"] }).message).toBe(
      "eventsEmit: `labels` is an object of keys to values, not an array",
    );
    expect(renderArgvError("status", { format: ["a"] }).message).toBe(
      "status: `format` takes a string, a number or a boolean, not an array",
    );
    expect(renderArgv("eventsMerge", { files: ["-a", "b"] })).toEqual([
      "events",
      "merge",
      "--",
      "-a",
      "b",
    ]);
    expect(renderArgvError("publish" as CapabilityMethod, {}).message).toBe(
      "`publish` is not a capability of onemessagebus",
    );
  });
});

function renderArgvError(capability: CapabilityMethod, args: Args): Error {
  try {
    renderArgv(capability, args);
  } catch (error) {
    return error as Error;
  }
  throw new Error("renderArgv rendered what it should have refused");
}

describe("the client's validation of options", () => {
  test("refuses an unknown option and a malformed one by name, before any transport call", async () => {
    const transport = new Recording();
    const client = new Client({ transport });
    const unknown = await caught(() =>
      client.status("surfaces", { queues: "x" } as unknown as { format: "json" }),
    );
    expect(unknown).toBeInstanceOf(BusRefused);
    expect((unknown as Error).message).toBe("`queues` is not an option of status");
    const malformed = await caught(() => client.schemaGen({ id: "not an id", lang: "json" }));
    expect(malformed).toBeInstanceOf(BusRefused);
    expect((malformed as Error).message).toStartWith("schemaGen: `id`");
    expect(transport.calls).toEqual([]);
  });

  test("applies the client's config, transportDir and registry only where a call leaves them out", async () => {
    const transport = new Recording();
    const client = new Client({
      config: { config: "default.yaml", transportDir: "default-dir", registry: "default-registry" },
      transport,
    });
    await caught(() => client.status("surfaces", { registry: "mine" }));
    await caught(() => client.transports());
    expect(transport.calls[0]?.args).toEqual({
      queue: "surfaces",
      registry: "mine",
      config: "default.yaml",
      transportDir: "default-dir",
    });
    expect(transport.calls[1]?.args).toEqual({});
  });
});

describe("the parity gate's reading of the client", () => {
  test("finds exactly one method per capability, and no other", async () => {
    const gate = await import("../../../scripts/sdk-coverage.mjs");
    const defined: Set<string> = gate.definedMethods("typescript", join(PACKAGE, "src/client.ts"));
    expect([...defined].sort()).toEqual([...METHODS].sort());
    expect(readFileSync(join(PACKAGE, "src/client.ts"), "utf8")).toContain("export class Client {");
  });
});
