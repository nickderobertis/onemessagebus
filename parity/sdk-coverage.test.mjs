// The SDK parity gate, watched failing.
//
// A gate whose whole job is to fail is known to work only once it has been seen
// to: so `parity/sdk-coverage.mjs` is run for real — the real bundle, the real
// clients — and then against copies of each client with one capability's method
// renamed, and must go red naming both the capability left without a method and
// the method left without a capability. Nothing is stubbed; the copies differ from
// the clients only in that method's name, and the real sources are never touched.

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";

import { render } from "./parity-audit.mjs";
import { ROOT } from "../crates/onemessagebus-cli/sdk-bundle.mjs";
import { CLIENTS, classBody, definedMethods, pythonName } from "./sdk-coverage.mjs";

let scratch;
before(() => {
  scratch = mkdtempSync(join(tmpdir(), "sdk-coverage-"));
});
after(() => {
  if (scratch) rmSync(scratch, { recursive: true, force: true });
});

/** The gate over the clients at `typescript` and `python`. */
function gate(typescript = CLIENTS.typescript, python = CLIENTS.python) {
  return spawnSync(process.execPath, ["parity/sdk-coverage.mjs", typescript, python], {
    cwd: ROOT,
    encoding: "utf8",
  });
}

/**
 * A copy of `client` with every class-level definition of `from` — overloads
 * included — renamed to `to`, matched by `definition`, the shape the gate reads a
 * method by. Refused when the client defines no such method.
 */
function renamed(client, name, definition, from, to) {
  const source = readFileSync(join(ROOT, client), "utf8");
  const pattern = new RegExp(`^(${definition})${from}(?=\\s*[(<])`, "gmu");
  const copy = source.replace(pattern, `$1${to}`);
  assert.notEqual(
    copy,
    source,
    `${client} defines no \`${from}\`; point this case at a method it has`,
  );
  const path = join(scratch, name);
  writeFileSync(path, copy);
  return path;
}

describe("parity/sdk-coverage.mjs", () => {
  it("passes the clients as they stand, naming how many capabilities it held", () => {
    const result = gate();
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^sdk-coverage: \d+ capabilities, each exactly one method/);
  });

  it("goes red when the TypeScript client loses a capability's method", () => {
    const candidate = renamed(
      CLIENTS.typescript,
      "client.ts",
      " {2}(?:async )?\\*?",
      "status",
      "queueStatus",
    );
    const result = gate(candidate, CLIENTS.python);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /TypeScript has no `status` for `onemessagebus status`/);
    assert.match(result.stderr, /TypeScript defines `queueStatus`, which no capability names/);
    assert.doesNotMatch(result.stderr, /Python/);
  });

  it("goes red when the Python client loses a capability's method", () => {
    const candidate = renamed(
      CLIENTS.python,
      "_client.py",
      " {4}(?:async )?def ",
      "transports",
      "list_transports",
    );
    const result = gate(CLIENTS.typescript, candidate);
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /Python has no `transports` for `onemessagebus transports`/);
    assert.match(result.stderr, /Python defines `list_transports`, which no capability names/);
    assert.doesNotMatch(result.stderr, /TypeScript/);
  });

  it("refuses a client file that declares no client, naming what it looked for", () => {
    const empty = join(scratch, "empty.ts");
    writeFileSync(empty, "export const nothing = 1;\n");
    const result = gate(empty, CLIENTS.python);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /declares no `export class Client`/);
  });

  it("reads a class body up to its end and no further", () => {
    const body = classBody(
      "class Client:\n    async def send(self):\n        pass\n\ndef helper():\n    pass\n",
      "class Client",
      "inline.py",
    );
    assert.match(body, /async def send/);
    assert.doesNotMatch(body, /helper/);
    assert.equal(pythonName("inboxCarried"), "inbox_carried");
  });
});

describe("parity/parity-audit.mjs", () => {
  it("keeps docs/sdk-parity.md current", () => {
    const result = spawnSync(process.execPath, ["parity/parity-audit.mjs", "--check"], {
      cwd: ROOT,
      encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stderr);
  });

  it("marks a capability a client lacks as missing rather than covered", () => {
    const capabilities = [
      {
        method: "schemaList",
        verb: ["schema", "list"],
        options: "schema_list_options",
        output: "schema_list",
        stdout: "json",
        stdin: false,
        library_entry: "onemessagebus::Registry::ids",
        bindings: [{ option: "registry", flag: "--registry", kind: "value" }],
        uncovered: [{ flag: "--x", reason: "a reason | with a pipe" }],
      },
    ];
    const text = render(capabilities, new Set(), new Set(["schema_list"]));
    assert.match(text, /\| \*\*missing\*\* \| yes, `schema_list` \|/);
    assert.match(text, /`--x` is not an SDK option: a reason \\\| with a pipe\./);
    assert.ok(definedMethods("typescript", CLIENTS.typescript).size > 0);
  });
});
