// The manifest is how a call is rendered: every binding of every capability must
// reach the command line from the client method that owns it.
import { afterAll, describe, expect, test } from "bun:test";
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
import { caught, removeScratch } from "./support.js";

afterAll(removeScratch);

/** A value for every option any capability binds, each one the option's schema admits. */
const VALUES = {
  queue: "surfaces",
  id: "demo.greeting@1",
  file: "record.json",
  registry: "registry",
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
  about: "task-7",
  sessionSeconds: 60,
  links: ["https://example.org/frames.json@8"],
};

class Stop extends Error {}

/** Records what the client hands the transport, and answers nothing. */
class Recording implements Transport {
  readonly calls: { capability: CapabilityMethod; args: Args }[] = [];
  async call(capability: CapabilityMethod, args: Args): Promise<unknown> {
    this.calls.push({ capability, args });
    throw new Stop();
  }
  /** A stream that fails before its first line, as one whose process never started does. */
  stream(capability: CapabilityMethod, args: Args): AsyncIterable<unknown> {
    this.calls.push({ capability, args });
    return {
      [Symbol.asyncIterator]: () => ({ next: () => Promise.reject(new Stop()) }),
    };
  }
  async close(): Promise<void> {}
}

/** Each method, called with every option its capability binds. */
const EVERY_OPTION: Record<CapabilityMethod, (client: Client, v: typeof VALUES) => unknown> = {
  schemaList: (c, v) => c.schemaList({ registry: v.registry, config: v.config, format: "json" }),
  schemaCheck: (c, v) =>
    c.schemaCheck({ id: v.id, file: v.file, registry: v.registry, config: v.config }),
  schemaGen: (c, v) =>
    c.schemaGen({ id: v.id, lang: "rust", registry: v.registry, config: v.config }),
  schemaRegister: (c, v) =>
    c.schemaRegister({ id: v.id, file: v.file, registry: v.registry, config: v.config }),
  schemas: (c) => c.schemas({ format: "json" }),
  schemasClear: (c) => c.schemasClear({ format: "json" }),
  schemasFetch: (c, v) => c.schemasFetch({ links: v.links, config: v.config, format: "json" }),
  eventsMerge: (c, v) =>
    c.eventsMerge({ files: v.files, filter: v.filter, profile: v.profile, format: "json" }),
  eventsEmit: (c, v) =>
    c.eventsEmit({
      path: v.path,
      kind: v.kind,
      stream: v.stream,
      source: v.source,
      profile: v.profile,
      labels: v.labels,
      file: v.file,
      format: "json",
    }),
  deliver: (c, v) =>
    c.deliver({ address: v.address, message: v.message, file: v.file, wait: v.wait }),
  inboxCarried: (c, v) => c.inboxCarried({ store: v.store, format: "json" }),
  send: (c, v) =>
    c.send(v.queue, undefined, {
      file: v.file,
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
    }),
  next: (c, v) =>
    c.next(v.queue, {
      consumer: v.consumer,
      asker: v.asker,
      format: "json",
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
    }),
  reply: (c, v) =>
    c.reply(v.queue, v.correlation, undefined, {
      position: v.position,
      file: v.file,
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
    }),
  subscribe: async (c, v) => {
    for await (const _ of c.subscribe(v.queue, {
      until: v.until,
      timeout: v.timeout,
      format: "json",
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
    })) {
      // the recording transport ends the stream before a line
    }
  },
  status: (c, v) =>
    c.status(v.queue, {
      format: "json",
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
    }),
  transports: (c) => c.transports({ format: "json" }),
  validate: (c, v) =>
    c.validate(v.queue, undefined, {
      file: v.file,
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
    }),
  ask: (c, v) =>
    c.ask(v.queue, undefined, {
      blocking: true,
      asker: v.asker,
      about: v.about,
      timeout: v.timeout,
      correlation: v.correlation,
      file: v.file,
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
    }),
  serve: (c, v) =>
    c.serve({
      queue: v.queue,
      codec: "onejudge",
      sessionSeconds: v.sessionSeconds,
      asker: v.asker,
      file: v.file,
      config: v.config,
      transportDir: v.transportDir,
      registry: v.registry,
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
    const kind: string = binding.kind;
    switch (kind) {
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
        if (typeof value !== "object" || value === null) {
          throw new Error(`${capability} passed \`${binding.option}\` as ${typeof value}`);
        }
        for (const [key, item] of Object.entries(value)) {
          const at = flags.findIndex(
            (word, i) => word === binding.flag && flags[i + 1] === `${key}=${item}`,
          );
          expect(at, `${capability} rendered no ${binding.flag} ${key}=${item}`).toBeGreaterThan(
            -1,
          );
        }
        break;
      default:
        throw new Error(`the manifest binds a kind this test does not know: ${kind}`);
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

  test("refuses a capability the manifest does not name, an option it does not bind, and a value no flag can carry", () => {
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
    // a JavaScript caller names capabilities by string, so the name is checked too
    expect(renderArgvError("publish", {}).message).toBe(
      "`publish` is not a capability of onemessagebus",
    );
    expect(renderArgvError("toString", {}).message).toBe(
      "`toString` is not a capability of onemessagebus",
    );
  });
});

function renderArgvError(capability: string, args: Args): Error {
  try {
    renderArgv(capability, args);
  } catch (error) {
    if (error instanceof Error) return error;
    throw error;
  }
  throw new Error("renderArgv rendered what it should have refused");
}

describe("the client's validation of options", () => {
  test("refuses an unknown option and a malformed one by name, before any transport call", async () => {
    const transport = new Recording();
    const client = new Client({ transport });
    // Options arriving as JSON, from outside TypeScript's view, as a config file's would.
    const unknownOption = await caught(() =>
      client.status("surfaces", JSON.parse('{"queues":"x"}')),
    );
    expect(unknownOption).toBeInstanceOf(BusRefused);
    expect(unknownOption.message).toBe("`queues` is not an option of status");
    const malformed = await caught(() => client.schemaGen({ id: "not an id", lang: "json" }));
    expect(malformed).toBeInstanceOf(BusRefused);
    expect(malformed.message).toStartWith("schemaGen: `id`");
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

describe("the client's public surface", () => {
  test("is exactly one method per capability, and no other", () => {
    const methods = Object.getOwnPropertyNames(Client.prototype).filter(
      (name) => name !== "constructor",
    );
    expect(methods.sort()).toEqual([...METHODS].sort());
  });
});
