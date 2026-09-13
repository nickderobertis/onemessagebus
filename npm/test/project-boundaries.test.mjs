// The project boundaries, enforced.
//
// `nx.json`'s `boundaries.allow` says which project types may depend on which,
// by tag: the core is a contract and depends on nothing in this repository, the
// profile depends on the core, the binary on both, the journeys and the
// packaging on what they drive. Nx cannot read a Cargo manifest, so each crate's
// project.json restates its Cargo dependencies as `implicitDependencies` — and
// a restated edge is one that drifts. Both halves are held here: every declared
// edge is one the tags allow, and every crate's declared edges are exactly the
// workspace dependencies Cargo reports for it.

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const SKIP = new Set(["node_modules", "target", ".git", ".nx", "dist"]);

/// Every project.json in the tree, as `{ path, project }`.
function projects(dir = REPO_ROOT, found = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP.has(entry)) continue;
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) projects(path, found);
    else if (entry === "project.json") {
      found.push({
        path: relative(REPO_ROOT, path),
        project: JSON.parse(readFileSync(path, "utf8")),
      });
    }
  }
  return found;
}

/// The one `type:` tag a project carries.
function typeOf(entry) {
  const types = (entry.project.tags ?? []).filter((tag) => tag.startsWith("type:"));
  assert.equal(types.length, 1, `${entry.path} must carry exactly one type: tag, has ${types}`);
  return types[0];
}

describe("the project boundaries", () => {
  const all = projects();
  const byName = new Map(all.map((entry) => [entry.project.name, entry]));
  const allow = JSON.parse(readFileSync(join(REPO_ROOT, "nx.json"), "utf8")).boundaries.allow;

  it("declares an allowed-dependency rule for every project type in use", () => {
    for (const entry of all) {
      assert.ok(typeOf(entry) in allow, `nx.json's boundaries do not name ${typeOf(entry)}`);
    }
  });

  it("draws every declared edge within what the tags allow", () => {
    for (const entry of all) {
      const from = typeOf(entry);
      for (const name of entry.project.implicitDependencies ?? []) {
        const target = byName.get(name);
        assert.ok(target, `${entry.path} depends on ${name}, which is no project`);
        assert.ok(
          allow[from].includes(typeOf(target)),
          `${entry.path} (${from}) may not depend on ${name} (${typeOf(target)}); nx.json's boundaries allow ${from} -> ${allow[from].join(", ") || "nothing"}`,
        );
      }
    }
  });

  it("keeps the contract project free of edges into this repository", () => {
    for (const entry of all.filter((entry) => typeOf(entry) === "type:contract")) {
      assert.deepEqual(
        entry.project.implicitDependencies ?? [],
        [],
        `${entry.path} depends on something`,
      );
    }
  });

  it("restates exactly the workspace dependencies Cargo reports for each crate", () => {
    const metadata = JSON.parse(
      execFileSync("cargo", ["metadata", "--format-version", "1", "--locked"], {
        cwd: REPO_ROOT,
        encoding: "utf8",
      }),
    );
    const members = new Set(
      metadata.packages
        .filter((pkg) => metadata.workspace_members.includes(pkg.id))
        .map((pkg) => pkg.name),
    );
    for (const pkg of metadata.packages) {
      if (!members.has(pkg.name)) continue;
      const declared = byName.get(pkg.name);
      assert.ok(declared, `crate ${pkg.name} has no project.json`);
      const cargo = [
        ...new Set(pkg.dependencies.map((dep) => dep.name).filter((name) => members.has(name))),
      ].sort();
      const nx = [...(declared.project.implicitDependencies ?? [])].sort();
      assert.deepEqual(
        nx,
        cargo,
        `${declared.path}'s implicitDependencies and ${pkg.name}'s Cargo workspace dependencies disagree`,
      );
    }
  });
});
