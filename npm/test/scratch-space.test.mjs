// The release scripts on a runner with nowhere to write a scratch file.
//
// Each of these makes a temporary file or directory before it does anything
// else, and under `set -e` a failed `mktemp` would end the script on the utility's
// own words with no next action — in a release job, where what the script prints
// is the only diagnosis anyone gets. So each is run for real with TMPDIR naming a
// directory that does not exist, and must say where it could not write and what to
// do, before it reaches a registry or needs the binary.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const NOWHERE = join(tmpdir(), `no-such-scratch-dir-${process.pid}`);

function run(script, args) {
  assert.ok(
    !existsSync(NOWHERE),
    `${NOWHERE} exists, so it cannot stand in for missing scratch space`,
  );
  const result = spawnSync("bash", [join("scripts", script), ...args], {
    cwd: REPO_ROOT,
    env: { PATH: process.env.PATH, HOME: process.env.HOME, TMPDIR: NOWHERE },
    encoding: "utf8",
  });
  assert.equal(result.error, undefined, `${script} could not be run: ${result.error}`);
  return result;
}

describe("the release scripts with no scratch space", {
  skip: process.platform === "win32" && "drives POSIX mktemp",
}, () => {
  it("release-probe.sh does not answer, and says so without claiming nothing is published", () => {
    const result = run("release-probe.sh", ["crate:onemessagebus"]);
    assert.notEqual(result.status, 0);
    assert.equal(result.stdout, "", "a not-answered probe printed an answer");
    assert.match(result.stderr, new RegExp(`cannot create a scratch file in ${NOWHERE}`));
    assert.match(result.stderr, /ACTION: .*this is NOT evidence that nothing is published/);
  });

  it("smoke-published.sh fails naming where it could not write, before probing any binary", () => {
    const result = run("smoke-published.sh", ["--label", "a runner with no scratch space"]);
    assert.equal(result.status, 1);
    assert.match(
      result.stderr,
      new RegExp(
        `::error::a runner with no scratch space: cannot create a scratch file in ${NOWHERE}`,
      ),
    );
    assert.match(result.stderr, /ACTION: free space there or point TMPDIR at a writable directory/);
  });

  it("publish-npm.sh fails naming where it could not write, before asking npm anything", () => {
    const result = run("publish-npm.sh", ["npm/onemessagebus-cli"]);
    assert.equal(result.status, 1);
    assert.equal(result.stdout, "");
    // mktemp's own complaint comes first; the script's diagnosis must follow on a
    // line of its own.
    assert.match(
      result.stderr,
      new RegExp(
        `^publish-npm: cannot create a scratch directory in ${NOWHERE}; .*then re-run the release$`,
        "m",
      ),
    );
  });
});
