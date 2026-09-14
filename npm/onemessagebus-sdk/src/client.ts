// The client: exactly one method per capability, named as the manifest names it.
//
// A method turns its arguments into the capability's options, applies the
// client's defaults, validates them by the generated options schema, hands them to
// the transport, and parses what comes back by the generated output schema. The
// argv and the resident request are the transport's, rendered from the manifest.
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { z } from "zod";
import { type ClientConfig, verifyVersion } from "./binary.js";
import { BusFailed, BusRefused, ContractError } from "./errors.js";
import {
  CAPABILITIES,
  type CapabilityMethod,
  type CapabilityOutputs,
  OPTION_SCHEMAS,
  OUTPUT_SCHEMAS,
} from "./generated/capabilities.js";
import type { AskOptions } from "./generated/options/ask-options.js";
import type { DeliverOptions } from "./generated/options/deliver-options.js";
import type { EventsEmitOptions } from "./generated/options/events-emit-options.js";
import type { EventsMergeOptions } from "./generated/options/events-merge-options.js";
import type { InboxCarriedOptions } from "./generated/options/inbox-carried-options.js";
import type { NextOptions } from "./generated/options/next-options.js";
import type { ReplyOptions } from "./generated/options/reply-options.js";
import type { SchemaCheckOptions } from "./generated/options/schema-check-options.js";
import type { SchemaGenOptions } from "./generated/options/schema-gen-options.js";
import type { SchemaListOptions } from "./generated/options/schema-list-options.js";
import type { SchemaRegisterOptions } from "./generated/options/schema-register-options.js";
import type { SendOptions } from "./generated/options/send-options.js";
import type { ServeOptions } from "./generated/options/serve-options.js";
import type { StatusOptions } from "./generated/options/status-options.js";
import type { SubscribeOptions } from "./generated/options/subscribe-options.js";
import type { TransportsOptions } from "./generated/options/transports-options.js";
import type { ValidateOptions } from "./generated/options/validate-options.js";
import type { Asked } from "./generated/roots/asked.js";
import type { CarriedEntry } from "./generated/roots/carried-entry.js";
import type { Claimed as ClaimedRecord } from "./generated/roots/claimed.js";
import type { CodecResponse } from "./generated/roots/codec-response.js";
import type { Disposition } from "./generated/roots/disposition.js";
import type { Envelope } from "./generated/roots/envelope.js";
import type { LogRecord } from "./generated/roots/log-record.js";
import type { QueueStatuses } from "./generated/roots/queue-statuses.js";
import type { Replied } from "./generated/roots/replied.js";
import type { SchemaList } from "./generated/roots/schema-list.js";
import type { Sent } from "./generated/roots/sent.js";
import type { TransportKinds } from "./generated/roots/transport-kinds.js";
import type { Validated } from "./generated/roots/validated.js";
import {
  assertSchemaId,
  isMessageDefinition,
  type JsonSchemaDocument,
  type MessageDefinition,
  messageOf,
  violation,
} from "./message.js";
import { CliTransport, printsText, type Transport } from "./transport.js";

/** Options asking for the text rendering: the method resolves a `string`. */
export type AsText<O> = O & { format: "text" };
/** Options asking for JSON, or leaving the format to its default: the method resolves the document. */
export type AsJson<O> = O & { format?: "json" | null | undefined };

/** The record `next` claimed, its record typed by the message type it was claimed as. */
export type Claimed<T = unknown> = Omit<ClaimedRecord, "record"> & { record: T };
export type QueueStatus = QueueStatuses[number];
export type Reply = Extract<Asked, { answer: "reply" }>;
export type Timeout = Extract<Asked, { answer: "timeout" }>;
export type Abandoned = Extract<Asked, { answer: "abandoned" }>;
export type Refused = Extract<Asked, { answer: "refused" }>;
/** What `ask` answered, named in `answer`; only a reply carries a `reply`. */
export type Answer = Reply | Timeout | Abandoned | Refused;

/** A predicate: inline JSON text, a path to a YAML document, or the predicate itself. */
export type Predicate = string | Readonly<Record<string, unknown>>;

type Without<O, K extends keyof O> = Omit<O, K>;
/** A message type: a `defineMessage` result or a generated one, or a bare Zod schema. */
export type MessageTypeLike<T> = MessageDefinition<T> | z.ZodType<T>;
type Typed<T> = { readonly type?: MessageTypeLike<T> | undefined };
type NextIn = Without<NextOptions, "queue">;
type StatusIn = Without<StatusOptions, "queue">;
type SubscribeIn = Without<SubscribeOptions, "queue" | "until"> & { until: Predicate };

export interface ClientOptions {
  readonly config?: ClientConfig | undefined;
  /** How calls reach the bus; a `CliTransport` over `config` when absent. */
  readonly transport?: Transport | undefined;
}

/** The options the client's configuration supplies when a call leaves them out. */
const DEFAULTED = ["config", "transportDir", "registry"] as const;

/** A payload as the bus reads it on stdin: JSON text. */
function payload(value: unknown): string | undefined {
  return value === undefined ? undefined : JSON.stringify(value);
}

function describeIssues(method: string, error: z.ZodError): string {
  const issue = error.issues[0];
  if (issue?.code === "unrecognized_keys") {
    return `\`${issue.keys.join("`, `")}\` is not an option of ${method}`;
  }
  const at = issue?.path.map(String).join(".") ?? "";
  return `${method}: \`${at}\` ${issue?.message ?? "is invalid"}`;
}

/** One document (or one line) a capability printed, parsed by its generated output schema. */
function parseOutput<M extends CapabilityMethod>(method: M, value: unknown): CapabilityOutputs[M] {
  const schema = OUTPUT_SCHEMAS[method];
  if (schema === null) {
    throw new ContractError(
      `${method}: the manifest gives it no output contract to read by`,
      value,
    );
  }
  const parsed = schema.safeParse(value);
  if (!parsed.success) {
    throw new ContractError(
      `${method}: the binary's output does not match the generated ${CAPABILITIES[method].output} contract — ${violation(CAPABILITIES[method].output ?? method, parsed.error)}`,
      value,
    );
  }
  return parsed.data;
}

/** The lines a `jsonl` capability printed, each parsed by its generated output schema. */
function parseLines<M extends CapabilityMethod>(method: M, value: unknown): CapabilityOutputs[M][] {
  if (!Array.isArray(value)) {
    throw new ContractError(
      `${method}: expected the lines the verb printed, and the bus answered ${JSON.stringify(value)}`,
      value,
    );
  }
  return value.map((line) => parseOutput(method, line));
}

function expectText(method: string, value: unknown): string {
  if (typeof value !== "string") {
    throw new ContractError(
      `${method}: expected text from the bus, and it answered ${JSON.stringify(value)}`,
    );
  }
  return value;
}

export class Client {
  /** Registering, checking and listing schemas, by message type or by id. */
  readonly schema: SchemaApi;
  /** How this client's calls reach the bus; `close()` it, or dispose of the client, when done. */
  readonly transport: Transport;
  readonly #config: ClientConfig;
  #verified: Promise<void> | undefined;

  constructor(options: ClientOptions = {}) {
    this.#config = options.config ?? {};
    this.transport = options.transport ?? new CliTransport(this.#config);
    this.transport.configure?.(this.#config);
    this.schema = new SchemaApi(this);
  }

  schemaList(options: AsText<SchemaListOptions>): Promise<string>;
  schemaList(options?: AsJson<SchemaListOptions>): Promise<SchemaList>;
  schemaList(options?: SchemaListOptions): Promise<SchemaList | string>;
  async schemaList(options: SchemaListOptions = {}): Promise<SchemaList | string> {
    return this.#reading("schemaList", options);
  }

  async schemaCheck(options: SchemaCheckOptions, payloadValue?: unknown): Promise<string> {
    return this.#text("schemaCheck", options, payload(payloadValue));
  }

  async schemaGen(options: SchemaGenOptions): Promise<string> {
    return this.#text("schemaGen", options);
  }

  async schemaRegister(options: SchemaRegisterOptions): Promise<string> {
    return this.#text("schemaRegister", options);
  }

  eventsMerge(options: AsText<EventsMergeOptions>): Promise<string>;
  eventsMerge(options: AsJson<EventsMergeOptions>): Promise<Envelope[]>;
  eventsMerge(options: EventsMergeOptions): Promise<Envelope[] | string>;
  async eventsMerge(options: EventsMergeOptions): Promise<Envelope[] | string> {
    return this.#readingLines("eventsMerge", options);
  }

  eventsEmit(options: AsText<EventsEmitOptions>, payloadValue?: unknown): Promise<string>;
  eventsEmit(options: AsJson<EventsEmitOptions>, payloadValue?: unknown): Promise<Envelope>;
  eventsEmit(options: EventsEmitOptions, payloadValue?: unknown): Promise<Envelope | string>;
  async eventsEmit(options: EventsEmitOptions, payloadValue?: unknown): Promise<Envelope | string> {
    return this.#reading("eventsEmit", options, payload(payloadValue));
  }

  async deliver(options: DeliverOptions, message?: unknown): Promise<Disposition> {
    return this.#document("deliver", options, payload(message));
  }

  inboxCarried(options: AsText<InboxCarriedOptions>): Promise<string>;
  inboxCarried(options: AsJson<InboxCarriedOptions>): Promise<CarriedEntry[]>;
  inboxCarried(options: InboxCarriedOptions): Promise<CarriedEntry[] | string>;
  async inboxCarried(options: InboxCarriedOptions): Promise<CarriedEntry[] | string> {
    return this.#readingLines("inboxCarried", options);
  }

  async send<T>(
    queue: string,
    message: T | undefined,
    options: Without<SendOptions, "queue"> & Typed<T> = {},
  ): Promise<Sent[]> {
    const { type, ...rest } = options;
    const body =
      type === undefined || message === undefined ? message : messageOf(type).parse(message);
    return this.#lines("send", { ...rest, queue }, payload(body));
  }

  next(queue: string, options: AsText<NextIn>): Promise<string | undefined>;
  next<T>(
    queue: string,
    options: AsJson<NextIn> & { type: MessageTypeLike<T> },
  ): Promise<Claimed<T> | undefined>;
  next(queue: string, options?: AsJson<NextIn>): Promise<Claimed | undefined>;
  next(queue: string, options?: NextIn): Promise<Claimed | string | undefined>;
  async next<T>(
    queue: string,
    options: NextIn & Typed<T> = {},
  ): Promise<Claimed<T> | Claimed | string | undefined> {
    const { type, ...rest } = options;
    let claimed: ClaimedRecord | string;
    try {
      claimed = await this.#reading("next", { ...rest, queue });
    } catch (error) {
      // The one refusal that is an answer: an empty queue has nothing to hand out.
      if (error instanceof BusFailed && error.message === `nothing on ${queue} to claim`) {
        return undefined;
      }
      throw error;
    }
    if (typeof claimed === "string" || type === undefined) return claimed;
    const definition = messageOf(type);
    const parsed = definition.schema.safeParse(claimed.record);
    if (!parsed.success) {
      throw new ContractError(
        `next: the record claimed from ${queue} is not a ${definition.id}: ${violation(definition.id, parsed.error)}`,
        claimed,
      );
    }
    return { ...claimed, record: parsed.data };
  }

  async reply(
    queue: string,
    correlation: string | undefined,
    reply: unknown,
    options: Without<ReplyOptions, "queue" | "correlation"> = {},
  ): Promise<Replied> {
    return this.#document("reply", { ...options, queue, correlation }, payload(reply));
  }

  subscribe(queue: string, options: AsText<SubscribeIn>): AsyncGenerator<string>;
  subscribe(queue: string, options: AsJson<SubscribeIn>): AsyncGenerator<LogRecord>;
  subscribe(queue: string, options: SubscribeIn): AsyncGenerator<LogRecord | string>;
  async *subscribe(queue: string, options: SubscribeIn): AsyncGenerator<LogRecord | string> {
    await this.#verify();
    const { until, ...rest } = options;
    const args = this.#args("subscribe", {
      ...rest,
      queue,
      until: typeof until === "string" ? until : JSON.stringify(until),
    });
    const textual = printsText("subscribe", args);
    for await (const line of this.transport.stream("subscribe", args)) {
      yield textual ? expectText("subscribe", line) : parseOutput("subscribe", line);
    }
  }

  status(queue: string | undefined, options: AsText<StatusIn>): Promise<string>;
  status(queue?: string, options?: AsJson<StatusIn>): Promise<QueueStatus[]>;
  status(queue?: string, options?: StatusIn): Promise<QueueStatus[] | string>;
  async status(queue?: string, options: StatusIn = {}): Promise<QueueStatus[] | string> {
    return this.#reading("status", { ...options, queue });
  }

  transports(options: AsText<TransportsOptions>): Promise<string>;
  transports(options?: AsJson<TransportsOptions>): Promise<TransportKinds>;
  transports(options?: TransportsOptions): Promise<TransportKinds | string>;
  async transports(options: TransportsOptions = {}): Promise<TransportKinds | string> {
    return this.#reading("transports", options);
  }

  async validate(
    queue: string,
    message: unknown,
    options: Without<ValidateOptions, "queue"> = {},
  ): Promise<Validated> {
    try {
      return await this.#document("validate", { ...options, queue }, payload(message));
    } catch (error) {
      // A verdict that is not a pass exits 1 with the verdict printed: it is the answer.
      if (error instanceof BusFailed && error.output !== undefined) {
        return parseOutput("validate", error.output);
      }
      throw error;
    }
  }

  async ask(
    queue: string,
    question: unknown,
    options: Without<AskOptions, "queue"> = {},
  ): Promise<Answer> {
    try {
      return await this.#document("ask", { ...options, queue }, payload(question));
    } catch (error) {
      // A timeout, an abandoned question and a refusal exit 1 with the answer printed.
      if (error instanceof BusFailed && error.output !== undefined) {
        return parseOutput("ask", error.output);
      }
      throw error;
    }
  }

  async serve(
    options: ServeOptions,
    frames?: string | readonly unknown[],
  ): Promise<CodecResponse[]> {
    const input =
      typeof frames === "string" || frames === undefined
        ? frames
        : frames.map((frame) => `${JSON.stringify(frame)}\n`).join("");
    return this.#lines("serve", options, input);
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.transport.close();
  }

  #verify(): Promise<void> {
    const binary = this.transport.binary;
    if (binary === undefined) return Promise.resolve();
    this.#verified ??= verifyVersion(binary, this.#config).catch((error: unknown) => {
      this.#verified = undefined;
      throw error;
    });
    return this.#verified;
  }

  #args(method: CapabilityMethod, options: object): Record<string, unknown> {
    const args: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(options)) {
      if (value !== undefined && value !== null) args[key] = value;
    }
    const bindings: readonly { readonly option: string }[] = CAPABILITIES[method].bindings;
    for (const key of DEFAULTED) {
      const fallback = this.#config[key];
      const binds = bindings.some((binding) => binding.option === key);
      if (binds && args[key] === undefined && fallback !== undefined) args[key] = fallback;
    }
    const schema: z.ZodType = OPTION_SCHEMAS[method];
    const parsed = schema.safeParse(args);
    if (!parsed.success) throw new BusRefused(describeIssues(method, parsed.error));
    return args;
  }

  /** The call itself: the version checked, the options validated, the transport's raw answer. */
  async #invoke(
    method: CapabilityMethod,
    options: object,
    input: string | undefined,
  ): Promise<{ args: Record<string, unknown>; raw: unknown }> {
    await this.#verify();
    const args = this.#args(method, options);
    return { args, raw: await this.transport.call(method, args, input) };
  }

  /** A `json` capability's document, or its text when the options ask for it. */
  async #reading<M extends CapabilityMethod>(
    method: M,
    options: object,
    input?: string,
  ): Promise<CapabilityOutputs[M] | string> {
    const { args, raw } = await this.#invoke(method, options, input);
    return printsText(method, args) ? expectText(method, raw) : parseOutput(method, raw);
  }

  /** A `jsonl` capability's lines, or its text when the options ask for it. */
  async #readingLines<M extends CapabilityMethod>(
    method: M,
    options: object,
    input?: string,
  ): Promise<CapabilityOutputs[M][] | string> {
    const { args, raw } = await this.#invoke(method, options, input);
    return printsText(method, args) ? expectText(method, raw) : parseLines(method, raw);
  }

  /** A `json` capability that has no text rendering. */
  async #document<M extends CapabilityMethod>(
    method: M,
    options: object,
    input?: string,
  ): Promise<CapabilityOutputs[M]> {
    const { raw } = await this.#invoke(method, options, input);
    return parseOutput(method, raw);
  }

  /** A `jsonl` capability that has no text rendering. */
  async #lines<M extends CapabilityMethod>(
    method: M,
    options: object,
    input?: string,
  ): Promise<CapabilityOutputs[M][]> {
    const { raw } = await this.#invoke(method, options, input);
    return parseLines(method, raw);
  }

  /** A `text` capability's confirmation or rendering. */
  async #text(method: CapabilityMethod, options: object, input?: string): Promise<string> {
    const { raw } = await this.#invoke(method, options, input);
    return expectText(method, raw);
  }
}

/** `client.schema`: the registry, by message type or by id. */
export class SchemaApi {
  readonly #client: Client;

  constructor(client: Client) {
    this.#client = client;
  }

  /**
   * Register a message type's JSON Schema under its id, or a plain JSON Schema
   * document under `id`. Answers the id registered.
   */
  async register(
    messageOrSchema: MessageDefinition<unknown> | JsonSchemaDocument,
    id?: string,
  ): Promise<string> {
    const definition = isMessageDefinition(messageOrSchema) ? messageOrSchema : undefined;
    const schemaId = id ?? definition?.id;
    if (schemaId === undefined) {
      throw new TypeError(
        "a plain JSON Schema document is registered under an id; pass one as register(schema, id)",
      );
    }
    assertSchemaId(schemaId);
    const document = definition === undefined ? messageOrSchema : definition.jsonSchema();
    const dir = await mkdtemp(join(tmpdir(), "onemessagebus-schema-"));
    try {
      const file = join(dir, "schema.json");
      await writeFile(file, JSON.stringify(document));
      await this.#client.schemaRegister({ id: schemaId, file });
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
    return schemaId;
  }

  /** Resolves when `payload` conforms to the schema; rejects with `BusFailed` naming the pointer when not. */
  async check(
    idOrMessage: string | MessageDefinition<unknown>,
    payloadValue: unknown,
  ): Promise<void> {
    const id = typeof idOrMessage === "string" ? idOrMessage : idOrMessage.id;
    await this.#client.schemaCheck({ id }, payloadValue);
  }

  /** Every registered id. */
  async list(): Promise<SchemaList> {
    return this.#client.schemaList();
  }
}
