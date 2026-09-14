// Zod source from the JSON Schema documents schemars writes.
//
// It covers exactly the constructs the bundle uses, and refuses every other one
// by keyword and JSON pointer: a keyword this emitter silently dropped would be a
// constraint the Rust core enforces and this SDK claims to, but does not.

/** Keywords that describe a schema and constrain nothing. */
const METADATA = new Set(["$schema", "title", "description", "default", "examples"]);

/** The integer formats schemars writes for Rust integers. */
const INTEGER_FORMATS = new Set([
  "int8",
  "int16",
  "int32",
  "int64",
  "int",
  "uint8",
  "uint16",
  "uint32",
  "uint64",
  "uint",
]);

/** Keywords that constrain one JSON type, for splitting a `type: [...]` list. */
const TYPE_KEYWORDS = {
  null: [],
  boolean: [],
  string: ["minLength", "maxLength", "pattern"],
  integer: ["format", "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"],
  number: ["format", "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"],
  array: ["items", "minItems", "maxItems"],
  object: ["properties", "required", "additionalProperties", "patternProperties"],
};

export class UnsupportedSchema extends Error {}

function refuse(at, what) {
  throw new UnsupportedSchema(
    `${at}: ${what}; extend scripts/zod-generator.mjs to enforce it (or change the Rust schema), then run \`just sdk-generate\``,
  );
}

function allow(schema, at, keywords) {
  for (const key of Object.keys(schema)) {
    if (!METADATA.has(key) && !keywords.includes(key)) {
      refuse(`${at}/${key}`, `the keyword \`${key}\` is not one this generator enforces here`);
    }
  }
}

function pointer(at, token) {
  return `${at}/${String(token).replaceAll("~", "~0").replaceAll("/", "~1")}`;
}

/** A JavaScript identifier for a `$defs` name, unique within one document. */
export function definitionIdentifier(name) {
  return `$${name.replace(/[^A-Za-z0-9_]/gu, "_")}`;
}

function regex(pattern, at) {
  try {
    new RegExp(pattern, "u");
  } catch (error) {
    refuse(
      at,
      `the pattern ${JSON.stringify(pattern)} is not a JavaScript regular expression (${error.message})`,
    );
  }
  return `new RegExp(${JSON.stringify(pattern)}, "u")`;
}

function numeric(schema, at, integer) {
  allow(schema, at, ["type", ...TYPE_KEYWORDS.integer]);
  if (schema.format !== undefined && schema.format !== "double" && schema.format !== "float") {
    if (!INTEGER_FORMATS.has(schema.format))
      refuse(`${at}/format`, `the number format ${schema.format}`);
  }
  let out = integer || INTEGER_FORMATS.has(schema.format) ? "z.int()" : "z.number()";
  if (schema.minimum !== undefined) out += `.gte(${schema.minimum})`;
  if (schema.maximum !== undefined) out += `.lte(${schema.maximum})`;
  if (schema.exclusiveMinimum !== undefined) out += `.gt(${schema.exclusiveMinimum})`;
  if (schema.exclusiveMaximum !== undefined) out += `.lt(${schema.exclusiveMaximum})`;
  return out;
}

function string(schema, at) {
  allow(schema, at, ["type", ...TYPE_KEYWORDS.string]);
  let out = "z.string()";
  if (schema.pattern !== undefined) out += `.regex(${regex(schema.pattern, `${at}/pattern`)})`;
  // JSON Schema counts code points; Zod's min/max count UTF-16 units, so an astral
  // character would count twice and refuse what Rust accepts.
  if (schema.minLength !== undefined) {
    out += `.refine((value) => [...value].length >= ${schema.minLength}, { message: "shorter than ${schema.minLength} characters" })`;
  }
  if (schema.maxLength !== undefined) {
    out += `.refine((value) => [...value].length <= ${schema.maxLength}, { message: "longer than ${schema.maxLength} characters" })`;
  }
  return out;
}

function array(schema, at) {
  allow(schema, at, ["type", ...TYPE_KEYWORDS.array]);
  if (schema.items === undefined || Array.isArray(schema.items)) {
    refuse(`${at}/items`, "an array needs exactly one item schema");
  }
  let out = `z.array(${expression(schema.items, `${at}/items`)})`;
  if (schema.minItems !== undefined) out += `.min(${schema.minItems})`;
  if (schema.maxItems !== undefined) out += `.max(${schema.maxItems})`;
  return out;
}

function object(schema, at) {
  allow(schema, at, ["type", ...TYPE_KEYWORDS.object]);
  const properties = schema.properties ?? {};
  const required = new Set(schema.required ?? []);
  for (const name of required) {
    if (!(name in properties))
      refuse(`${at}/required`, `the required property ${name} has no schema`);
  }
  const fields = Object.entries(properties).map(([name, property]) => {
    let field = expression(property, pointer(`${at}/properties`, name));
    if (!required.has(name)) field += ".optional()";
    // A schema that admits anything admits `undefined` too, so a missing key would pass.
    else if (field === "z.unknown()") {
      field += `.refine((value) => value !== undefined, { message: "required" })`;
    }
    return `${JSON.stringify(name)}: ${field}`;
  });
  const shape = `{ ${fields.join(", ")} }`;
  const extra = schema.additionalProperties;
  if (schema.patternProperties !== undefined) {
    const patterns = Object.entries(schema.patternProperties);
    if (patterns.length !== 1 || fields.length > 0 || extra !== false) {
      refuse(
        `${at}/patternProperties`,
        "only one pattern, with no declared properties and additionalProperties false, is enforced",
      );
    }
    const [[pattern, value]] = patterns;
    const key = `z.string().regex(${regex(pattern, `${at}/patternProperties`)})`;
    return `z.record(${key}, ${expression(value, pointer(`${at}/patternProperties`, pattern))})`;
  }
  if (extra === false) return `z.strictObject(${shape})`;
  if (extra === undefined || extra === true) return `z.looseObject(${shape})`;
  const values = expression(extra, `${at}/additionalProperties`);
  if (fields.length === 0) return `z.record(z.string(), ${values})`;
  return `z.object(${shape}).catchall(${values})`;
}

/** Keywords that shape an object beside a branch list. */
const OBJECT_BASE = ["type", "properties", "required", "additionalProperties"];

function branches(schema, at, keyword) {
  allow(schema, at, [keyword, ...OBJECT_BASE]);
  const members = schema[keyword];
  if (!Array.isArray(members) || members.length === 0)
    refuse(`${at}/${keyword}`, "an empty branch list");
  const list = members.map((member, index) => expression(member, `${at}/${keyword}/${index}`));
  const union = keyword === "oneOf" ? `oneOf([${list.join(", ")}])` : `anyOf([${list.join(", ")}])`;
  const base = Object.fromEntries(
    Object.entries(schema).filter(([key]) => OBJECT_BASE.includes(key)),
  );
  if (Object.keys(base).length === 0) return union;
  // schemars writes a struct with a flattened enum as the shared fields beside the
  // variants; JSON Schema ANDs them, so both must hold.
  if (base.type !== undefined && base.type !== "object") {
    refuse(`${at}/type`, `a ${keyword} beside type ${JSON.stringify(base.type)}`);
  }
  if (base.additionalProperties === false) {
    refuse(
      `${at}/additionalProperties`,
      `a closed object beside ${keyword} admits no branch's fields`,
    );
  }
  return `z.intersection(${object({ ...base, type: "object" }, at)}, ${union})`;
}

/** The Zod expression for `schema`, found at the JSON pointer `at`. */
export function expression(schema, at) {
  if (schema === true) return "z.unknown()";
  if (schema === false) return "z.never()";
  if (schema === null || typeof schema !== "object" || Array.isArray(schema)) {
    refuse(at, "a node that is not a schema");
  }
  if (schema.$ref !== undefined) {
    allow(schema, at, ["$ref"]);
    if (!schema.$ref.startsWith("#/$defs/"))
      refuse(`${at}/$ref`, `a reference outside the document's $defs (${schema.$ref})`);
    const name = schema.$ref.slice("#/$defs/".length).replaceAll("~1", "/").replaceAll("~0", "~");
    return `z.lazy(() => ${definitionIdentifier(name)})`;
  }
  if (schema.const !== undefined) {
    // schemars keeps a field's type, format and minimum beside its const. The
    // literal is the stricter constraint, so it stands alone — once it is known to
    // satisfy the rest, since a const they refuse admits nothing at all.
    allow(schema, at, ["const", "type", "format", "minimum", "maximum"]);
    const value = schema.const;
    const types = {
      string: "string",
      boolean: "boolean",
      integer: "number",
      number: "number",
      null: "object",
    };
    if (schema.type !== undefined && typeof value !== types[schema.type]) {
      refuse(`${at}/const`, `the const ${JSON.stringify(value)} is not of type ${schema.type}`);
    }
    if (schema.type === "integer" && !Number.isInteger(value))
      refuse(`${at}/const`, "a non-integer const of type integer");
    if (schema.minimum !== undefined && !(value >= schema.minimum))
      refuse(`${at}/const`, "a const below its minimum");
    if (schema.maximum !== undefined && !(value <= schema.maximum))
      refuse(`${at}/const`, "a const above its maximum");
    if (schema.format !== undefined && !INTEGER_FORMATS.has(schema.format)) {
      refuse(`${at}/format`, `the format ${schema.format} beside a const`);
    }
    return `z.literal(${JSON.stringify(value)})`;
  }
  if (schema.enum !== undefined) {
    allow(schema, at, ["enum", "type"]);
    const words = schema.enum.map((word) => `z.literal(${JSON.stringify(word)})`);
    return words.length === 1 ? words[0] : `z.union([${words.join(", ")}])`;
  }
  if (schema.oneOf !== undefined) return branches(schema, at, "oneOf");
  if (schema.anyOf !== undefined) return branches(schema, at, "anyOf");
  if (Array.isArray(schema.type)) {
    const claimed = new Set(["type", ...schema.type.flatMap((type) => TYPE_KEYWORDS[type] ?? [])]);
    allow(schema, at, [...claimed]);
    return `anyOf([${schema.type
      .map((type) => {
        if (!(type in TYPE_KEYWORDS)) refuse(`${at}/type`, `the type ${type}`);
        const member = Object.fromEntries(
          Object.entries(schema).filter(([key]) => TYPE_KEYWORDS[type].includes(key)),
        );
        return expression({ ...member, type }, at);
      })
      .join(", ")}])`;
  }
  switch (schema.type) {
    case "object":
      return object(schema, at);
    case "array":
      return array(schema, at);
    case "string":
      return string(schema, at);
    case "integer":
      return numeric(schema, at, true);
    case "number":
      return numeric(schema, at, false);
    case "boolean":
      allow(schema, at, ["type"]);
      return "z.boolean()";
    case "null":
      allow(schema, at, ["type"]);
      return "z.null()";
    case undefined:
      allow(schema, at, []);
      return "z.unknown()";
    default:
      return refuse(`${at}/type`, `the type ${schema.type}`);
  }
}

/**
 * The Zod declarations of one document: each of its `$defs` as a module-local
 * constant, then the root, exported as `exportName` and typed as `typeName`.
 */
export function zodDeclarations(document, { exportName, typeName, at }) {
  const lines = [];
  const defs = document.$defs ?? {};
  const seen = new Map();
  for (const name of Object.keys(defs)) {
    const identifier = definitionIdentifier(name);
    if (seen.has(identifier)) {
      refuse(
        `${at}/$defs`,
        `the definitions ${seen.get(identifier)} and ${name} both become ${identifier}`,
      );
    }
    seen.set(identifier, name);
    lines.push(
      `const ${identifier}: z.ZodType = ${expression(defs[name], pointer(`${at}/$defs`, name))};`,
    );
  }
  const { $defs: _defs, ...root } = document;
  lines.push(`export const ${exportName} = contract<${typeName}>(${expression(root, at)});`);
  return lines.join("\n\n");
}

/** The runtime the generated modules share: JSON Schema's two branch keywords. */
export const RUNTIME_MODULE = `import { z } from "zod";

type Branches = [z.ZodType, ...z.ZodType[]];

/** \`anyOf\`: at least one branch admits the value. */
export function anyOf(branches: Branches): z.ZodType {
  return branches.length === 1 ? branches[0] : z.union(branches);
}

/** \`oneOf\`: exactly one branch admits the value, as the Rust validator requires. */
export function oneOf(branches: Branches): z.ZodType {
  return anyOf(branches).superRefine((value, context) => {
    const admitted = branches.filter((branch) => branch.safeParse(value).success).length;
    if (admitted > 1) {
      context.addIssue({ code: "custom", message: \`matches \${admitted} oneOf branches, where exactly one must\` });
    }
  });
}

/**
 * A document's schema, typed as the declaration generated from the same document.
 * The declaration cannot be inferred from the schema — its \`$defs\` are lazily typed —
 * so the generated modules' one type assertion is here rather than in each of them.
 */
export function contract<T>(schema: z.ZodType): z.ZodType<T> {
  return schema as z.ZodType<T>; // sound: \`schema\` and \`T\` are generated from one JSON Schema document
}
`;
