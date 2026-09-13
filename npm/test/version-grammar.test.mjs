// The one release-version grammar, read in two languages.
//
// `scripts/npm-build.mjs` refuses a version before stamping it into a manifest,
// and `scripts/release-probe.sh` refuses a registry's answer that is not one
// version. They are JavaScript and bash, and the probe runs where only bash and
// curl can be assumed, so neither can read the other's expression at run time.
// This holds them to one language instead, twice over: the expressions are
// compared structurally, after spelling the two dialects' syntax the same way, so
// any difference in what they accept fails here; and both real matchers — the
// script's exported RegExp and bash's `[[ =~ ]]` over the probe's own definitions
// — are run over one corpus and must agree with each other and with SemVer.

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { VERSION } from "../../scripts/npm-build.mjs";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const PROBE = join(REPO_ROOT, "scripts", "release-probe.sh");

/// The probe's grammar definitions, exactly as the script declares them.
const DEFINITIONS = readFileSync(PROBE, "utf8")
  .split("\n")
  .filter((line) => /^readonly (NUMBER|PRERELEASE_ID|BUILD_ID|VERSION_SHAPE)=/.test(line))
  .join("\n");

/// Evaluates the probe's definitions in bash and hands back what bash made of them.
function bash(script, input = "") {
  const result = spawnSync("bash", ["-c", `set -euo pipefail\n${DEFINITIONS}\n${script}`], {
    input,
    encoding: "utf8",
  });
  assert.equal(result.status, 0, `bash could not evaluate the probe's grammar: ${result.stderr}`);
  return result.stdout;
}

/// The JavaScript dialect spelled as POSIX ERE: groups are capturing and digits
/// are a bracket expression. Nothing else in the grammar differs between the two.
function asEre(source) {
  return source.replaceAll("(?:", "(").replaceAll("\\d", "[0-9]");
}

const ACCEPTED = [
  "0.0.0",
  "0.1.0",
  "1.2.3",
  "10.20.30",
  "1.2.3-0",
  "1.2.3-rc.1",
  "1.2.3-alpha-beta",
  "1.2.3-0a",
  "1.0.0-0.3.7",
  "1.0.0-x.7.z.92",
  "1.2.3+build.7",
  "1.2.3+001",
  "1.0.0-0.3.7+exp.sha.5114f85",
  "1.0.0-rc.1+build-1.2",
];

const REFUSED = [
  "",
  "1",
  "1.2",
  "v1.2.3",
  "01.2.3",
  "1.02.3",
  "1.2.03",
  "1.2.3.4",
  "1.2.3-",
  "1.2.3-.",
  "1.2.3-a..b",
  "1.2.3-rc.",
  "1.2.3-.rc",
  "1.2.3-01",
  "1.2.3-rc.01",
  "1.2.3+",
  "1.2.3+a..b",
  "1.2.3+.b",
  "1.2.3+one+two",
  "1.2.3-a_b",
  "1.2.3 ",
  " 1.2.3",
  "1.2.3\n",
];

describe("the release-version grammar", () => {
  it("is one expression in npm-build.mjs and release-probe.sh", () => {
    const probe = bash('printf %s "$VERSION_SHAPE"');
    assert.ok(probe.length > 0, "release-probe.sh declares no VERSION_SHAPE");
    assert.equal(
      asEre(VERSION.source),
      probe,
      "npm-build.mjs's VERSION and release-probe.sh's VERSION_SHAPE accept different versions",
    );
  });

  it("accepts and refuses the same versions in both matchers, as SemVer does", () => {
    const corpus = [...ACCEPTED.map((v) => [v, true]), ...REFUSED.map((v) => [v, false])];
    // One bash process for the whole corpus: NUL-separated in, one verdict per line out.
    const verdicts = bash(
      'while IFS= read -r -d "" v; do if [[ $v =~ $VERSION_SHAPE ]]; then echo yes; else echo no; fi; done',
      corpus.map(([v]) => `${v}\0`).join(""),
    )
      .trimEnd()
      .split("\n");
    assert.equal(verdicts.length, corpus.length, "bash did not judge every version");
    corpus.forEach(([version, expected], i) => {
      const label = JSON.stringify(version);
      assert.equal(VERSION.test(version), expected, `npm-build.mjs on ${label}`);
      assert.equal(verdicts[i] === "yes", expected, `release-probe.sh on ${label}`);
    });
  });
});
