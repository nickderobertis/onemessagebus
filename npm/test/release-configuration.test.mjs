// The facts the release configuration restates, held to their one source.
//
// Two values are spelled in more than one file because the files that read them
// cannot share a constant: a workflow expression cannot read release-plz.toml,
// and nx.json cannot read the justfile. So each is held here to the file that
// owns it, and a change to the owner that misses a restatement fails the gate.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { parse } from "smol-toml";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

function read(...parts) {
  return readFileSync(join(REPO_ROOT, ...parts), "utf8");
}

describe("the release PR branch prefix", () => {
  const prefix = parse(read("release-plz.toml")).workspace.pr_branch_prefix;

  it("is declared in release-plz.toml", () => {
    assert.ok(prefix, "release-plz.toml declares no pr_branch_prefix");
  });

  it("is what ci.yml gives the full sweep to", () => {
    const ci = read(".github", "workflows", "ci.yml");
    const spelled = [...ci.matchAll(/startsWith\(github\.head_ref, '([^']+)'\)/g)].map((m) => m[1]);
    assert.ok(spelled.length > 0, "ci.yml no longer routes the release PR by its branch prefix");
    for (const found of spelled) assert.equal(found, prefix);
  });

  it("is what release-plz.yml auto-merges by", () => {
    const workflow = read(".github", "workflows", "release-plz.yml");
    const found = workflow.match(/startswith\("([^"]+)"\)/);
    assert.ok(found, "release-plz.yml no longer selects the release PR by its branch prefix");
    assert.equal(found[1], prefix);
  });
});

describe("the coverage profile directory", () => {
  it("is the same path in the justfile and in nx.json's test outputs", () => {
    const justfile = read("justfile");
    const owned = justfile.match(/^profraw-root := "([^"]+)"$/m);
    assert.ok(owned, "the justfile no longer names profraw-root");
    const nx = JSON.parse(read("nx.json"));
    const outputs = nx.targetDefaults.test.outputs;
    assert.deepEqual(outputs, [`{workspaceRoot}/${owned[1]}/*.profraw`]);
  });
});
