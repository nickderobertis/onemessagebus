// The workspace's npm install stays in place while the format and lint targets
// run, as `just lint` and `just format-check` run them.
//
// `npm ci` replaces node_modules, so a target that reinstalled it mid-run swapped
// the Biome binary out from under every format and lint task running beside it,
// and they failed with MODULE_NOT_FOUND, `biome: command not found` or ETXTBSY.
// Only `just bootstrap`, serialized, may reinstall it.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { statSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";
import { root } from "../workflow-graph.mjs";

const POSIX_ONLY = process.platform === "win32" && "scripts/nx is a bash wrapper";

describe("the workspace's npm install", { skip: POSIX_ONLY }, () => {
  it("is not reinstalled by an uncached format and lint run", () => {
    const biome = join(root, "node_modules/@biomejs/biome/package.json");
    const before = statSync(biome).ino;
    const result = spawnSync(
      "bash",
      [
        "scripts/nx",
        "run-many",
        "-t",
        "format-check,lint",
        "--projects=github-workflows,onemessagebus-npm",
        "--skip-nx-cache",
      ],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(
      statSync(biome).ino,
      before,
      "node_modules was reinstalled during the run; keep `npm ci` out of every target a format or lint target depends on",
    );
  });
});
