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

/// One job's body in a workflow: from its header to the next line at job indentation.
function jobBody(workflow, job) {
  const start = workflow.indexOf(`\n  ${job}:\n`);
  assert.notEqual(start, -1, `no \`${job}\` job in release.yml`);
  const rest = workflow.slice(start + 1);
  const next = rest.slice(1).search(/\n {2}[a-z][a-z0-9-]*:\n/);
  return next === -1 ? rest : rest.slice(0, next + 1);
}

// The release commit was swept whole on its release PR, so a job that runs after
// publishing may verify what the registry serves and nothing else. What that looks
// like in a workflow is held here, so a post-publish job cannot quietly grow into a
// second gate over a commit that has already passed one.
describe("the post-publish jobs", () => {
  const workflow = read(".github", "workflows", "release.yml");
  const allowedScripts = ["scripts/retry-install.sh", "scripts/smoke-published.sh"];
  const gates = [
    /\bjust\b/,
    /\bcargo\b/,
    /\bnx\b/,
    /node --test/,
    /\bnpm (test|run)\b/,
    /\bpytest\b/,
    /\buv run\b/,
  ];

  for (const job of ["verify-pypi", "verify-npm"]) {
    it(`${job} checks out only the two scripts it runs`, () => {
      const body = jobBody(workflow, job);
      const checkout = body.match(
        /- uses: actions\/checkout@v4\n\s+with:\n\s+sparse-checkout: \|\n((?:\s+\S+\n)+)\s+sparse-checkout-cone-mode: false\n/,
      );
      assert.ok(checkout, `${job} checks out more than the scripts it runs`);
      assert.deepEqual(
        checkout[1].trim().split(/\s+/).sort(),
        [...allowedScripts].sort(),
        `${job}'s sparse checkout is not exactly the scripts it runs`,
      );
      assert.equal(
        (body.match(/actions\/checkout@/g) ?? []).length,
        1,
        `${job} checks out the repository a second time`,
      );
    });

    it(`${job} installs what the registry serves and smoke-tests it, running no repository gate`, () => {
      const body = jobBody(workflow, job);
      assert.match(
        body,
        /bash scripts\/retry-install\.sh/,
        `${job} no longer installs the published artifact`,
      );
      assert.match(
        body,
        /bash scripts\/smoke-published\.sh/,
        `${job} no longer smoke-tests the published artifact`,
      );
      const commands = [...body.matchAll(/run: \|?\n?((?:.*\n)*?)(?=\s+- (?:name|uses):|$)/g)]
        .map((m) => m[1])
        .join("\n")
        .split("\n")
        .map((line) => line.trim())
        .filter((line) => line && !line.startsWith("#"));
      for (const line of commands) {
        for (const gate of gates) {
          assert.doesNotMatch(
            line,
            gate,
            `${job} runs a repository gate after publishing: ${line}`,
          );
        }
        for (const script of line.matchAll(/bash (scripts\/\S+)/g)) {
          assert.ok(
            allowedScripts.includes(script[1]),
            `${job} runs ${script[1]} after publishing`,
          );
        }
      }
    });
  }
});
