// Message types declared in TypeScript, and the ones the Rust registry already holds.
//
// A definition is an id and a Zod schema. The SDK validates a payload by it before
// sending, and parses a claimed record by it; the core validates the same payload
// again, in Rust, against the JSON Schema registered under the id — which is the
// schema this definition renders.
import { z } from "zod";
import { BusFailed } from "./errors.js";
import { SchemaIdSchema } from "./generated/roots/schema-id.js";

export type JsonSchemaDocument = Record<string, unknown>;

/** A message type: what a queue's records are, by id. */
export interface MessageDefinition<T> {
  readonly id: string;
  readonly schema: z.ZodType<T>;
  /** The canonical draft 2020-12 JSON Schema registered under `id`. */
  jsonSchema(): JsonSchemaDocument;
  /** `value` as a `T`, or a `BusFailed` naming the id and the JSON pointer of the first violation. */
  parse(value: unknown): T;
}

/** The type a definition describes: `type Greeting = MessageType<typeof Greeting>`. */
export type MessageType<D> = D extends MessageDefinition<infer T> ? T : never;

function pointer(path: readonly PropertyKey[]): string {
  return path.map((key) => `/${String(key).replaceAll("~", "~0").replaceAll("/", "~1")}`).join("");
}

/** The words a violation is reported in, the way the core reports its own: `<id>: at <pointer>: <why>`. */
export function violation(id: string, error: z.ZodError): string {
  const issue = error.issues[0];
  const at = issue === undefined ? "" : pointer(issue.path);
  return `${id}: at ${at === "" ? "/" : at}: ${issue?.message ?? "invalid"}`;
}

/** Refuses an id the core's `SchemaId` would not parse, by the generated schema of that type. */
export function assertSchemaId(id: string): void {
  if (!SchemaIdSchema.safeParse(id).success) {
    throw new TypeError(
      `${JSON.stringify(id)} is not a schema id; an id is <namespace>.<name>@<version>, e.g. demo.greeting@1`,
    );
  }
}

function definition<T>(
  id: string,
  schema: z.ZodType<T>,
  jsonSchema: () => JsonSchemaDocument,
): MessageDefinition<T> {
  assertSchemaId(id);
  return Object.freeze({
    id,
    schema,
    jsonSchema,
    parse(value: unknown): T {
      const parsed = schema.safeParse(value);
      if (!parsed.success) throw new BusFailed(violation(id, parsed.error));
      return parsed.data;
    },
  });
}

/**
 * Declare a message type: `defineMessage("demo.greeting@1", z.object({ text: z.string() }))`.
 * Throws a `TypeError` for a malformed id.
 */
export function defineMessage<T>(id: string, schema: z.ZodType<T>): MessageDefinition<T> {
  const title = id.slice(0, id.indexOf("@"));
  return definition(id, schema, () => ({
    ...z.toJSONSchema(schema, { target: "draft-2020-12" }),
    title,
  }));
}

/** A message the Rust registry holds, with the document it is registered under. Used by generated code. */
export function registeredMessage<T>(
  id: string,
  schema: z.ZodType<T>,
  document: JsonSchemaDocument,
): MessageDefinition<T> {
  return definition(id, schema, () => structuredClone(document));
}

/** Whether `value` is a message definition rather than a plain JSON Schema document. */
export function isMessageDefinition(value: unknown): value is MessageDefinition<unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    "id" in value &&
    typeof value.id === "string" &&
    "jsonSchema" in value &&
    typeof value.jsonSchema === "function" &&
    "parse" in value &&
    typeof value.parse === "function"
  );
}

/**
 * What a `type` option names, as a definition: a message type as it is, and a bare
 * Zod schema as one with no id, whose violations are reported as the payload's.
 */
export function messageOf<T>(
  type: MessageDefinition<T> | z.ZodType<T>,
): Pick<MessageDefinition<T>, "id" | "schema" | "parse"> {
  if (!(type instanceof z.ZodType)) return type;
  const schema = type;
  return {
    id: "payload",
    schema,
    parse(value: unknown): T {
      const parsed = schema.safeParse(value);
      if (!parsed.success) throw new BusFailed(violation("payload", parsed.error));
      return parsed.data;
    },
  };
}
