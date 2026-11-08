// The Zod generator's cases, run in a process of their own by tooling.test.ts: the
// generator is a build script, not SDK source, so its behaviour is tested from the
// outside and it never enters the SDK's coverage report. Prints one JSON document.
import { z } from "zod";
import { typescriptDeclarations } from "../scripts/typescript-generator.mjs";
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
      old: { type: "string", description: "Superseded.", deprecated: true },
    },
    required: ["name", "any"],
    additionalProperties: false,
    $defs: { Word: { enum: ["x", "y"] } },
  },
  { exportName: "ThingSchema", typeName: "unknown", at: "thing" },
);
// The emitted TypeScript, as JavaScript: its type annotations and type arguments removed.
const source = `${module
  .replace("export const", "const")
  .replaceAll(": z.ZodType =", " =")
  .replace("contract<unknown>(", "contract(")}\nreturn ThingSchema;`;
const union = (branches) => (branches.length === 1 ? branches[0] : z.union(branches));
const contract = (typed) => typed;
const schema = new Function("z", "anyOf", "oneOf", "contract", source)(z, union, union, contract);

const parses = [
  { name: "😀😀", any: null, count: 3, map: { k: "x" } },
  { name: "", any: 1 },
  { name: "abc", any: 1 },
  { name: "a" },
  { name: "a", any: 1, count: -1 },
  { name: "a", any: 1, extra: true },
  { name: "a", any: 1, map: { k: "z" } },
  { name: "a", any: 1, old: "kept" },
  { name: "a", any: 1, old: 1 },
].map((value) => schema.safeParse(value).success);

// A member referring to a definition that admits anything is still required when
// the schema requires it, and still optional when it does not.
const referring = zodDeclarations(
  {
    type: "object",
    properties: { raw: { $ref: "#/$defs/Anything" }, maybe: { $ref: "#/$defs/Anything" } },
    required: ["raw"],
    additionalProperties: false,
    $defs: { Anything: { description: "Any JSON value." } },
  },
  { exportName: "ReferringSchema", typeName: "unknown", at: "referring" },
);
const referringSchema = new Function(
  "z",
  "anyOf",
  "oneOf",
  "contract",
  `${referring
    .replace("export const", "const")
    .replaceAll(": z.ZodType =", " =")
    .replace("contract<unknown>(", "contract(")}\nreturn ReferringSchema;`,
)(z, union, union, contract);
const references = [{ raw: null }, { raw: 1, maybe: "x" }, { maybe: 1 }, { raw: "x" }].map(
  (value) => referringSchema.safeParse(value).success,
);

// `deprecated` constrains nothing, so it is carried to the declarations as a tag.
const declarations = await typescriptDeclarations(
  {
    type: "object",
    properties: { old: { type: "string", description: "Superseded.", deprecated: true } },
  },
  "Thing",
);

process.stdout.write(JSON.stringify({ refusals, parses, references, declarations }));
