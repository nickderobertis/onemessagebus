#!/usr/bin/env node
// The published shape, installed: pack the SDK as scripts/pack.mjs stamps it,
// assemble the launcher and this host's platform package from the debug binary as
// a release does, install all of it offline into a fresh project, and drive the SDK
// under node with no binary named — so it finds the CLI the way a user's install
// does, through its exact `onemessagebus-cli` dependency.
//
// Needs `dist/` built (the `test:package` script builds it) and
// target/debug/onemessagebus (`cargo build -p onemessagebus-cli --locked`).
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PACKAGE = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ROOT = resolve(PACKAGE, "../..");
const BINARY = join(ROOT, "target/debug/onemessagebus");

function fail(message, action) {
  process.stderr.write(`test:package: ${message}\n  fix: ${action}\n`);
  process.exit(1);
}

const TARGETS = {
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
};
const target = TARGETS[`${process.platform}-${process.arch}`];
if (!target) {
  fail(
    `no platform package is assembled for ${process.platform}-${process.arch}`,
    "run this on linux or macOS",
  );
}
if (!existsSync(BINARY)) {
  fail(`${BINARY} is not built`, "cargo build -p onemessagebus-cli --locked --quiet");
}

const base = process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir();
mkdirSync(base, { recursive: true });
const work = mkdtempSync(join(base, "sdk-package-"));
const socketDir = mkdtempSync(join(tmpdir(), "omb-pkg-"));
process.on("exit", () => {
  rmSync(work, { recursive: true, force: true });
  rmSync(socketDir, { recursive: true, force: true });
});

// The child never inherits a binary override: the installed SDK must find its own.
const env = { ...process.env };
delete env.ONEMESSAGEBUS_BIN;

function run(command, args, options = {}) {
  try {
    return execFileSync(command, args, { encoding: "utf8", env, stdio: "pipe", ...options }).trim();
  } catch (error) {
    fail(
      `\`${command} ${args.join(" ")}\` failed:\n${[error.stdout, error.stderr].filter(Boolean).join("\n")}`,
      "read the output above; the full install is left nowhere, so rerun `bun run test:package` after fixing it",
    );
  }
}

const npmEnv = {
  ...env,
  npm_config_audit: "false",
  npm_config_fund: "false",
  npm_config_update_notifier: "false",
  npm_config_cache: join(work, "npm-cache"),
};
const npm = (args, cwd) => run("npm", args, { cwd, env: npmEnv });
const pack = (dir) => npm(["pack", "--silent", "--pack-destination", work], dir).split("\n").at(-1);

const sdkDir = run(process.execPath, [
  join(PACKAGE, "scripts/pack.mjs"),
  "--out",
  join(work, "sdk"),
]);
const packages = join(work, "packages");
const platformDir = run(process.execPath, [
  join(ROOT, "scripts/npm-build.mjs"),
  "platform",
  "--target",
  target,
  "--binary",
  BINARY,
  "--out",
  packages,
]);
const launcherDir = run(process.execPath, [
  join(ROOT, "scripts/npm-build.mjs"),
  "launcher",
  "--out",
  packages,
]);
const stamped = JSON.parse(readFileSync(join(sdkDir, "package.json"), "utf8"));
const platformName = JSON.parse(readFileSync(join(platformDir, "package.json"), "utf8")).name;

const consumer = join(work, "consumer");
mkdirSync(consumer);
writeFileSync(
  join(consumer, "package.json"),
  `${JSON.stringify({
    private: true,
    type: "module",
    dependencies: {
      "@onemessagebus/sdk": `file:../${pack(sdkDir)}`,
      "onemessagebus-cli": `file:../${pack(launcherDir)}`,
      [platformName]: `file:../${pack(platformDir)}`,
      zod: `file:../${pack(join(PACKAGE, "node_modules/zod"))}`,
    },
  })}\n`,
);
// Offline against an empty cache and an unreachable registry: everything the SDK
// needs at run time is one of the tarballs above, or the install fails.
npm(
  [
    "install",
    "--offline",
    "--registry",
    "http://127.0.0.1:9/",
    "--ignore-scripts",
    "--no-package-lock",
    "--omit=dev",
  ],
  consumer,
);

const installed = JSON.parse(
  readFileSync(join(consumer, "node_modules/@onemessagebus/sdk/package.json"), "utf8"),
);
if (installed.dependencies?.["onemessagebus-cli"] !== stamped.version) {
  fail(
    `the installed SDK pins onemessagebus-cli ${JSON.stringify(installed.dependencies?.["onemessagebus-cli"])}, not exactly ${stamped.version}`,
    "check scripts/pack.mjs",
  );
}

writeFileSync(
  join(consumer, "consume.mjs"),
  `import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { Client, CLI_VERSION, messages, ResidentTransport, SDK_VERSION, BusRefused } from "@onemessagebus/sdk";

const version = ${JSON.stringify(stamped.version)};
assert.equal(SDK_VERSION, version);
assert.equal(CLI_VERSION, version);

const config = { transportDir: "channel" };
const client = new Client({ config });
const kinds = await client.transports();
assert.ok(kinds.some((kind) => kind.kind === "local"), JSON.stringify(kinds));
const surface = { kind: "finding", message: "installed", source: "proposal", blocking: false };
const [sent] = await client.send("surfaces", surface);
assert.equal(sent.queue, "surfaces");
const claimed = await client.next("surfaces", { type: messages.AgentPlannerSurfaceV1 });
assert.equal(claimed.record.message, "installed");
assert.equal(await client.next("surfaces"), undefined);
await assert.rejects(client.send("nowhere", surface), BusRefused);

const socket = ${JSON.stringify(join(socketDir, "bus.sock"))};
const resident = new Client({ config, transport: new ResidentTransport({ socket }) });
const statuses = await resident.status("surfaces");
assert.equal(statuses[0].queue, "surfaces");
await resident.transport.close();
assert.equal(existsSync(socket), false, "closing stopped the resident it started");
console.log("ok");
`,
);
const said = run(process.execPath, ["consume.mjs"], { cwd: consumer });
if (said !== "ok") fail(`the installed SDK printed ${JSON.stringify(said)}`, "read it above");
console.log(
  `test:package: @onemessagebus/sdk ${stamped.version} installed and drove onemessagebus under node`,
);
