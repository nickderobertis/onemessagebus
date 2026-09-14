// Which binary a client runs, and the version pin it holds that binary to.
import { afterAll, describe, expect, test } from "bun:test";
import { chmodSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { checkoutVersion, childEnv, describeBinary } from "../src/binary.js";
import {
  Client,
  pinnedVersion,
  resolveBinary,
  TransportError,
  VersionMismatch,
  verifyVersion,
} from "../src/index.js";
import { BINARY, caught, PACKAGE, ROOT, removeScratch, scratch } from "./support.js";

afterAll(removeScratch);

const WORKSPACE_VERSION = checkoutVersion(ROOT);

/** An executable that prints `output` for any argument: the one subprocess double here. */
function fakeBinary(output: string, exit = 0): string {
  const path = join(scratch("fake-bin"), "onemessagebus");
  writeFileSync(path, `#!/bin/sh\necho '${output}'\nexit ${exit}\n`);
  chmodSync(path, 0o755);
  return path;
}

describe("the version pin", () => {
  test("in a checkout, the pin is the workspace version Cargo.toml declares", () => {
    expect(WORKSPACE_VERSION).toMatch(/^\d+\.\d+\.\d+/u);
    expect(pinnedVersion()).toBe(WORKSPACE_VERSION as string);
    expect(pinnedVersion("1.2.3")).toBe("1.2.3");
  });

  test("with no stamp and no checkout, there is no pin, and that is said plainly", () => {
    const error = (() => {
      try {
        return pinnedVersion("0.0.0-dev", "/");
      } catch (thrown) {
        return thrown;
      }
    })();
    expect(error).toBeInstanceOf(VersionMismatch);
    expect((error as Error).message).toContain("carries no CLI version pin");
  });

  test("a client refuses a binary of another version before its first call, naming both", async () => {
    const fake = fakeBinary("onemessagebus 9.9.9");
    const client = new Client({ config: { binary: fake } });
    const error = await caught(() => client.transports());
    expect(error).toBeInstanceOf(VersionMismatch);
    expect((error as Error).message).toBe(
      `this onemessagebus SDK drives onemessagebus-cli ${WORKSPACE_VERSION}, and ${fake} reports 9.9.9; install onemessagebus-cli@${WORKSPACE_VERSION}`,
    );
    expect((error as VersionMismatch).expected).toBe(WORKSPACE_VERSION as string);
    expect((error as VersionMismatch).actual).toBe("9.9.9");
  });

  test("the real binary is the pinned version, and the check is made once", async () => {
    await verifyVersion({ command: BINARY, prefix: [] });
    const client = new Client({ config: { binary: BINARY } });
    await client.transports();
    await client.transports();
  });

  test("a program that is not onemessagebus, one that fails, and one that is missing are each named", async () => {
    const other = await caught(() => verifyVersion({ command: fakeBinary("git 2.0"), prefix: [] }));
    expect(other).toBeInstanceOf(VersionMismatch);
    expect((other as Error).message).toContain("is not an onemessagebus binary");
    const failing = await caught(() =>
      verifyVersion({ command: fakeBinary("broken", 3), prefix: [] }),
    );
    expect(failing).toBeInstanceOf(TransportError);
    expect((failing as Error).message).toContain("--version failed");
    const missing = await caught(() =>
      new Client({ config: { binary: join(scratch("missing"), "onemessagebus") } }).status(),
    );
    expect(missing).toBeInstanceOf(TransportError);
    expect((missing as Error).message).toContain(
      "name the binary with ClientConfig.binary or ONEMESSAGEBUS_BIN",
    );
  });
});

describe("resolving the binary", () => {
  test("ClientConfig.binary first, then ONEMESSAGEBUS_BIN, then onemessagebus on PATH", () => {
    expect(resolveBinary({ binary: "/a/b", env: { ONEMESSAGEBUS_BIN: "/c" } })).toEqual({
      command: "/a/b",
      prefix: [],
    });
    expect(resolveBinary({ env: { ONEMESSAGEBUS_BIN: "/c" } })).toEqual({
      command: "/c",
      prefix: [],
    });
    // In a checkout the launcher resolves only where the root workspace installed it.
    const previous = process.env.ONEMESSAGEBUS_BIN;
    delete process.env.ONEMESSAGEBUS_BIN;
    try {
      let launcher: string | undefined;
      try {
        launcher = createRequire(join(PACKAGE, "src/binary.ts")).resolve(
          "onemessagebus-cli/bin/onemessagebus.js",
        );
      } catch {
        launcher = undefined;
      }
      expect(resolveBinary()).toEqual(
        launcher === undefined
          ? { command: "onemessagebus", prefix: [] }
          : { command: process.execPath, prefix: [launcher] },
      );
    } finally {
      if (previous !== undefined) process.env.ONEMESSAGEBUS_BIN = previous;
    }
    expect(describeBinary({ command: process.execPath, prefix: ["launcher.js"] })).toBe(
      `${process.execPath} launcher.js`,
    );
    expect(childEnv({ env: { EXTRA: "1" } }).EXTRA).toBe("1");
  });
});
