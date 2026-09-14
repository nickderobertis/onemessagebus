// How a call reaches the bus. The client builds a capability's options and parses
// what comes back; a transport carries one to the other and maps a refusal to its
// typed error. Two ship here — a subprocess per call, and the resident core over a
// unix socket — and a native binding is a third implementation of the same seam.
import { type ChildProcess, execFile, spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { unlink } from "node:fs/promises";
import { createConnection, type Socket } from "node:net";
import { resolve } from "node:path";
import { createInterface } from "node:readline";
import {
  type Binary,
  type ClientConfig,
  childEnv,
  describeBinary,
  resolveBinary,
} from "./binary.js";
import { BusError, BusRefused, ContractError, refusal, TransportError } from "./errors.js";
import { CAPABILITIES, type CapabilityMethod } from "./generated/capabilities.js";
import {
  type BusResidentProtocolV1,
  BusResidentProtocolV1Schema,
} from "./generated/messages/bus-resident-protocol-1.js";

/** A capability's options, keyed as its options root keys them. */
export type Args = Readonly<Record<string, unknown>>;

/**
 * The seam between the client and the bus.
 *
 * `call` answers what the verb printed: the document of a `json` verb, the list of
 * line documents of a `jsonl` verb, and the text of a `text` verb or of any verb
 * asked for `format: "text"`. A refusal rejects with `BusFailed` (exit 1) or
 * `BusRefused` (exit 2), carrying the bus's own words and any document printed
 * before it. `stream` yields a streaming verb's lines as they arrive.
 */
export interface Transport {
  /** The binary the client checks the version of before its first call; absent for a backend that runs none. */
  readonly binary?: Binary | undefined;
  /** The client's configuration, for a transport that starts processes of its own. */
  configure?(config: ClientConfig): void;
  call(capability: CapabilityMethod, args: Args, input?: string): Promise<unknown>;
  stream(capability: CapabilityMethod, args: Args, input?: string): AsyncIterable<unknown>;
  close(): Promise<void>;
}

/** Whether `name` is a capability this build's manifest declares (its own key, not an inherited one). */
export function isCapability(name: string): name is CapabilityMethod {
  return Object.hasOwn(CAPABILITIES, name);
}

/** What went wrong, from whatever was thrown. */
function reason(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function scalar(method: string, option: string, value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  throw new BusRefused(
    `${method}: \`${option}\` takes a string, a number or a boolean, not ${Array.isArray(value) ? "an array" : typeof value}`,
  );
}

/**
 * The argv after the binary for `capability` with `args`, rendered from the
 * manifest's bindings. Positionals go last, after `--`, so a value that starts
 * with a dash is still a positional; an absent or null option renders nothing.
 */
export function renderArgv(capability: string, args: Args): string[] {
  if (!isCapability(capability)) {
    throw new BusRefused(`\`${capability}\` is not a capability of onemessagebus`);
  }
  const declared = CAPABILITIES[capability];
  const bindings: readonly { option: string; flag: string; kind: string }[] = declared.bindings;
  for (const key of Object.keys(args)) {
    if (!bindings.some((binding) => binding.option === key)) {
      throw new BusRefused(`\`${key}\` is not an option of ${capability}`);
    }
  }
  const flags: string[] = [];
  const positionals: string[] = [];
  for (const { option, flag, kind } of bindings) {
    const value = args[option];
    if (value === undefined || value === null) continue;
    switch (kind) {
      case "positional":
        for (const item of Array.isArray(value) ? value : [value]) {
          positionals.push(scalar(capability, option, item));
        }
        break;
      case "value":
        flags.push(flag, scalar(capability, option, value));
        break;
      case "repeated":
        for (const item of Array.isArray(value) ? value : [value]) {
          flags.push(flag, scalar(capability, option, item));
        }
        break;
      case "switch":
        if (typeof value !== "boolean") {
          throw new BusRefused(
            `${capability}: \`${option}\` is true or false, not ${typeof value}`,
          );
        }
        if (value) flags.push(flag);
        break;
      case "key-value":
        if (typeof value !== "object" || Array.isArray(value)) {
          throw new BusRefused(
            `${capability}: \`${option}\` is an object of keys to values, not ${Array.isArray(value) ? "an array" : typeof value}`,
          );
        }
        for (const [key, item] of Object.entries(value)) {
          flags.push(flag, `${key}=${scalar(capability, `${option}.${key}`, item)}`);
        }
        break;
      default:
        throw new ContractError(
          `${capability}: the manifest binds \`${option}\` with an unknown kind ${kind}`,
        );
    }
  }
  return [...declared.verb, ...flags, ...(positionals.length > 0 ? ["--", ...positionals] : [])];
}

/** Whether `capability` with `args` prints text rather than JSON. */
export function printsText(capability: CapabilityMethod, args: Args): boolean {
  return CAPABILITIES[capability].stdout === "text" || args.format === "text";
}

/** The refusal on stderr: its last `onemessagebus: ` line, or all of it for a usage report. */
function refusalText(stderr: string, code: number): string {
  const lines = stderr.split("\n").filter((line) => line.trim() !== "");
  const last = lines.at(-1);
  if (last?.startsWith("onemessagebus: ")) return last.slice("onemessagebus: ".length);
  return stderr.trim() || `onemessagebus exited ${code} and said nothing on stderr`;
}

function parseJson(capability: CapabilityMethod, text: string): unknown {
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new ContractError(
      `${capability}: the binary printed ${JSON.stringify(text.slice(0, 200))}, which is not JSON (${reason(error)})`,
    );
  }
}

/** Stdout as the capability's shape says to read it. */
function readStdout(capability: CapabilityMethod, args: Args, stdout: string): unknown {
  if (printsText(capability, args)) return stdout;
  if (CAPABILITIES[capability].stdout === "jsonl") {
    return stdout
      .split("\n")
      .filter((line) => line.trim() !== "")
      .map((line) => parseJson(capability, line));
  }
  return parseJson(capability, stdout);
}

function spawnFailure(binary: Binary, error: unknown): TransportError {
  return new TransportError(
    `could not start ${describeBinary(binary)}: ${reason(error)}; install onemessagebus-cli, or name the binary with ClientConfig.binary or ONEMESSAGEBUS_BIN`,
    error,
  );
}

/** The bus as a subprocess: one `onemessagebus` per call, its argv rendered from the manifest. */
export class CliTransport implements Transport {
  #config: ClientConfig;
  #binary: Binary | undefined;

  constructor(config: ClientConfig = {}) {
    this.#config = config;
  }

  get binary(): Binary {
    this.#binary ??= resolveBinary(this.#config);
    return this.#binary;
  }

  /** The client's configuration fills in whatever this transport was not given itself. */
  configure(config: ClientConfig): void {
    this.#config = { ...config, ...definedOnly(this.#config) };
    this.#binary = undefined;
  }

  #spawn(capability: CapabilityMethod, args: Args, input: string | undefined): ChildProcess {
    const binary = this.binary;
    // A verb that may read stdin reads it whenever it is not a terminal, so with no
    // input it is handed /dev/null rather than a pipe nobody writes or closes.
    const child = spawn(binary.command, [...binary.prefix, ...renderArgv(capability, args)], {
      cwd: this.#config.cwd,
      env: childEnv(this.#config),
      stdio: [input === undefined ? "ignore" : "pipe", "pipe", "pipe"],
    });
    if (input !== undefined && child.stdin) {
      // A verb may refuse before reading stdin (an undeclared queue); its exit is the answer.
      child.stdin.on("error", () => {});
      child.stdin.end(input);
    }
    return child;
  }

  call(capability: CapabilityMethod, args: Args, input?: string): Promise<unknown> {
    return new Promise((resolvePromise, reject) => {
      let child: ChildProcess;
      try {
        child = this.#spawn(capability, args, input);
      } catch (error) {
        reject(error instanceof BusError ? error : spawnFailure(this.binary, error));
        return;
      }
      const stdout: Buffer[] = [];
      const stderr: Buffer[] = [];
      child.stdout?.on("data", (chunk: Buffer) => stdout.push(chunk));
      child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk));
      let failed = false;
      child.once("error", (error) => {
        failed = true;
        reject(spawnFailure(this.binary, error));
      });
      child.once("close", (code, signal) => {
        if (failed) return;
        const out = Buffer.concat(stdout).toString("utf8");
        const err = Buffer.concat(stderr).toString("utf8");
        try {
          if (code === 0) {
            resolvePromise(readStdout(capability, args, out));
          } else if (code === null) {
            reject(new BusError(`onemessagebus ${capability} was ended by ${signal}`, {}));
          } else {
            let output: unknown;
            if (out.trim() !== "") {
              try {
                output = readStdout(capability, args, out);
              } catch {
                output = out;
              }
            }
            reject(refusal(code, refusalText(err, code), output));
          }
        } catch (error) {
          reject(error);
        }
      });
    });
  }

  async *stream(capability: CapabilityMethod, args: Args, input?: string): AsyncGenerator<unknown> {
    const child = this.#spawn(capability, args, input);
    const stderr: Buffer[] = [];
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk));
    const ended = new Promise<{ code: number | null; error?: Error }>((settle) => {
      child.once("error", (error) => settle({ code: null, error }));
      child.once("close", (code) => settle({ code }));
    });
    // A binary that never started has a stdout that never ends; know before reading it.
    const started = await new Promise<Error | undefined>((settle) => {
      child.once("spawn", () => settle(undefined));
      child.once("error", (error) => settle(error));
    });
    if (started !== undefined) throw spawnFailure(this.binary, started);
    const textual = printsText(capability, args);
    const { stdout } = child;
    if (stdout === null) {
      throw new TransportError(`onemessagebus ${capability} started with no stdout pipe to read`);
    }
    const lines = createInterface({ input: stdout, crlfDelay: Infinity });
    let finished = false;
    try {
      for await (const line of lines) {
        if (line.trim() === "") continue;
        yield textual ? line : parseJson(capability, line);
      }
      const { code, error } = await ended;
      finished = true;
      if (error) throw spawnFailure(this.binary, error);
      if (code !== 0) {
        const text = Buffer.concat(stderr).toString("utf8");
        throw code === null
          ? new BusError(`onemessagebus ${capability} was ended by a signal`, {})
          : refusal(code, refusalText(text, code));
      }
    } finally {
      lines.close();
      // Iteration stopped early: the process this stream started is this stream's to end.
      if (!finished && child.exitCode === null && child.signalCode === null) {
        child.kill();
        await ended;
      }
    }
  }

  async close(): Promise<void> {}
}

/** `config` without the keys it leaves undefined, so a spread does not erase another's. */
function definedOnly(config: ClientConfig): ClientConfig {
  return Object.fromEntries(Object.entries(config).filter(([, value]) => value !== undefined));
}

/** Lines one streaming request has produced and nobody has read yet. */
class Lines {
  #items: unknown[] = [];
  #ended = false;
  #error: Error | undefined;
  #wake: (() => void) | undefined;

  push(item: unknown): void {
    this.#items.push(item);
    this.#notify();
  }

  end(): void {
    this.#ended = true;
    this.#notify();
  }

  fail(error: Error): void {
    this.#error = error;
    this.#notify();
  }

  #notify(): void {
    const wake = this.#wake;
    this.#wake = undefined;
    wake?.();
  }

  /** The next line, or `done` once the request ended; rejects once it failed. */
  async next(): Promise<IteratorResult<unknown>> {
    while (true) {
      if (this.#items.length > 0) return { done: false, value: this.#items.shift() };
      if (this.#error) throw this.#error;
      if (this.#ended) return { done: true, value: undefined };
      await new Promise<void>((wake) => {
        this.#wake = wake;
      });
    }
  }
}

type Pending =
  | { readonly kind: "call"; resolve(value: unknown): void; reject(error: Error): void }
  | { readonly kind: "stream"; readonly lines: Lines };

export interface ResidentTransportOptions {
  /** The unix socket the resident core listens on. */
  readonly socket: string;
  /** Start `serve --resident` on the socket when nothing answers there; true by default. */
  readonly start?: boolean | undefined;
  /** How long a started resident has to begin listening, in milliseconds. */
  readonly startTimeout?: number | undefined;
  /** The binary, and the configuration a started resident runs with; the client's fills in the rest. */
  readonly config?: ClientConfig | undefined;
}

/** The longest path a unix socket address holds on the platforms with the smallest one. */
const SOCKET_PATH_LIMIT = 103;

/** What the resident core's opening options were, when this transport started it. */
const RESIDENT_OPTIONS = ["config", "transportDir", "registry"] as const;
type OpeningOptions = Partial<Record<(typeof RESIDENT_OPTIONS)[number], string>>;

/**
 * The bus as the resident core: one connection to `serve --resident`, every call a
 * request line on it, demultiplexed by id so concurrent calls and a running
 * subscription share the connection.
 */
export class ResidentTransport implements Transport {
  readonly socket: string;
  readonly #start: boolean;
  readonly #startTimeout: number;
  #config: ClientConfig;
  #connection: Promise<Socket> | undefined;
  /** The connection once open, for the synchronous ref-counting a pending request needs. */
  #socket: Socket | undefined;
  #started: ChildProcess | undefined;
  #startedWith: OpeningOptions = {};
  #nextId = 1;
  readonly #pending = new Map<number, Pending>();

  constructor(options: ResidentTransportOptions) {
    this.#config = options.config ?? {};
    this.socket = resolve(this.#config.cwd ?? process.cwd(), options.socket);
    this.#start = options.start ?? true;
    this.#startTimeout = options.startTimeout ?? 20_000;
    if (Buffer.byteLength(this.socket) > SOCKET_PATH_LIMIT) {
      throw new TransportError(
        `the socket path ${this.socket} is ${Buffer.byteLength(this.socket)} bytes, longer than the ${SOCKET_PATH_LIMIT} a unix socket address holds; choose a shorter path`,
      );
    }
  }

  get binary(): Binary {
    return resolveBinary(this.#config);
  }

  /** Whether the resident answering on the socket is one this transport started, and `close` stops. */
  get started(): boolean {
    return this.#started !== undefined;
  }

  configure(config: ClientConfig): void {
    this.#config = { ...config, ...definedOnly(this.#config) };
  }

  async call(capability: CapabilityMethod, args: Args, input?: string): Promise<unknown> {
    const socket = await this.#connect();
    const id = this.#nextId++;
    return new Promise((resolvePromise, reject) => {
      this.#pending.set(id, { kind: "call", resolve: resolvePromise, reject });
      this.#write(socket, this.#request(id, capability, args, input));
    });
  }

  async *stream(capability: CapabilityMethod, args: Args, input?: string): AsyncGenerator<unknown> {
    const socket = await this.#connect();
    const id = this.#nextId++;
    const lines = new Lines();
    this.#pending.set(id, { kind: "stream", lines });
    this.#write(socket, this.#request(id, capability, args, input));
    let done = false;
    try {
      while (true) {
        const next = await lines.next();
        if (next.done) {
          done = true;
          return;
        }
        yield next.value;
      }
    } catch (error) {
      done = true;
      throw error;
    } finally {
      // Left early: cancel the request, and wait for the resident to say it stopped.
      if (!done && this.#pending.has(id) && !socket.destroyed) {
        this.#write(socket, { id, cancel: true });
        try {
          while (!(await lines.next()).done) {
            // lines already on their way before the cancel are nobody's now
          }
        } catch {
          // a connection that closed meanwhile has stopped the request too
        }
      }
    }
  }

  async close(): Promise<void> {
    const connection = this.#connection;
    this.#connection = undefined;
    const socket = await connection?.catch(() => undefined);
    socket?.destroy();
    const child = this.#started;
    this.#started = undefined;
    if (child && child.exitCode === null && child.signalCode === null) {
      const exited = new Promise<void>((settle) => child.once("exit", () => settle()));
      // Removing its socket is how a resident is asked to stop; it exits once it notices.
      try {
        await unlink(this.socket);
      } catch {
        // already gone: the resident noticed first, or someone else removed it
      }
      let timer: NodeJS.Timeout | undefined;
      const patience = new Promise<void>((settle) => {
        timer = setTimeout(settle, 5_000);
      });
      await Promise.race([exited, patience]);
      clearTimeout(timer);
      // One that ignored its socket's removal is ended the only other way there is.
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGKILL");
        await exited;
      }
    }
  }

  #request(id: number, capability: CapabilityMethod, args: Args, input: string | undefined) {
    // A request naming none of the resident's own opening options runs over the
    // transport it holds open; one restating them would have it resolve them again.
    const trimmed: Record<string, unknown> = { ...args };
    for (const key of RESIDENT_OPTIONS) {
      if (this.#startedWith[key] !== undefined && trimmed[key] === this.#startedWith[key]) {
        delete trimmed[key];
      }
    }
    return {
      id,
      verb: capability,
      ...(Object.keys(trimmed).length > 0 ? { args: trimmed } : {}),
      ...(input === undefined ? {} : { input }),
    };
  }

  #write(socket: Socket, line: BusResidentProtocolV1): void {
    socket.ref();
    socket.write(`${JSON.stringify(line)}\n`);
  }

  #settle(id: number): Pending | undefined {
    const pending = this.#pending.get(id);
    this.#pending.delete(id);
    if (this.#pending.size === 0) {
      // Nothing is waiting: an idle connection does not keep the process alive.
      this.#socket?.unref();
    }
    return pending;
  }

  #failAll(error: Error): void {
    for (const id of [...this.#pending.keys()]) {
      const pending = this.#settle(id);
      if (pending?.kind === "call") pending.reject(error);
      else pending?.lines.fail(error);
    }
  }

  #receive(text: string): void {
    if (text.trim() === "") return;
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch {
      this.#failAll(
        new ContractError(
          `the resident on ${this.socket} wrote a line that is not JSON: ${text.slice(0, 200)}`,
        ),
      );
      return;
    }
    const parsed = BusResidentProtocolV1Schema.safeParse(value);
    const id = typeof value === "object" && value !== null && "id" in value ? value.id : undefined;
    if (!parsed.success || typeof id !== "number") {
      const error =
        parsed.success && "error" in parsed.data
          ? new BusError(
              `the resident on ${this.socket} refused a line this SDK wrote: ${parsed.data.error.message}`,
              { exit: parsed.data.error.exit },
            )
          : new ContractError(
              `the resident on ${this.socket} wrote a line bus.resident-protocol@1 does not admit: ${text.slice(0, 200)}`,
            );
      const pending = typeof id === "number" ? this.#settle(id) : undefined;
      if (pending?.kind === "call") pending.reject(error);
      else if (pending) pending.lines.fail(error);
      else this.#failAll(error);
      return;
    }
    const line = parsed.data;
    if ("event" in line) {
      const pending = this.#pending.get(id);
      if (pending?.kind === "stream") pending.lines.push(line.event);
      return;
    }
    if ("ok" in line) {
      const pending = this.#settle(id);
      if (pending?.kind === "call") pending.resolve(line.ok);
      else pending?.lines.end();
      return;
    }
    if ("error" in line) {
      const failure = refusal(line.error.exit, line.error.message, line.error.output);
      const pending = this.#settle(id);
      if (pending?.kind === "call") pending.reject(failure);
      else pending?.lines.fail(failure);
    }
  }

  #connect(): Promise<Socket> {
    this.#connection ??= this.#open().catch((error: unknown) => {
      this.#connection = undefined;
      throw error;
    });
    return this.#connection;
  }

  async #open(): Promise<Socket> {
    let socket: Socket;
    try {
      socket = await attach(this.socket);
    } catch (error) {
      const code = error instanceof Error && "code" in error ? error.code : undefined;
      if (!this.#start || (code !== "ENOENT" && code !== "ECONNREFUSED")) {
        throw new TransportError(
          `nothing answers on ${this.socket} (${reason(error)}); start a resident with \`onemessagebus serve --resident --socket ${this.socket}\`, or let this transport start one with start: true`,
          error,
        );
      }
      socket = await this.#startResident();
    }
    socket.setEncoding("utf8");
    const lines = createInterface({ input: socket, crlfDelay: Infinity });
    lines.on("line", (line) => this.#receive(line));
    socket.once("close", () => {
      lines.close();
      this.#connection = undefined;
      this.#socket = undefined;
      this.#failAll(
        new TransportError(
          `the resident on ${this.socket} closed the connection; the next call reconnects`,
        ),
      );
    });
    socket.unref();
    this.#socket = socket;
    return socket;
  }

  async #startResident(): Promise<Socket> {
    const binary = this.binary;
    const argv = ["serve", "--resident", "--socket", this.socket];
    const startedWith: OpeningOptions = {};
    for (const [key, flag] of [
      ["config", "--config"],
      ["transportDir", "--transport-dir"],
      ["registry", "--registry"],
    ] as const) {
      const value = this.#config[key];
      if (value !== undefined) {
        argv.push(flag, value);
        startedWith[key] = value;
      }
    }
    const child = spawn(binary.command, [...binary.prefix, ...argv], {
      cwd: this.#config.cwd,
      env: childEnv(this.#config),
      stdio: ["ignore", "ignore", "pipe"],
    });
    const stderr: Buffer[] = [];
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk));
    let exit: { code: number | null; error?: Error } | undefined;
    child.once("error", (error) => {
      exit = { code: null, error };
    });
    child.once("exit", (code) => {
      exit ??= { code };
    });
    // A resident outlives any one call; it is this transport's `close` that ends it.
    child.unref();
    // Node's pipe is a socket that can be unreferenced; a runtime whose pipe is not keeps it.
    const { stderr: pipe } = child;
    if (pipe !== null && "unref" in pipe && typeof pipe.unref === "function") pipe.unref();
    const deadline = Date.now() + this.#startTimeout;
    while (true) {
      try {
        const socket = await attach(this.socket);
        // Something answers, but another client's resident may have won the socket
        // while this one was starting: only the resident that recorded itself is ours.
        if (await owns(this.socket, child, deadline)) {
          this.#started = child;
          this.#startedWith = startedWith;
        }
        return socket;
      } catch {
        // not listening yet
      }
      if (exit?.error) throw spawnFailure(binary, exit.error);
      if (exit !== undefined) {
        const said = Buffer.concat(stderr).toString("utf8");
        throw new TransportError(
          `${describeBinary(binary)} ${argv.join(" ")} exited ${exit.code} before listening: ${refusalText(said, exit.code ?? 1)}`,
        );
      }
      if (Date.now() > deadline) {
        child.kill("SIGKILL");
        throw new TransportError(
          `the resident started on ${this.socket} did not listen within ${this.#startTimeout}ms; run \`${describeBinary(binary)} ${argv.join(" ")}\` to see why`,
        );
      }
      await pause(25);
    }
  }
}

/** The live pid `<socket>.pid` records, or undefined while it records none (or a gone one). */
function recordedOwner(socket: string): number | undefined {
  let pid: number;
  try {
    pid = Number.parseInt(readFileSync(`${socket}.pid`, "utf8").trim(), 10);
  } catch {
    return undefined;
  }
  if (!Number.isInteger(pid) || pid <= 0) return undefined;
  try {
    // Signal 0 delivers nothing; it answers whether the process is there.
    process.kill(pid, 0);
    return pid;
  } catch {
    return undefined;
  }
}

/** The parent of process `pid`, as `ps` reports it; undefined when it cannot say. */
function parentOf(pid: number): Promise<number | undefined> {
  return new Promise((settle) => {
    execFile("ps", ["-o", "ppid=", "-p", String(pid)], (error, stdout) => {
      const parent = Number.parseInt(stdout.trim(), 10);
      settle(error === null && Number.isInteger(parent) ? parent : undefined);
    });
  });
}

/**
 * Whether the resident answering on `socket` is the one `child` started. A resident
 * binds its socket before it records its pid beside it, so a connection can reach
 * the winner of a start race before its pid is on disk; the loser exits naming the
 * winner. The answer waits for the pid file to name a live owner — `child` itself,
 * or the binary a launcher `child` runs as its own child — for `child` to exit, or
 * for the start deadline, after which nothing else claimed the socket.
 */
async function owns(socket: string, child: ChildProcess, deadline: number): Promise<boolean> {
  while (true) {
    const owner = recordedOwner(socket);
    if (owner !== undefined) {
      return owner === child.pid || (await parentOf(owner)) === child.pid;
    }
    if (child.exitCode !== null || child.signalCode !== null) return false;
    if (Date.now() > deadline) return true;
    await pause(20);
  }
}

/** Resolves after `ms` milliseconds. */
function pause(ms: number): Promise<void> {
  return new Promise((wake) => setTimeout(wake, ms));
}

function attach(path: string): Promise<Socket> {
  return new Promise((resolvePromise, reject) => {
    const socket = createConnection({ path });
    socket.once("connect", () => {
      resolvePromise(socket);
    });
    // The listener stays: an error on an open connection is a no-op here and ends in
    // its close event, which the transport handles, instead of an unhandled error.
    socket.on("error", reject);
  });
}
