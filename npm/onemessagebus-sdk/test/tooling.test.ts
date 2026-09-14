// The package's own tooling: the generator's drift gate and its loud refusals, the
// packer's stamp, and defineMessage's contract.
import { afterAll, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { z } from "zod";
import { BusFailed, defineMessage, type MessageType } from "../src/index.js";
import { PACKAGE, removeScratch, scratch } from "./support.js";

afterAll(removeScratch);

function node(args: string[]) {
  return spawnSync(process.execPath === "bun" ? "node" : "node", args, {
    cwd: PACKAGE,
    encoding: "utf8",
  });
}

describe("generate:check", () => {
  test("is green on the committed output", () => {
    const run = node(["scripts/generate.mjs", "--check"]);
    expect(run.stderr).toBe("");
    expect(run.status).toBe(0);
  });

  test("goes red on a stale, a missing and a leftover file, naming each and how to fix it", () => {
    const copy = join(scratch("generated"), "generated");
    cpSync(join(PACKAGE, "src/generated"), copy, { recursive: true });
    const stale = join(copy, "roots/sent.ts");
    writeFileSync(stale, readFileSync(stale, "utf8").replace("queue", "queueName"));
    unlinkSync(join(copy, "roots/asked.ts"));
    writeFileSync(join(copy, "roots/removed.ts"), "export {};\n");
    const run = node(["scripts/generate.mjs", "--check", "--out", copy]);
    expect(run.status).toBe(1);
    expect(run.stderr).toContain("roots/sent.ts is stale (line ");
    expect(run.stderr).toContain("roots/asked.ts is missing");
    expect(run.stderr).toContain("roots/removed.ts is no longer generated");
    expect(run.stderr).toContain("run `bun run --cwd npm/onemessagebus-sdk generate`");
  });
});

describe("the zod generator", () => {
  const run = node(["test/zod-generator-cases.mjs"]);
  const cases: {
    refusals: { expected: string; refused: boolean; named?: boolean; message?: string }[];
    parses: boolean[];
  } = JSON.parse(run.stdout || "{}");

  test("runs its cases cleanly", () => {
    expect(run.stderr).toBe("");
    expect(run.status).toBe(0);
  });

  test("refuses a construct it does not enforce, by keyword and pointer", () => {
    expect(cases.refusals).toHaveLength(7);
    for (const refusal of cases.refusals) {
      expect(refusal.refused, `not refused as unsupported: ${refusal.expected}`).toBe(true);
      expect(refusal.message).toContain(refusal.expected);
      expect(refusal.message).toContain("extend scripts/zod-generator.mjs");
    }
  });

  test("emits schemas that hold the constraints the Rust validator does", () => {
    // a two-astral-character name passes (code points, not UTF-16 units); then an empty
    // name, a long one, a missing required key, a negative uint, an unknown key, and
    // a map value outside its enum are each refused
    expect(cases.parses).toEqual([true, false, false, false, false, false, false]);
  });
});

describe("defineMessage", () => {
  const Greeting = defineMessage("demo.greeting@1", z.object({ text: z.string() }));
  type Greeting = MessageType<typeof Greeting>;

  test("exposes its id, schema, canonical JSON Schema and parse", () => {
    const value: Greeting = Greeting.parse({ text: "hi" });
    expect(value).toEqual({ text: "hi" });
    expect(Greeting.id).toBe("demo.greeting@1");
    expect(Greeting.schema.safeParse({ text: 1 }).success).toBe(false);
    const document = Greeting.jsonSchema();
    expect(document.$schema).toBe("https://json-schema.org/draft/2020-12/schema");
    expect(document.title).toBe("demo.greeting");
    expect(document.required).toEqual(["text"]);
  });

  test("refuses a violating value naming the id and the pointer, as the core does", () => {
    const nested = defineMessage(
      "demo.nested@2",
      z.object({ items: z.array(z.object({ "a/b": z.number() })) }),
    );
    try {
      nested.parse({ items: [{ "a/b": "no" }] });
      throw new Error("parsed a violation");
    } catch (error) {
      expect(error).toBeInstanceOf(BusFailed);
      expect((error as Error).message).toStartWith("demo.nested@2: at /items/0/a~1b:");
    }
    expect(() => Greeting.parse("text")).toThrow("demo.greeting@1: at /:");
  });

  test("refuses a malformed id", () => {
    for (const id of [
      "greeting@1",
      "demo.greeting",
      "demo.greeting@0",
      "demo..x@1",
      "demo.a b@1",
    ]) {
      expect(() => defineMessage(id, z.object({}))).toThrow("is not a schema id");
    }
  });
});

describe("the packer", () => {
  function packageCopy(): string {
    const from = scratch("pack-from");
    cpSync(join(PACKAGE, "package.json"), join(from, "package.json"));
    cpSync(join(PACKAGE, "README.md"), join(from, "README.md"));
    const build = spawnSync(
      join(PACKAGE, "node_modules/.bin/tsc"),
      ["-p", "tsconfig.build.json", "--outDir", join(from, "dist")],
      { cwd: PACKAGE, encoding: "utf8" },
    );
    expect(build.stdout + build.stderr).toBe("");
    return from;
  }

  test("stamps the version, the exact CLI dependency and both constants, and refuses a moved placeholder", () => {
    const from = packageCopy();
    const out = join(scratch("pack-out"), "sdk");
    const packed = node(["scripts/pack.mjs", "--from", from, "--out", out, "--version", "4.5.6"]);
    expect(packed.stderr).toBe("");
    expect(packed.stdout.trim()).toBe(out);
    const manifest = JSON.parse(readFileSync(join(out, "package.json"), "utf8"));
    expect(manifest.version).toBe("4.5.6");
    expect(manifest.dependencies["onemessagebus-cli"]).toBe("4.5.6");
    expect(manifest.dependencies.zod).toBeDefined();
    expect(manifest.peerDependencies).toBeUndefined();
    expect(manifest.scripts).toBeUndefined();
    expect(manifest.devDependencies).toBeUndefined();
    const version = readFileSync(join(out, "dist/version.js"), "utf8");
    expect(version).toContain('export const SDK_VERSION = "4.5.6";');
    expect(version).toContain('export const CLI_VERSION = "4.5.6";');
    expect(version).toContain('export const PLACEHOLDER = "0.0.0-dev";');
    expect(existsSync(join(out, "README.md"))).toBe(true);

    // Defaulting to Cargo.toml's workspace version.
    const defaulted = node(["scripts/pack.mjs", "--from", from, "--out", out]);
    expect(defaulted.status).toBe(0);
    expect(JSON.parse(readFileSync(join(out, "package.json"), "utf8")).version).toMatch(
      /^\d+\.\d+\.\d+/u,
    );

    const built = join(from, "dist/version.js");
    writeFileSync(
      built,
      readFileSync(built, "utf8").replace('CLI_VERSION = "0.0.0-dev"', 'CLI_VERSION = "1.0.0"'),
    );
    const moved = node(["scripts/pack.mjs", "--from", from, "--out", out]);
    expect(moved.status).toBe(1);
    expect(moved.stderr).toContain('holds 0 `export const CLI_VERSION = "0.0.0-dev";`');

    const manifestPath = join(from, "package.json");
    writeFileSync(
      manifestPath,
      readFileSync(manifestPath, "utf8").replace('"version": "0.0.0-dev"', '"version": "1.0.0"'),
    );
    const version2 = node(["scripts/pack.mjs", "--from", from, "--out", out]);
    expect(version2.status).toBe(1);
    expect(version2.stderr).toContain("not the placeholder 0.0.0-dev");

    rmSync(join(from, "dist"), { recursive: true });
    mkdirSync(join(from, "dist"));
    const unbuilt = node(["scripts/pack.mjs", "--from", from, "--out", out]);
    expect(unbuilt.status).toBe(1);
    expect(unbuilt.stderr).toContain("build first");
    expect(node(["scripts/pack.mjs", "--version", "0.0.0-dev"]).stderr).toContain(
      "is not a version to publish",
    );
  }, 120_000);
});
