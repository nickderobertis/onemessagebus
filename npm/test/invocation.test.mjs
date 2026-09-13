// `npm/e2e/support/invocation.mjs` decides how the launcher journeys start `npm`
// and the launcher npm installed. The Windows answers are proven here, on any
// host, by handing them the platform — and the POSIX answers by starting what
// they return.
//
// The Windows launcher form is held to what `cmd.exe /d /s /c` does with it: `/s`
// strips the first and last quote of the line and runs the rest, so the words
// that line holds are the shim's `.cmd` and each argument, quoted whole.

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { npmInvocation, shimInvocation } from "../e2e/support/invocation.mjs";

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
  const windowsNode = String.raw`C:\hostedtoolcache\windows\node\24.1.0\x64\node.exe`;

  it("starts npm on Windows as node on the npm-cli.js beside it, with no batch file", () => {
    const args = ["pack", "--json", "--pack-destination", String.raw`C:\tmp\a b`, "dir"];
    const form = npmInvocation(args, { platform: "win32", execPath: windowsNode });
    assert.deepEqual(form, {
      command: windowsNode,
      args: [
        String.raw`C:\hostedtoolcache\windows\node\24.1.0\x64\node_modules\npm\bin\npm-cli.js`,
        ...args,
      ],
      options: {},
    });
    assert.doesNotMatch(
      form.command,
      /\.(cmd|bat)$/i,
      "Node cannot start a batch file without a shell",
    );
  });

  it("runs the installed launcher's .cmd shim through cmd.exe on Windows rather than the script by path", () => {
    const form = shimInvocation(launcher, ["events", "merge", "no-such-stream.ndjson"], "win32");
    assert.equal(form.command, "cmd.exe", "Node cannot start the .cmd itself without a shell");
    assert.deepEqual(form.args.slice(0, 3), ["/d", "/s", "/c"]);
    assert.equal(form.args.length, 4, "the line is one argument cmd.exe reads verbatim");
    assert.deepEqual(form.options, { windowsVerbatimArguments: true });
    assert.equal(form.options.shell, undefined);
    assert.deepEqual(cmdWords(form.args[3]), [
      `${launcher}.cmd`,
      "events",
      "merge",
      "no-such-stream.ndjson",
    ]);
  });

  it("refuses on Windows a shim named without its full path, naming it", () => {
    assert.throws(
      () => shimInvocation("onemessagebus", ["--version"], "win32"),
      /"onemessagebus".*full path/,
    );
  });

  it("refuses on Windows an argument cmd.exe would reinterpret, naming it", () => {
    assert.throws(() => shimInvocation(launcher, ["%PATH%"], "win32"), /"%PATH%".*cmd\.exe/);
    assert.throws(() => shimInvocation(launcher, ['a"b'], "win32"), /cmd\.exe/);
  });

  for (const platform of ["linux", "darwin"]) {
    it(`starts each program by its bare name on ${platform}`, () => {
      const npm = npmInvocation(["install", "--no-audit"], { platform, execPath: "/usr/bin/node" });
      assert.deepEqual(npm, { command: "npm", args: ["install", "--no-audit"], options: {} });
      const bin = shimInvocation(
        "/tmp/app/node_modules/.bin/onemessagebus",
        ["--version"],
        platform,
      );
      assert.deepEqual(bin, {
        command: "/tmp/app/node_modules/.bin/onemessagebus",
        args: ["--version"],
        options: {},
      });
    });
  }

  it("gives a POSIX host forms it executes, arguments intact", {
    skip: process.platform === "win32" && "starts POSIX executables",
  }, () => {
    const dir = mkdtempSync(join(tmpdir(), "invocation-"));
    try {
      const program = join(dir, "echo-args");
      writeFileSync(program, '#!/bin/sh\nprintf "%s\\n" "$@"\n', { mode: 0o755 });
      const args = ["a b", "%PATH%", 'q"d'];
      const form = shimInvocation(program, args, process.platform);
      const out = execFileSync(form.command, form.args, { ...form.options, encoding: "utf8" });
      assert.deepEqual(out.split("\n").slice(0, -1), args);

      const npm = npmInvocation(["--version"], {
        platform: process.platform,
        execPath: process.execPath,
      });
      assert.match(
        execFileSync(npm.command, npm.args, { ...npm.options, encoding: "utf8" }),
        /^\d+\.\d+\.\d+/,
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
