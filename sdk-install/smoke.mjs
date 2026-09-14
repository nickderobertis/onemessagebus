// The installed Node SDK, driven against the binary installed beside it.
//
// Run from a fresh project holding only what `npm install @onemessagebus/sdk`
// puts there, at the version a release would stamp: the SDK resolves the binary
// through the `onemessagebus-cli` launcher it depends on, holds it to the version
// it pins, and answers once over each transport.

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { Client, ResidentTransport, SDK_VERSION } from "@onemessagebus/sdk";

const expected = process.argv[2];
if (SDK_VERSION !== expected) {
  throw new Error(`the installed SDK is ${SDK_VERSION}, and this revision packs ${expected}`);
}

const scratch = mkdtempSync(join(tmpdir(), "sdk-install-"));
try {
  const config = { transportDir: join(scratch, "channel") };
  const oneShot = new Client({ config });
  const kinds = await oneShot.transports();
  if (!kinds.some((kind) => kind.kind === "local")) {
    throw new Error("the installed binary lists no local transport");
  }
  await oneShot.send("surfaces", {
    kind: "finding",
    message: "installed",
    source: "proposal",
    blocking: false,
  });
  await oneShot[Symbol.asyncDispose]();

  const resident = new Client({
    config,
    transport: new ResidentTransport({ socket: join(scratch, "bus.sock") }),
  });
  const statuses = await resident.status("surfaces");
  await resident[Symbol.asyncDispose]();
  if (statuses[0]?.records !== 1) {
    throw new Error(`the resident core reads ${JSON.stringify(statuses[0])} for what was sent`);
  }
  console.log(
    `onemessagebus ${expected}: the installed Node SDK drove the installed binary one-shot and through a resident core`,
  );
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
