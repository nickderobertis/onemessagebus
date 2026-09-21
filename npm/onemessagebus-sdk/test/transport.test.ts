// The transports' own behaviour: what the resident does with a shared connection,
// who stops a resident, and how a bus that cannot be reached is reported.
import { afterAll, describe, expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { chmodSync, existsSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  BusRefused,
  Client,
  CliTransport,
  ResidentTransport,
  TransportError,
} from "../src/index.js";
import {
  BINARY,
  baseConfig,
  caught,
  Greeting,
  QUESTION,
  removeScratch,
  scratch,
  socketPath,
} from "./support.js";

afterAll(removeScratch);

describe("the resident transport", () => {
  test("starts a resident, shares one connection among concurrent calls and a subscription, and stops only what it started", async () => {
    const dir = scratch("resident-shared");
    const socket = socketPath();
    const config = baseConfig(dir);
    const owner = new Client({ config, transport: new ResidentTransport({ socket }) });
    // The configuration types `greetings` by a schema registered at run time; until
    // it is, the resident refuses every request that reads the configuration.
    await owner.schema.register(Greeting);
    const subscription = (async () => {
      const seen = [];
      for await (const line of owner.subscribe("questions", {
        until: { field: "message", equals: "5" },
        timeout: 20,
      })) {
        seen.push(line);
      }
      return seen;
    })();
    const sent = await Promise.all(
      [1, 2, 3, 4, 5].map((n) => owner.send("questions", { ...QUESTION, message: String(n) })),
    );
    expect(sent.flat()).toHaveLength(5);
    // the subscription ends when the fifth arrives, whichever order the sends landed in
    expect((await subscription).length).toBeGreaterThan(0);

    // A second client attaches to the running resident without starting its own.
    const guest = new Client({
      config,
      transport: new ResidentTransport({ socket, start: false }),
    });
    expect((await guest.status("questions"))[0]?.records).toBeGreaterThanOrEqual(5);
    await guest.transport.close();
    expect(existsSync(socket)).toBe(true);
    expect((await owner.status("questions"))[0]?.queue).toBe("questions");

    await owner[Symbol.asyncDispose]();
    expect(existsSync(socket)).toBe(false);
    // a closed transport reconnects on its next call, starting a resident again
    const again = await owner.transports();
    expect(again.length).toBeGreaterThan(0);
    await owner.transport.close();
  });

  test("with start: false and nothing listening, a call is a TransportError naming how to start one", async () => {
    const socket = socketPath();
    const client = new Client({
      config: { binary: BINARY },
      transport: new ResidentTransport({ socket, start: false }),
    });
    const error = await caught(() => client.transports());
    expect(error).toBeInstanceOf(TransportError);
    expect(error.message).toContain(`serve --resident --socket ${socket}`);
  });

  test("a resident that refuses to start is reported with its own words", async () => {
    const dir = scratch("resident-refused");
    const client = new Client({
      config: { binary: BINARY, config: join(dir, "missing.yaml"), cwd: dir },
      transport: new ResidentTransport({ socket: socketPath() }),
    });
    const error = await caught(() => client.transports());
    expect(error).toBeInstanceOf(TransportError);
    expect(error.message).toContain("before listening");
    expect(error.message).toContain("missing.yaml");
  });

  test("a socket path no unix socket address can hold is refused up front", () => {
    const socket = `/tmp/${"x".repeat(120)}.sock`;
    expect(() => new ResidentTransport({ socket })).toThrow(TransportError);
    expect(() => new ResidentTransport({ socket })).toThrow("longer than the 103");
  });

  test("a refusal over the resident is the same typed error the command line gives", async () => {
    const dir = scratch("resident-refusal");
    const client = new Client({
      config: baseConfig(dir),
      transport: new ResidentTransport({ socket: socketPath() }),
    });
    try {
      await client.schema.register(Greeting);
      const error = await caught(() => client.send("nowhere", QUESTION));
      expect(error).toBeInstanceOf(BusRefused);
      expect(error.message).toContain("`nowhere` is not a queue this configuration declares");
    } finally {
      await client.transport.close();
    }
  });
});

describe("the CLI transport", () => {
  test("a binary that cannot be started is a TransportError, for a call and for a stream", async () => {
    const missing = join(scratch("cli-missing"), "onemessagebus");
    const transport = new CliTransport({ binary: missing });
    const call = await caught(() => transport.call("transports", {}));
    expect(call).toBeInstanceOf(TransportError);
    expect(call.message).toContain(`could not start ${missing}`);
    const stream = await caught(async () => {
      for await (const _ of transport.stream("subscribe", { queue: "q", until: "{}" })) {
        // nothing starts
      }
    });
    expect(stream).toBeInstanceOf(TransportError);
    await transport.close();
  });

  test("a transport's own configuration wins over the client's", async () => {
    const transport = new CliTransport({ binary: BINARY });
    transport.configure({ binary: "/elsewhere/onemessagebus", cwd: "/" });
    expect(transport.binary.command).toBe(BINARY);
    const kinds = await transport.call("transports", {});
    expect(Array.isArray(kinds)).toBe(true);
  });

  test("a payload the verb refuses before reading it is answered by the refusal, not a broken pipe", async () => {
    const dir = scratch("epipe");
    const client = new Client({ config: baseConfig(dir) });
    await client.schema.register(Greeting);
    // Far more than a pipe holds, so the write outlives the process that refused it.
    const error = await caught(() => client.send("nowhere", { text: "x".repeat(4 * 1024 * 1024) }));
    expect(error).toBeInstanceOf(BusRefused);
    expect(error.message).toContain("`nowhere` is not a queue this configuration declares");
  });
});

const sleep = (ms: number) => new Promise((wake) => setTimeout(wake, ms));

describe("a bus that cannot be reached, or stops answering", () => {
  test("a resident whose binary cannot start is a TransportError naming it", async () => {
    const missing = join(scratch("resident-missing"), "onemessagebus");
    const transport = new ResidentTransport({ socket: socketPath(), config: { binary: missing } });
    const error = await caught(() => transport.call("transports", {}));
    expect(error).toBeInstanceOf(TransportError);
    expect(error.message).toContain(`could not start ${missing}`);
    await transport.close();
  });

  test("a resident stopped under a running subscription ends it with a TransportError, and exits cleanly", async () => {
    const dir = scratch("resident-stops");
    const socket = socketPath();
    const config = baseConfig(dir);
    // A resident this test starts and owns by pid, as another process would run one.
    const resident = spawn(
      BINARY,
      [
        "serve",
        "--resident",
        "--socket",
        socket,
        "--config",
        String(config.config),
        "--registry",
        String(config.registry),
      ],
      { cwd: dir, stdio: "ignore" },
    );
    const exited = new Promise<number | null>((settle) => resident.once("exit", settle));
    try {
      const client = new Client({
        config,
        transport: new ResidentTransport({ socket, start: false }),
      });
      for (let tries = 0; !existsSync(socket); tries += 1) {
        expect(tries).toBeLessThan(800);
        await sleep(25);
      }
      await client.schema.register(Greeting, "demo.greeting@1");
      const subscription = caught(async () => {
        for await (const _ of client.subscribe("actions", {
          until: { field: "x", present: true },
          timeout: 20,
        })) {
          // nothing arrives on actions
        }
      });
      await sleep(300);
      unlinkSync(socket);
      const error = await subscription;
      expect(error).toBeInstanceOf(TransportError);
      expect(error.message).toContain("closed the connection");
      expect(await exited).toBe(0);
      await client.transport.close();
    } finally {
      if (resident.exitCode === null) {
        rmSync(socket, { force: true });
        await exited;
      }
    }
  }, 60_000);
});

/** An executable at `path` running `body`: the built binary, reached another way. */
function wrapper(path: string, body: string): string {
  writeFileSync(path, `#!/bin/sh\n${body}\n`);
  chmodSync(path, 0o755);
  return path;
}

describe("who owns a resident", () => {
  test("a resident another client started first is used and left running", async () => {
    const dir = scratch("resident-race");
    const socket = socketPath();
    const config = baseConfig(dir);
    const spawned = join(dir, "late-resident-spawned");
    // The built binary, except that `serve` first says it was spawned and pauses, so
    // another client's resident takes the socket between the late spawn and its connect.
    const late = wrapper(
      join(dir, "late-onemessagebus"),
      `if [ "$1" = serve ]; then touch "${spawned}"; sleep 2; fi\nexec "${BINARY}" "$@"`,
    );
    const ownerTransport = new ResidentTransport({ socket });
    const lateTransport = new ResidentTransport({ socket });
    const owner = new Client({ config, transport: ownerTransport });
    const lateClient = new Client({
      config: { ...config, binary: late },
      transport: lateTransport,
    });
    try {
      const answering = lateClient.transports({ format: "text" });
      for (let tries = 0; !existsSync(spawned); tries += 1) {
        expect(tries).toBeLessThan(1500);
        await sleep(20);
      }
      expect(await owner.transports({ format: "text" })).toContain("local");
      expect(ownerTransport.started).toBe(true);
      expect(await answering).toContain("local");
      expect(lateTransport.started).toBe(false);

      await lateTransport.close();
      expect(existsSync(socket)).toBe(true);
      expect(await owner.transports({ format: "text" })).toContain("local");

      await ownerTransport.close();
      expect(existsSync(socket)).toBe(false);
    } finally {
      await lateTransport.close();
      await ownerTransport.close();
    }
  }, 60_000);

  test("a resident a launcher runs as its own child is still the transport's to stop", async () => {
    const dir = scratch("resident-launcher");
    const socket = socketPath();
    // As the npm launcher does: the binary runs as the launcher's child, not in its place.
    const launcher = wrapper(
      join(dir, "launcher-onemessagebus"),
      `"${BINARY}" "$@"\nstatus=$?\nexit "$status"`,
    );
    const transport = new ResidentTransport({ socket });
    const client = new Client({ config: { ...baseConfig(dir), binary: launcher }, transport });
    try {
      expect(await client.transports({ format: "text" })).toContain("local");
      expect(transport.started).toBe(true);
      await transport.close();
      expect(existsSync(socket)).toBe(false);
    } finally {
      await transport.close();
    }
  }, 60_000);
});

/** Whether a process is still there; signal 0 delivers nothing. */
function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/**
 * A resident double: it listens on its socket, records its pid beside it as a
 * resident does, answers every request with an empty list, and ignores both the
 * removal of its socket and SIGTERM.
 */
const STUBBORN_RESIDENT = [
  'const { writeFileSync } = require("node:fs");',
  'const { createServer } = require("node:net");',
  'const { createInterface } = require("node:readline");',
  'process.on("SIGTERM", () => {});',
  'const socket = process.argv[process.argv.indexOf("--socket") + 1];',
  "const server = createServer((connection) => {",
  '  createInterface({ input: connection }).on("line", (line) => {',
  '    connection.write(JSON.stringify({ id: JSON.parse(line).id, ok: [] }) + "\\n");',
  "  });",
  "});",
  'server.listen(socket, () => writeFileSync(socket + ".pid", process.pid + "\\n"));',
  "",
].join("\n");

describe("a started resident that will not stop", () => {
  test("one that outlives its socket's removal and SIGTERM is killed when the transport closes", async () => {
    const dir = scratch("resident-stubborn");
    const socket = socketPath();
    const script = join(dir, "resident.cjs");
    writeFileSync(script, STUBBORN_RESIDENT);
    // The real binary answers the version check, so the double is the pinned version.
    const binary = wrapper(
      join(dir, "onemessagebus"),
      `if [ "$1" = --version ]; then exec "${BINARY}" --version; fi\nexec node "${script}" "$@"`,
    );
    const transport = new ResidentTransport({ socket });
    const client = new Client({ config: { binary }, transport });
    let pid: number | undefined;
    try {
      expect(await client.transports()).toEqual([]);
      expect(transport.started).toBe(true);
      pid = Number.parseInt(await Bun.file(`${socket}.pid`).text(), 10);
      expect(alive(pid)).toBe(true);
      await transport.close();
      expect(alive(pid)).toBe(false);
      expect(existsSync(socket)).toBe(false);
    } finally {
      if (pid !== undefined && alive(pid)) process.kill(pid, "SIGKILL");
      await transport.close();
    }
  }, 60_000);

  test("one that never listens is killed, and the call names the wait it outlasted", async () => {
    const dir = scratch("resident-silent");
    const socket = socketPath();
    const recorded = join(dir, "pid");
    const binary = wrapper(
      join(dir, "onemessagebus"),
      `if [ "$1" = --version ]; then exec "${BINARY}" --version; fi\necho $$ > "${recorded}"\nexec sleep 30`,
    );
    const transport = new ResidentTransport({ socket, startTimeout: 300 });
    const client = new Client({ config: { binary }, transport });
    let pid: number | undefined;
    try {
      const error = await caught(() => client.transports());
      expect(error).toBeInstanceOf(TransportError);
      expect(error.message).toContain(
        `the resident started on ${socket} did not listen within 300ms`,
      );
      pid = Number.parseInt(await Bun.file(recorded).text(), 10);
      for (let tries = 0; alive(pid); tries += 1) {
        expect(tries).toBeLessThan(200);
        await sleep(10);
      }
      expect(transport.started).toBe(false);
    } finally {
      if (pid !== undefined && alive(pid)) process.kill(pid, "SIGKILL");
      await transport.close();
    }
  }, 60_000);
});
