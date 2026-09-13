// `npm/e2e/support/invocation.mjs` decides how the launcher journeys start `npm`
// and the launcher npm installed. The Windows answer is proven here, on any host,
// by handing it the platform — and the POSIX answer by starting what it returns.
//
// The Windows form is held to what `cmd.exe /d /s /c` does with it: `/s` strips
// the first and last quote of the line and runs the rest, so the words that
// line holds are the program's `.cmd` shim and each argument, quoted whole.

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { invocation } from "../e2e/support/invocation.mjs";

/// The words `cmd.exe /s /c` runs from `line`: the outer quotes stripped, then
/// each double-quoted word read back. Fails unless every word is quoted whole.
function cmdWords(line) {
  assert.ok(line.startsWith('"') && line.endsWith('"'), `/s needs an outer-quoted line: ${line}`);
  const inner = line.slice(1, -1);
  const words = [...inner.matchAll(/"([^"]*)"/g)].map((match) => match[1]);
  assert.equal(
    words.map((word) => `"${word}"`).join(" "),
    inner,
    `every word must be quoted whole: ${inner}`,
  );
  return words;
}

describe("how the launcher journeys start a program", () => {
  const launcher = String.raw`C:\Users\runner\AppData\Local\Temp\app-1\node_modules\.bin\onemessagebus`;

  it("starts cmd.exe on Windows, which runs npm's batch shim without Node's shell", () => {
    const args = ["pack", "--json", "--pack-destination", String.raw`C:\tmp\a b`, "dir"];
    const form = invocation("npm", args, "win32");
    assert.equal(form.command, "cmd.exe", "Node cannot start npm.cmd itself without a shell");
    assert.deepEqual(form.args.slice(0, 3), ["/d", "/s", "/c"]);
    assert.equal(form.args.length, 4, "the line is one argument cmd.exe reads verbatim");
    assert.deepEqual(form.options, { windowsVerbatimArguments: true });
    assert.equal(form.options.shell, undefined);
    assert.deepEqual(cmdWords(form.args[3]), ["npm.cmd", ...args]);
  });

  it("runs the installed launcher's .cmd shim on Windows rather than the script by path", () => {
    const form = invocation(launcher, ["events", "merge", "no-such-stream.ndjson"], "win32");
    assert.equal(form.command, "cmd.exe");
    assert.deepEqual(cmdWords(form.args[3]), [
      `${launcher}.cmd`,
      "events",
      "merge",
      "no-such-stream.ndjson",
    ]);
  });

  it("refuses on Windows an argument cmd.exe would reinterpret, naming it", () => {
    assert.throws(() => invocation("npm", ["%PATH%"], "win32"), /"%PATH%".*cmd\.exe/);
    assert.throws(() => invocation("npm", ['a"b'], "win32"), /cmd\.exe/);
  });

  for (const platform of ["linux", "darwin"]) {
    it(`starts the program by its bare name on ${platform}`, () => {
      const form = invocation("npm", ["install", "--no-audit"], platform);
      assert.deepEqual(form, { command: "npm", args: ["install", "--no-audit"], options: {} });
      const bin = invocation(launcher, ["--version"], platform);
      assert.equal(bin.command, launcher);
      assert.deepEqual(bin.options, {});
    });
  }

  it("gives a POSIX host a form it executes, arguments intact", {
    skip: process.platform === "win32" && "starts a POSIX executable",
  }, () => {
    const dir = mkdtempSync(join(tmpdir(), "invocation-"));
    try {
      const program = join(dir, "echo-args");
      writeFileSync(program, '#!/bin/sh\nprintf "%s\\n" "$@"\n', { mode: 0o755 });
      const args = ["a b", "%PATH%", 'q"d'];
      const form = invocation(program, args, process.platform);
      const out = execFileSync(form.command, form.args, { ...form.options, encoding: "utf8" });
      assert.deepEqual(out.split("\n").slice(0, -1), args);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
