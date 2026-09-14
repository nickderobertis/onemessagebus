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

/** A reading verb's result: text when the options ask for `format: "text"`. */
export type Reading<O, T> = O extends { format: "text" } ? string : T;

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

function parseOutput<M extends CapabilityMethod>(method: M, value: unknown): unknown {
  const schema = OUTPUT_SCHEMAS[method] as z.ZodType | null;
  if (schema === null) return value;
  const parsed = schema.safeParse(value);
  if (!parsed.success) {
    throw new ContractError(
      `${method}: the binary's output does not match the generated ${CAPABILITIES[method].output} contract — ${violation(CAPABILITIES[method].output ?? method, parsed.error)}`,
      value,
    );
  }
  return parsed.data;
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

  async schemaList<O extends SchemaListOptions>(options?: O): Promise<Reading<O, SchemaList>> {
    return (await this.#read("schemaList", options ?? {})) as Reading<O, SchemaList>;
  }

  async schemaCheck(options: SchemaCheckOptions, payloadValue?: unknown): Promise<string> {
    return expectText(
      "schemaCheck",
      await this.#read("schemaCheck", options, payload(payloadValue)),
    );
  }

  async schemaGen(options: SchemaGenOptions): Promise<string> {
    return expectText("schemaGen", await this.#read("schemaGen", options));
  }

  async schemaRegister(options: SchemaRegisterOptions): Promise<string> {
    return expectText("schemaRegister", await this.#read("schemaRegister", options));
  }

  async eventsMerge<O extends EventsMergeOptions>(options: O): Promise<Reading<O, Envelope[]>> {
    return (await this.#read("eventsMerge", options)) as Reading<O, Envelope[]>;
  }

  async eventsEmit<O extends EventsEmitOptions>(
    options: O,
    payloadValue?: unknown,
  ): Promise<Reading<O, Envelope>> {
    return (await this.#read("eventsEmit", options, payload(payloadValue))) as Reading<O, Envelope>;
  }

  async deliver(options: DeliverOptions, message?: unknown): Promise<Disposition> {
    return this.#read("deliver", options, payload(message));
  }

  async inboxCarried<O extends InboxCarriedOptions>(
    options: O,
  ): Promise<Reading<O, CarriedEntry[]>> {
    return (await this.#read("inboxCarried", options)) as Reading<O, CarriedEntry[]>;
  }

  async send<T>(
    queue: string,
    message: T | undefined,
    options: Without<SendOptions, "queue"> & Typed<T> = {},
  ): Promise<Sent[]> {
    const { type, ...rest } = options;
    const body =
      type === undefined || message === undefined ? message : messageOf(type).parse(message);
    return (await this.#read("send", { ...rest, queue }, payload(body))) as Sent[];
  }

  next(
    queue: string,
    options: Without<NextOptions, "queue" | "format"> & { format: "text" },
  ): Promise<string | undefined>;
  next<T>(
    queue: string,
    options: Without<NextOptions, "queue"> & { type: MessageTypeLike<T> },
  ): Promise<Claimed<T> | undefined>;
  next(queue: string, options?: Without<NextOptions, "queue">): Promise<Claimed | undefined>;
  async next<T>(
    queue: string,
    options: Without<NextOptions, "queue"> & Typed<T> = {},
  ): Promise<Claimed<T> | string | undefined> {
    const { type, ...rest } = options;
    let claimed: unknown;
    try {
      claimed = await this.#read("next", { ...rest, queue });
    } catch (error) {
      // The one refusal that is an answer: an empty queue has nothing to hand out.
      if (error instanceof BusFailed && error.message === `nothing on ${queue} to claim`) {
        return undefined;
      }
      throw error;
    }
    if (typeof claimed === "string" || type === undefined) return claimed as Claimed<T> | string;
    const record = (claimed as ClaimedRecord).record;
    const definition = messageOf(type);
    const parsed = definition.schema.safeParse(record);
    if (!parsed.success) {
      throw new ContractError(
        `next: the record claimed from ${queue} is not a ${definition.id}: ${violation(definition.id, parsed.error)}`,
        claimed,
      );
    }
    return { ...(claimed as ClaimedRecord), record: parsed.data };
  }

  async reply(
    queue: string,
    correlation: string | undefined,
    reply: unknown,
    options: Without<ReplyOptions, "queue" | "correlation"> = {},
  ): Promise<Replied> {
    return (await this.#read(
      "reply",
      { ...options, queue, correlation },
      payload(reply),
    )) as Replied;
  }

  subscribe(
    queue: string,
    options: Without<SubscribeOptions, "queue" | "until" | "format"> & {
      until: Predicate;
      format: "text";
    },
  ): AsyncGenerator<string>;
  subscribe(
    queue: string,
    options: Without<SubscribeOptions, "queue" | "until"> & { until: Predicate },
  ): AsyncGenerator<LogRecord>;
  async *subscribe(
    queue: string,
    options: Without<SubscribeOptions, "queue" | "until"> & { until: Predicate },
  ): AsyncGenerator<LogRecord | string> {
    await this.#verify();
    const { until, ...rest } = options;
    const args = this.#args("subscribe", {
      ...rest,
      queue,
      until: typeof until === "string" ? until : JSON.stringify(until),
    });
    const textual = printsText("subscribe", args);
    for await (const line of this.transport.stream("subscribe", args)) {
      yield textual ? expectText("subscribe", line) : (parseOutput("subscribe", line) as LogRecord);
    }
  }

  async status<O extends Without<StatusOptions, "queue">>(
    queue?: string,
    options?: O,
  ): Promise<Reading<O, QueueStatus[]>> {
    return (await this.#read("status", { ...options, queue })) as Reading<O, QueueStatus[]>;
  }

  async transports<O extends TransportsOptions>(options?: O): Promise<Reading<O, TransportKinds>> {
    return (await this.#read("transports", options ?? {})) as Reading<O, TransportKinds>;
  }

  async validate(
    queue: string,
    message: unknown,
    options: Without<ValidateOptions, "queue"> = {},
  ): Promise<Validated> {
    try {
      return (await this.#read("validate", { ...options, queue }, payload(message))) as Validated;
    } catch (error) {
      // A verdict that is not a pass exits 1 with the verdict printed: it is the answer.
      if (error instanceof BusFailed && error.output !== undefined) {
        return parseOutput("validate", error.output) as Validated;
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
      return (await this.#read("ask", { ...options, queue }, payload(question))) as Answer;
    } catch (error) {
      // A timeout, an abandoned question and a refusal exit 1 with the answer printed.
      if (error instanceof BusFailed && error.output !== undefined) {
        return parseOutput("ask", error.output) as Answer;
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
    return (await this.#read("serve", options, input)) as CodecResponse[];
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
    const bound = CAPABILITIES[method].bindings.map((binding) => binding.option as string);
    for (const key of DEFAULTED) {
      const fallback = this.#config[key];
      if (bound.includes(key) && args[key] === undefined && fallback !== undefined) {
        args[key] = fallback;
      }
    }
    const parsed = (OPTION_SCHEMAS[method] as z.ZodType).safeParse(args);
    if (!parsed.success) throw new BusRefused(describeIssues(method, parsed.error));
    return args;
  }

  async #read(method: CapabilityMethod, options: object, input?: string): Promise<unknown> {
    await this.#verify();
    const args = this.#args(method, options);
    const raw = await this.transport.call(method, args, input);
    if (printsText(method, args)) return expectText(method, raw);
    if (CAPABILITIES[method].stdout === "jsonl") {
      if (!Array.isArray(raw)) {
        throw new ContractError(
          `${method}: expected the lines the verb printed, and the bus answered ${JSON.stringify(raw)}`,
          raw,
        );
      }
      return raw.map((line) => parseOutput(method, line));
    }
    return parseOutput(method, raw);
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
