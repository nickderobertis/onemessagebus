// The Zod generator's cases, run in a process of their own by tooling.test.ts: the
// generator is a build script, not SDK source, so its behaviour is tested from the
// outside and it never enters the SDK's coverage report. Prints one JSON document.
import { z } from "zod";
import { expression, UnsupportedSchema, zodDeclarations } from "../scripts/zod-generator.mjs";

/** Schemas the generator must refuse, each by the words it must refuse them in. */
const REFUSED = [
  [{ type: "string", format: "email" }, "root/format: the keyword `format`"],
  [
    { type: "array", uniqueItems: true, items: true },
    "root/uniqueItems: the keyword `uniqueItems`",
  ],
  [{ $ref: "other.json#/x" }, "outside the document's $defs"],
  [{ const: "a", type: "integer" }, "is not of type integer"],
  [{ oneOf: [], type: "object" }, "an empty branch list"],
  [{ type: "string", pattern: "(?<" }, "not a JavaScript regular expression"],
  [{ type: "object", additionalProperties: false, oneOf: [true] }, "a closed object"],
];

const refusals = REFUSED.map(([schema, expected]) => {
  try {
    expression(schema, "root");
    return { expected, refused: false };
  } catch (error) {
    return {
      expected,
      refused: error instanceof UnsupportedSchema,
      named: error.message.includes(expected),
      message: error.message,
    };
  }
});

const module = zodDeclarations(
  {
    type: "object",
    properties: {
      name: { type: "string", minLength: 1, maxLength: 2 },
      count: { type: ["integer", "null"], format: "uint64", minimum: 0 },
      any: {},
      tagged: { oneOf: [{ const: "a" }, { const: "b" }] },
      map: { type: "object", additionalProperties: { $ref: "#/$defs/Word" } },
    },
    required: ["name", "any"],
    additionalProperties: false,
    $defs: { Word: { enum: ["x", "y"] } },
  },
  { exportName: "ThingSchema", typeName: "unknown", at: "thing" },
);
// The emitted TypeScript, as JavaScript: its type annotations and the root's cast removed.
const source = `${module
  .replace("export const", "const")
  .replaceAll(": z.ZodType =", " =")
  .replace(/ as unknown as z\.ZodType<unknown>;$/u, ";")}\nreturn ThingSchema;`;
const union = (branches) => (branches.length === 1 ? branches[0] : z.union(branches));
const schema = new Function("z", "anyOf", "oneOf", source)(z, union, union);

const parses = [
  { name: "😀😀", any: null, count: 3, map: { k: "x" } },
  { name: "", any: 1 },
  { name: "abc", any: 1 },
  { name: "a" },
  { name: "a", any: 1, count: -1 },
  { name: "a", any: 1, extra: true },
  { name: "a", any: 1, map: { k: "z" } },
].map((value) => schema.safeParse(value).success);

process.stdout.write(JSON.stringify({ refusals, parses }));
