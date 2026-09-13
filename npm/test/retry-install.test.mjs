// `scripts/retry-install.sh`, driven the way release.yml's verify jobs drive it:
// the real script around a real command whose outcome changes between attempts.
//
// The command is a shell one-liner over a counter file, so "the registry serves
// it on the fourth try" is a real process exiting non-zero three times and then
// zero — the script cannot tell it from an install that raced a CDN edge. Delays
// are the script's one-second minimum, so the whole suite runs in seconds.

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const SCRIPT = join(REPO_ROOT, "scripts", "retry-install.sh");

function retry(...args) {
  const result = spawnSync("bash", [SCRIPT, ...args], { encoding: "utf8", timeout: 60_000 });
  assert.equal(result.error, undefined, `retry-install.sh could not be run: ${result.error}`);
  return result;
}

describe("scripts/retry-install.sh", {
  skip: process.platform === "win32" && "drives POSIX shell commands",
}, () => {
  let dir;
  let cases = 0;

  before(() => {
    dir = mkdtempSync(join(tmpdir(), "retry-install-"));
  });
  after(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  /// An install that fails `failures` times — printing a resolver line and then
  /// the error, as `pip` and `npm` do — and succeeds on the attempt after.
  function flakyInstall(failures, status = 5) {
    cases += 1;
    const counter = join(dir, `attempts-${cases}`);
    const script = [
      'echo attempt >> "$1"',
      'n=$(wc -l < "$1")',
      `if [ "$n" -le ${failures} ]; then echo "resolving onemessagebus-cli==0.4.0"; echo "E404 no matching version on this edge"; exit ${status}; fi`,
      "echo installed onemessagebus-cli 0.4.0",
    ].join("\n");
    return {
      argv: ["bash", "-c", script, "install", counter],
      attempts: () =>
        existsSync(counter) ? readFileSync(counter, "utf8").split("\n").filter(Boolean).length : 0,
    };
  }

  it("is one line on stdout and nothing else when the first attempt installs", () => {
    const install = flakyInstall(0);
    const result = retry("--label", "onemessagebus-cli from PyPI", "--", ...install.argv);
    assert.equal(result.status, 0, result.stderr);
    assert.match(
      result.stdout,
      /^onemessagebus-cli from PyPI: installed on attempt 1 after \d+s\n$/,
    );
    assert.equal(result.stderr, "");
    assert.equal(install.attempts(), 1);
  });

  it("retries until the registry serves the version, doubling the delay up to its cap", () => {
    const install = flakyInstall(3);
    const result = retry(
      "--first-delay",
      "1",
      "--max-delay",
      "2",
      "--budget",
      "60",
      "--",
      ...install.argv,
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /: installed on attempt 4 after \d+s\n$/);
    assert.equal(install.attempts(), 4);
    // Each failure names itself with the command's own exit status and its last
    // line — the error, not the resolver chatter before it.
    const failures = [
      ...result.stderr.matchAll(/attempt (\d+) failed after \d+s \(exit (\d+)\): (.*)/g),
    ];
    assert.deepEqual(
      failures.map((m) => [m[1], m[2], m[3]]),
      [1, 2, 3].map((n) => [String(n), "5", "E404 no matching version on this edge"]),
    );
    const delays = [...result.stderr.matchAll(/retrying in (\d+)s/g)].map((m) => Number(m[1]));
    assert.deepEqual(
      delays,
      [1, 2, 2],
      "the delay did not double from --first-delay and stop at --max-delay",
    );
  });

  it("exhausts its budget with the last attempt's own words, the error, and the command's status", () => {
    const install = flakyInstall(1000, 7);
    const result = retry(
      "--budget",
      "3",
      "--first-delay",
      "1",
      "--max-delay",
      "1",
      "--label",
      "onemessagebus-cli on x86_64-pc-windows-msvc from npm",
      "--action",
      "check npm for onemessagebus-cli@0.4.0",
      "--",
      ...install.argv,
    );
    assert.equal(result.status, 7, "the exit status was not the failing install's own");
    assert.equal(result.stdout, "");
    assert.match(
      result.stderr,
      /--- last attempt's output ---\nresolving onemessagebus-cli==0\.4\.0\nE404 no matching version on this edge\n--- end of last attempt's output ---\n/,
    );
    const error = result.stderr.match(
      /::error::onemessagebus-cli on x86_64-pc-windows-msvc from npm: still not installable after (\d+) attempts over \d+s/,
    );
    assert.ok(error, `no error line naming the label:\n${result.stderr}`);
    assert.equal(Number(error[1]), install.attempts(), "the error miscounted the attempts it made");
    assert.ok(
      install.attempts() >= 2,
      "a three-second budget with one-second delays made a single attempt",
    );
    assert.match(result.stderr, /ACTION: check npm for onemessagebus-cli@0\.4\.0\n$/);
  });

  it("names the command, and the default next action, when not told either", () => {
    // The default first delay is past a one-second budget, so this is one attempt
    // and no sleep.
    const install = flakyInstall(1000, 1);
    const result = retry("--budget", "1", "--", ...install.argv);
    assert.equal(result.status, 1);
    assert.equal(install.attempts(), 1);
    assert.match(result.stderr, /::error::bash -c .* still not installable after 1 attempts/);
    assert.match(result.stderr, /ACTION: check the registry for the version above/);
  });

  it("refuses timing it cannot honour before running anything, naming the flag", () => {
    const refusals = [
      [["--budget", "0"], /--budget must be at least 1 second/],
      [["--budget", "ten"], /--budget needs a whole number of seconds, not 'ten'/],
      [["--budget", ""], /--budget needs a whole number of seconds, not ''/],
      [["--first-delay", "0"], /--first-delay must be at least 1 second/],
      [["--first-delay", "-1"], /--first-delay needs a whole number of seconds, not '-1'/],
      [["--max-delay", "1.5"], /--max-delay needs a whole number of seconds, not '1\.5'/],
      [["--first-delay", "5", "--max-delay", "2"], /--max-delay is below --first-delay/],
    ];
    for (const [options, reason] of refusals) {
      const install = flakyInstall(0);
      const result = retry(...options, "--", ...install.argv);
      const named = options.join(" ");
      assert.equal(result.status, 2, `'${named}' was not refused as input`);
      assert.equal(result.stdout, "", `'${named}' wrote to stdout`);
      assert.match(result.stderr, reason, `'${named}' was refused for the wrong reason`);
      assert.match(result.stderr, /ACTION: run 'retry-install\.sh /);
      assert.equal(install.attempts(), 0, `'${named}' ran the install before refusing`);
    }
  });

  it("refuses an invocation with no command, an unknown option, or an option with no value", () => {
    const refusals = [
      [["--budget", "5"], /no command to run/],
      [["--"], /no command to run/],
      [["--bogus", "--", "true"], /unknown option --bogus/],
      [["--label"], /--label needs a value/],
    ];
    for (const [options, reason] of refusals) {
      const result = retry(...options);
      assert.equal(result.status, 2, `'${options.join(" ")}' was not refused as input`);
      assert.match(result.stderr, reason);
    }
  });
});
