// What the npm launcher does on each platform, held on any host.
//
// The launcher reads `process.platform` and `process.arch` once and hands them to
// `platformPackage`, so the answer for a platform this machine is not — Intel
// macOS, which releases no longer build, or one never built — is asserted here by
// passing it, rather than by faking the host. npm/e2e/launcher.test.mjs drives
// the installed launcher on the platform it really runs on.

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { describe, it } from "node:test";

const { platformPackage } = createRequire(import.meta.url)(
  "../onemessagebus-cli/bin/onemessagebus.js",
);

describe("the launcher's platform resolution", () => {
  it("resolves each released platform to its package", () => {
    for (const [platform, arch] of [
      ["linux", "x64"],
      ["linux", "arm64"],
      ["darwin", "arm64"],
      ["win32", "x64"],
    ])
      assert.deepEqual(platformPackage(platform, arch), {
        package: `onemessagebus-cli-${platform}-${arch}`,
      });
  });

  it("refuses Intel macOS as no longer supported, naming the way that still works", () => {
    const { package: pkg, refusal } = platformPackage("darwin", "x64");
    assert.equal(pkg, undefined, "no package is looked up for Intel macOS");
    assert.match(refusal, /^Intel macOS \(darwin-x64\) is no longer supported/);
    assert.match(refusal, /Apple Silicon macOS \(darwin-arm64\)/);
    assert.match(
      refusal,
      /cargo install --git https:\/\/github\.com\/nickderobertis\/onemessagebus/,
    );
    assert.doesNotMatch(refusal, /pip install|onemessagebus-cli-darwin-x64/);
  });

  it("refuses a platform no release ever built, listing those that exist", () => {
    const { package: pkg, refusal } = platformPackage("freebsd", "x64");
    assert.equal(pkg, undefined);
    assert.match(refusal, /^unsupported platform freebsd-x64\./);
    assert.match(refusal, /linux-x64, linux-arm64, darwin-arm64, win32-x64/);
    assert.match(refusal, /pip install onemessagebus-cli/);
  });
});
