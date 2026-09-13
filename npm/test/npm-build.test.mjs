// `scripts/npm-build.mjs`'s version boundary, driven the way release.yml drives
// the script: `launcher --version <v> --out <dir>`, as a real process over a real
// output directory.
//
// The version it stamps is what npm indexes the launcher and every platform pin
// under, so a malformed one must stop the script before anything is assembled —
// at the registry it would fail mid-publish, after some packages were already up.

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const SCRIPT = join(REPO_ROOT, "scripts", "npm-build.mjs");

describe("scripts/npm-build.mjs's version", () => {
  let scratch;
  let cases = 0;

  before(() => {
    scratch = mkdtempSync(join(tmpdir(), "npm-build-version-"));
  });
  after(() => {
    rmSync(scratch, { recursive: true, force: true });
  });

  function assemble(version) {
    cases += 1;
    const out = join(scratch, `out-${cases}`);
    const result = spawnSync(
      process.execPath,
      [SCRIPT, "launcher", "--version", version, "--out", out],
      {
        cwd: REPO_ROOT,
        encoding: "utf8",
      },
    );
    assert.equal(result.error, undefined, `npm-build.mjs could not be run: ${result.error}`);
    return { ...result, out };
  }

  it("stamps a SemVer version into the launcher and every platform pin", () => {
    for (const version of ["0.1.0", "1.2.3-rc.1", "1.0.0-0.3.7+exp.sha.5114f85", "2.0.0+build.7"]) {
      const result = assemble(version);
      assert.equal(result.status, 0, `${version}: ${result.stderr}`);
      const dir = join(result.out, "onemessagebus-cli");
      assert.equal(result.stdout, `${dir}\n`);
      const manifest = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
      assert.equal(manifest.version, version);
      assert.ok(Object.keys(manifest.optionalDependencies).length > 0);
      for (const [name, pinned] of Object.entries(manifest.optionalDependencies)) {
        assert.equal(pinned, version, `${name} is not pinned to ${version}`);
      }
    }
  });

  it("refuses a malformed version by name before assembling anything", () => {
    for (const version of [
      "1.2",
      "v1.2.3",
      "01.2.3",
      "1.2.3-",
      "1.2.3-.",
      "1.2.3-a..b",
      "1.2.3-rc.",
      "1.2.3-01",
      "1.2.3+",
      "1.2.3+a..b",
      "1.2.3+one+two",
    ]) {
      const result = assemble(version);
      assert.equal(result.status, 1, `${version} was accepted:\n${result.stdout}`);
      assert.equal(result.stdout, "", `${version} printed a package directory`);
      assert.match(
        result.stderr,
        new RegExp(
          `^npm-build: '${version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}' is not a version either registry can index\nACTION: pass --version X\\.Y\\.Z`,
        ),
        version,
      );
      assert.ok(!existsSync(result.out), `${version} left an assembled package behind`);
    }
  });
});
