// The release probe's three answers.
//
// `scripts/release-probe.sh` answers a registry-qualified identifier in exactly
// three ways — a version on stdout, nothing on stdout, or a non-zero exit with
// its reason on stderr. The deterministic gate stays offline, so no case here
// reaches a public registry: the first group needs no registry at all, and the
// second is answered by one this suite serves on the loopback interface, reached
// by the probe's own curl (see `probeRegistry`).
//
// The answer that matters most is the one that must never be confused with "no
// release yet". A consumer holds indefinitely on "not answered" and must never
// read it as evidence that nothing is published, so every not-answered case
// asserts all three of: a non-zero exit, an empty stdout, and a reason with a
// next action on stderr.

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";

import {
  assertNotAnswered,
  probe,
  probeRegistry,
  probeUnreachable,
  probeWithPath,
} from "./support/release-probe.mjs";

/// Where a tool actually is, so a search path can be built that has some of them
/// and not others.
function which(tool) {
  return execFileSync("bash", ["-c", `command -v ${tool}`], { encoding: "utf8" }).trim();
}

describe("the release probe's not-answered answer", () => {
  it("refuses anything but exactly one identifier", () => {
    assertNotAnswered(probe(), "no argument at all");
    assertNotAnswered(probe("crate:onemessagebus", "npm:onemessagebus-cli"), "two identifiers");
  });

  it("refuses an unqualified name rather than guessing a registry", () => {
    // The whole point of the qualification: one name is published to two
    // registries on two cadences, so an unqualified name is two artifacts and
    // answering for either would be answering the wrong question.
    const result = probe("onemessagebus-cli");
    assertNotAnswered(result, "an unqualified name");
    assert.match(result.stderr, /not registry-qualified/);
  });

  it("refuses a registry it cannot answer for", () => {
    const result = probe("apt:onemessagebus");
    assertNotAnswered(result, "an unknown registry");
    assert.match(result.stderr, /unknown registry/);
  });

  it("refuses a name it cannot build a URL for", () => {
    for (const identifier of [
      "npm:",
      "npm:@scope",
      "npm:@/pkg",
      "npm:@scope/pkg/extra",
      "pypi:@scope/pkg",
      "crate:@scope/pkg",
      "npm:one agent graph",
      "pypi:../etc",
    ]) {
      assertNotAnswered(probe(identifier), `the name in '${identifier}'`);
    }
  });

  it("refuses to answer at all when it has no transport", (t) => {
    if (process.platform === "win32") {
      // The search path this builds is a POSIX one, and the gate that runs this
      // suite is Linux. Nothing about the branch is platform-specific.
      t.skip("builds a POSIX search path");
      return;
    }
    // A real machine with no curl on it, assembled rather than simulated: a
    // search path carrying its interpreter and the one tool the probe reaches
    // for before the transport check, and nothing else. Reaching a registry is
    // the only thing this probe can do, so a machine that cannot must say so —
    // an empty answer here would report every artifact in the world as
    // unreleased.
    const bin = mkdtempSync(join(tmpdir(), "release-probe-no-curl-"));
    try {
      for (const tool of ["bash", "grep"]) symlinkSync(which(tool), join(bin, tool));
      const result = probeWithPath(bin, "crate:onemessagebus");
      assertNotAnswered(result, "a machine with no curl");
      assert.match(result.stderr, /curl/);
    } finally {
      rmSync(bin, { recursive: true, force: true });
    }
  });

  it("never answers an unrecognised identifier the way it answers an unreleased one", () => {
    // The failure this whole contract exists to prevent, stated as one
    // assertion. An unrecognised identifier must not exit 0 with empty output,
    // because that is precisely the answer that means "this registry has never
    // served it" — and a consumer reading the one as the other launches early,
    // against a fix that is not in force.
    // A scope is recognised on npm alone, so one on PyPI is refused before any
    // registry is asked.
    const unrecognised = probe("pypi:@scope/pkg");
    assert.notEqual(
      unrecognised.status,
      0,
      "an unrecognised identifier exited 0, which a caller reads as an answer about a real artifact",
    );
  });
});

describe("the release probe against a registry that answers", {
  skip: process.platform === "win32" && "reroutes curl with a bash script",
}, () => {
  const CRATE = "/crates.io/api/v1/crates/onemessagebus";
  const PYPI = "/pypi.org/pypi/onemessagebus-cli/json";
  const NPM = "/registry.npmjs.org/onemessagebus-cli";

  it("answers the version each registry says a bare install resolves to", async () => {
    const answers = [
      [
        {
          [CRATE]: {
            body: { crate: { max_stable_version: "1.4.2", newest_version: "1.5.0-rc.1" } },
          },
        },
        "crate:onemessagebus",
        "1.4.2",
      ],
      // No stable release yet: what `cargo add` falls back to.
      [
        {
          [CRATE]: { body: { crate: { max_stable_version: null, newest_version: "0.2.0-rc.1" } } },
        },
        "crate:onemessagebus",
        "0.2.0-rc.1",
      ],
      [
        { [PYPI]: { body: { info: { name: "onemessagebus-cli", version: "0.3.0" } } } },
        "pypi:onemessagebus-cli",
        "0.3.0",
      ],
      [
        {
          [NPM]: {
            body: {
              "dist-tags": { latest: "0.3.0+build.7", next: "0.4.0-rc.1" },
              versions: { "0.3.0+build.7": { version: "0.3.0+build.7" } },
            },
          },
        },
        "npm:onemessagebus-cli",
        "0.3.0+build.7",
      ],
      // A prerelease and a build together, each with several identifiers: the
      // whole of what SemVer lets one version say.
      [
        {
          [PYPI]: {
            body: { info: { name: "onemessagebus-cli", version: "1.0.0-0.3.7+exp.sha.5114f85" } },
          },
        },
        "pypi:onemessagebus-cli",
        "1.0.0-0.3.7+exp.sha.5114f85",
      ],
      // The Node SDK's scoped name, asked for as the registry's one path segment.
      [
        {
          "/registry.npmjs.org/@onemessagebus%2fsdk": {
            body: { "dist-tags": { latest: "0.4.0" }, versions: { "0.4.0": {} } },
          },
        },
        "npm:@onemessagebus/sdk",
        "0.4.0",
      ],
    ];
    for (const [routes, identifier, version] of answers) {
      const result = await probeRegistry(routes, identifier);
      assert.equal(result.status, 0, `${identifier}: ${result.stderr}`);
      assert.equal(result.stdout, `${version}\n`, `${identifier} did not answer its one version`);
      assert.equal(result.stderr, "");
    }
  });

  it("answers nothing, and exits 0, only when the registry says there is no such artifact", async () => {
    const result = await probeRegistry(
      { [NPM]: { status: 404, body: { error: "Not found" } } },
      "npm:onemessagebus-cli",
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, "");
  });

  it("never passes through a value that is not one version", async () => {
    // Each of these is exactly one `"key": "value"` pair, so the reader picks it
    // out; what it picked out is still not a version, and printing it would hand
    // a consumer a tag name, an escape sequence, or a second line as the answer.
    const values = [
      "latest",
      "1.2",
      "v1.2.3",
      "1.2.3 1.2.4",
      "1.2.3\n9.9.9",
      '1.2.3"',
      "1.2.3-",
      "$(touch pwned)",
      // Shaped like a version until an identifier is read: empty ones, a leading
      // zero, a build suffix repeated or left bare.
      "1.2.3-.",
      "1.2.3-a..b",
      "1.2.3-rc.",
      "1.2.3-01",
      "01.2.3",
      "1.2.3+",
      "1.2.3+a..b",
      "1.2.3+one+two",
    ];
    for (const value of values) {
      for (const [routes, identifier] of [
        [{ [CRATE]: { body: { crate: { max_stable_version: value } } } }, "crate:onemessagebus"],
        [{ [PYPI]: { body: { info: { version: value } } } }, "pypi:onemessagebus-cli"],
        [{ [NPM]: { body: { "dist-tags": { latest: value } } } }, "npm:onemessagebus-cli"],
      ]) {
        const result = await probeRegistry(routes, identifier);
        assertNotAnswered(result, `${identifier} serving ${JSON.stringify(value)}`);
        assert.match(
          result.stderr,
          /which is not a version/,
          `${identifier} serving ${JSON.stringify(value)}`,
        );
      }
    }
  });

  it("does not answer a document it cannot read one version from", async () => {
    const unreadable = [
      [{ [CRATE]: { body: { crate: { name: "onemessagebus" } } } }, "crate:onemessagebus"],
      [
        { [PYPI]: { body: '{"info":{"version":"0.3.0"},"also":{"version":"0.4.0"}}' } },
        "pypi:onemessagebus-cli",
      ],
      // npm serves a packument with no dist-tags for a name whose every version was unpublished.
      [
        { [NPM]: { body: { name: "onemessagebus-cli", time: { unpublished: {} } } } },
        "npm:onemessagebus-cli",
      ],
    ];
    for (const [routes, identifier] of unreadable) {
      const result = await probeRegistry(routes, identifier);
      assertNotAnswered(result, `${identifier} serving an unreadable document`);
      assert.match(result.stderr, /no version could be read from it unambiguously/);
    }
  });

  it("does not answer when the registry answers anything but yes or no", async () => {
    for (const status of [429, 500, 503]) {
      const result = await probeRegistry(
        { [CRATE]: { status, body: "busy" } },
        "crate:onemessagebus",
      );
      assertNotAnswered(result, `HTTP ${status}`);
      assert.match(result.stderr, new RegExp(`answered HTTP ${status}`));
    }
  });

  it("does not answer when the registry cannot be reached", async () => {
    const result = await probeUnreachable("pypi:onemessagebus-cli");
    assertNotAnswered(result, "an unreachable registry");
    assert.match(
      result.stderr,
      /could not reach https:\/\/pypi\.org\/pypi\/onemessagebus-cli\/json/,
    );
    assert.match(result.stderr, /NOT evidence that nothing is published/);
  });
});
