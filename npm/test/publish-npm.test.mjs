// `scripts/publish-npm.sh` refusing an invocation with nothing to publish.
//
// The publish itself talks to the live registry and is proven by release.yml's
// verify-npm job; what can be driven here is the refusal that happens before any
// registry is asked anything, and it must say what was wrong as well as what to do.

import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

describe("scripts/publish-npm.sh", {
  skip: process.platform === "win32" && "drives a POSIX shell script",
}, () => {
  it("refuses to run with no package, naming the cause and the next action, before asking npm anything", () => {
    const result = spawnSync("bash", [join("scripts", "publish-npm.sh")], {
      cwd: REPO_ROOT,
      env: { PATH: process.env.PATH, HOME: process.env.HOME },
      encoding: "utf8",
    });
    assert.equal(result.error, undefined, `publish-npm.sh could not be run: ${result.error}`);
    assert.equal(result.status, 1);
    assert.equal(result.stdout, "");
    assert.match(
      result.stderr,
      /^publish-npm: no package directory or tarball was given, so there is nothing to publish; pass at least one, e\.g\. 'publish-npm\.sh npm\/dist\/onemessagebus-cli'\n$/,
    );
  });
});
