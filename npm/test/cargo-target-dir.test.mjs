// `scripts/cargo-target-dir.mjs`, run the way the SDK install recipe runs it and
// over the real cargo: the directory it prints is the one Cargo's environment and
// configuration name, a hand-off is taken as handed, and a refusal says why.
// The last case drives the recipe itself to its refusal, which costs nothing
// because the resolution comes before anything the journey builds.

import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { after, describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const SCRIPT = join(REPO_ROOT, "scripts", "cargo-target-dir.mjs");

const made = [];
after(() => {
  for (const dir of made) rmSync(dir, { recursive: true, force: true });
});
function scratch() {
  const dir = mkdtempSync(join(tmpdir(), "cargo-target-dir-"));
  made.push(dir);
  return dir;
}

/// The process environment without anything that names a target directory, plus `env`.
function environment(env) {
  const {
    CARGO_TARGET_DIR: _target,
    CARGO_BUILD_TARGET_DIR: _build,
    ONEMESSAGEBUS_TARGET_DIR: _handed,
    ...rest
  } = process.env;
  return { ...rest, ...env };
}

function resolveUnder(env) {
  return spawnSync(process.execPath, [SCRIPT], {
    cwd: REPO_ROOT,
    env: environment(env),
    encoding: "utf8",
  });
}

/// A CARGO_HOME whose configuration cargo refuses to parse.
function brokenCargoHome() {
  const home = scratch();
  writeFileSync(join(home, "config.toml"), "[build\n");
  return home;
}

describe("cargo-target-dir", () => {
  for (const variable of ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]) {
    it(`prints the directory ${variable} names`, () => {
      const dir = scratch();
      const run = resolveUnder({ [variable]: dir });
      assert.equal(run.stderr, "");
      assert.equal(run.status, 0);
      assert.equal(run.stdout, dir);
    });
  }

  it("prints the clone's own target directory when only .cargo/config.toml names one", () => {
    const run = resolveUnder({});
    assert.equal(run.status, 0, run.stderr);
    assert.equal(run.stdout, join(REPO_ROOT, "target"));
  });

  it("takes a recipe's hand-off as handed, and refuses one that is not absolute", () => {
    const dir = scratch();
    const handed = resolveUnder({
      ONEMESSAGEBUS_TARGET_DIR: dir,
      CARGO_TARGET_DIR: join(dir, "x"),
    });
    assert.equal(handed.status, 0, handed.stderr);
    assert.equal(handed.stdout, dir);

    const relative = resolveUnder({ ONEMESSAGEBUS_TARGET_DIR: "target" });
    assert.equal(relative.status, 1);
    assert.equal(relative.stdout, "");
    assert.match(
      relative.stderr,
      /^cargo-target-dir: ONEMESSAGEBUS_TARGET_DIR named "target" as Cargo's target directory, not an absolute path\n {2}fix: /,
    );
  });

  it("refuses with cargo's own words when cargo cannot answer", () => {
    const home = brokenCargoHome();
    const run = resolveUnder({ CARGO_HOME: home });
    assert.equal(run.status, 1);
    assert.equal(run.stdout, "");
    assert.match(
      run.stderr,
      /^cargo-target-dir: `cargo metadata` did not name Cargo's target directory:\n/,
    );
    assert.ok(
      run.stderr.includes(`could not parse TOML configuration in \`${join(home, "config.toml")}\``),
      run.stderr,
    );
  });

  it("stops the SDK install recipe before it builds anything, naming why", () => {
    const home = brokenCargoHome();
    const run = spawnSync("just", ["_sdk-install-test"], {
      cwd: REPO_ROOT,
      env: environment({ CARGO_HOME: home }),
      encoding: "utf8",
    });
    assert.equal(run.status, 1, run.stderr);
    assert.ok(run.stderr.includes("could not parse TOML configuration"), run.stderr);
    assert.ok(
      run.stderr.includes(
        "onemessagebus-sdk-install-e2e: Cargo's target directory did not resolve — its output is above",
      ),
      run.stderr,
    );
  });
});
