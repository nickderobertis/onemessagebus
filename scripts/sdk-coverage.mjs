#!/usr/bin/env node
// The SDK parity gate: every capability has a method in both language clients,
// and neither client has a method no capability names.
//
// The generate-checks compare the SDKs' generated contracts to the bundle, so
// they catch a shape that drifted; they cannot catch a method nobody wrote, or
// one left behind after its verb was removed. This does, in both directions.
//
// The expectation is derived, never listed: the bundle's `capabilities` —
// `onemessagebus::CAPABILITIES`, which `crates/onemessagebus-cli/tests/capability.rs`
// holds to the real clap tree — says what exists, and each client's own source
// says what it defines. Read rather than imported, so the gate needs no build of
// either package and a method that exists only as a type is not mistaken for one
// a caller can invoke.
//
// Usage: node scripts/sdk-coverage.mjs [typescript-client] [python-client]
// The paths default to the real clients; a candidate client can be named instead,
// which is how the gate is shown to go red without touching the real sources.
//
// Quiet on success, one line. On failure it names each missing or extra method
// and where to fix it.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { ROOT, schemaBundle } from "./sdk-bundle.mjs";

/** The two clients, relative to the repository root. */
export const CLIENTS = Object.freeze({
  typescript: "npm/onemessagebus-sdk/src/client.ts",
  python: "python/onemessagebus-sdk/src/onemessagebus/_client.py",
});

/** The Python spelling of a capability's camelCase method. */
export const pythonName = (method) =>
  method.replace(/[A-Z]/gu, (upper) => `_${upper.toLowerCase()}`);

/**
 * The text of the class `header` opens in `source`: from its header line to the
 * first later line that starts at column zero (the closing brace in TypeScript,
 * the next top-level statement in Python).
 */
export function classBody(source, header, file) {
  const lines = source.split("\n");
  const start = lines.findIndex((line) => line.startsWith(header));
  if (start === -1) {
    throw new Error(`${file} declares no \`${header}\`; the parity gate reads the client there`);
  }
  const end = lines.findIndex(
    (line, index) =>
      index > start && line.length > 0 && !/^\s/u.test(line) && !line.startsWith("#"),
  );
  const body = lines.slice(start + 1, end === -1 ? lines.length : end);
  // TypeScript's closing brace is the line that ended the class; Python's next
  // statement is not part of it either way.
  return body.join("\n");
}

/** The public methods a client source defines, by the spelling a caller uses. */
export function definedMethods(language, file) {
  const source = readFileSync(resolve(ROOT, file), "utf8");
  if (language === "typescript") {
    const body = classBody(source, "export class Client", file);
    return new Set(
      [...body.matchAll(/^ {2}(?:async )?\*?([A-Za-z][A-Za-z0-9]*)\s*[(<]/gmu)]
        .map((match) => match[1])
        .filter((name) => name !== "constructor"),
    );
  }
  const body = classBody(source, "class Client", file);
  return new Set(
    [...body.matchAll(/^ {4}(?:async )?def ([a-z][a-z0-9_]*)\s*\(/gmu)].map((match) => match[1]),
  );
}

/**
 * Every disagreement between `capabilities` and the clients at `files`: a
 * capability with no method, and a method with no capability.
 */
export function disagreements(capabilities, files = CLIENTS) {
  const surfaces = [
    { label: "TypeScript", language: "typescript", file: files.typescript, name: (m) => m },
    { label: "Python", language: "python", file: files.python, name: pythonName },
  ];
  const found = [];
  for (const surface of surfaces) {
    const defined = definedMethods(surface.language, surface.file);
    const expected = new Map(capabilities.map((c) => [surface.name(c.method), c]));
    for (const [name, capability] of expected) {
      if (!defined.has(name)) {
        found.push(
          `sdk-coverage: ${surface.label} has no \`${name}\` for \`onemessagebus ${capability.verb.join(" ")}\`\n` +
            `  fix: add it to ${surface.file}; it renders its argv from the manifest's \`${capability.method}\` bindings`,
        );
      }
    }
    for (const name of defined) {
      if (!expected.has(name)) {
        found.push(
          `sdk-coverage: ${surface.label} defines \`${name}\`, which no capability names\n` +
            `  fix: remove it from ${surface.file}, make it private, or declare the capability in crates/onemessagebus/src/capability.rs`,
        );
      }
    }
  }
  return found;
}

function main() {
  const [typescript = CLIENTS.typescript, python = CLIENTS.python] = process.argv.slice(2);
  const { capabilities } = schemaBundle({ script: "sdk-coverage", rerun: "just sdk-coverage" });
  let found;
  try {
    found = disagreements(capabilities, { typescript, python });
  } catch (error) {
    console.error(`sdk-coverage: ${error.message}`);
    process.exit(1);
  }
  if (found.length > 0) {
    for (const line of found) console.error(line);
    console.error(
      "A capability with no SDK method sends a consumer back to raw argv, and a method with no capability is one no parity gate holds; both are what this gate exists to refuse.",
    );
    process.exit(1);
  }
  console.log(
    `sdk-coverage: ${capabilities.length} capabilities, each exactly one method in TypeScript and in Python`,
  );
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) main();
