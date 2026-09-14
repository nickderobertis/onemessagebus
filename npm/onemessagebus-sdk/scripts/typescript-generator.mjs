// TypeScript declarations from the bundle's JSON Schema documents, through
// json-schema-to-typescript, with the two adjustments this package's compiler
// settings need.
import { compile } from "json-schema-to-typescript";

const METADATA = new Set(["$defs", "$schema", "description", "default", "examples", "title"]);

/**
 * A schema that says nothing but metadata admits any JSON value; the compiler
 * would otherwise render it as an open object, which a string reply is not.
 */
function unknownWhereUnconstrained(schema) {
  if (!schema || typeof schema !== "object" || Array.isArray(schema)) return schema;
  const out = { ...schema };
  for (const keyword of ["properties", "$defs", "patternProperties"]) {
    if (schema[keyword] && typeof schema[keyword] === "object") {
      out[keyword] = Object.fromEntries(
        Object.entries(schema[keyword]).map(([name, value]) => [
          name,
          unknownWhereUnconstrained(value),
        ]),
      );
    }
  }
  for (const keyword of ["oneOf", "anyOf", "allOf"]) {
    if (Array.isArray(schema[keyword]))
      out[keyword] = schema[keyword].map(unknownWhereUnconstrained);
  }
  if (schema.items && !Array.isArray(schema.items))
    out.items = unknownWhereUnconstrained(schema.items);
  if (schema.additionalProperties && typeof schema.additionalProperties === "object") {
    out.additionalProperties = unknownWhereUnconstrained(schema.additionalProperties);
  }
  if (Object.keys(schema).every((key) => METADATA.has(key))) out.tsType = "unknown";
  return out;
}

/**
 * `exactOptionalPropertyTypes` refuses `undefined` for `name?: T`; a caller
 * spreading options it may not have set passes exactly that, and the contract
 * says an absent option and an undefined one render the same nothing.
 */
function exactOptionalProperties(declarations) {
  const lines = declarations.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const property = /^(\s*)(?:readonly )?(?:[A-Za-z_$][A-Za-z0-9_$]*|"[^"]*")\?:\s/u.exec(
      lines[index],
    );
    if (!property) continue;
    const indentation = property[1];
    let end = index;
    while (true) {
      const candidate = lines[end];
      if (candidate === undefined) {
        throw new Error(
          `generated optional property has no terminator: ${lines[index]}; extend scripts/typescript-generator.mjs for this shape, then run \`bun run generate\``,
        );
      }
      const atIndent =
        candidate.startsWith(indentation) && !candidate.slice(indentation.length).startsWith(" ");
      if (atIndent && candidate.trimEnd().endsWith(";")) break;
      end += 1;
    }
    lines[end] = lines[end].replace(/;\s*$/u, " | undefined;");
    index = end;
  }
  return lines.join("\n");
}

/** The declarations of `document`, its root named `typeName`. */
export async function typescriptDeclarations(document, typeName) {
  const prepared = { ...unknownWhereUnconstrained(document), title: typeName };
  const source = await compile(prepared, typeName, {
    bannerComment: "",
    additionalProperties: true,
    declareExternallyReferenced: true,
    format: true,
    style: { printWidth: 100, endOfLine: "lf" },
    strictIndexSignatures: false,
    unknownAny: true,
  });
  return exactOptionalProperties(source.trim());
}
