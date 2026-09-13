// How every case drives `scripts/release-probe.sh`: as the contract says a
// consumer drives it, and never any other way.
//
// One spawn environment for every case, so the environment restriction below is
// asserted by all of them — the ones that reach no registry and the ones that
// reach a registry this suite serves.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const PROBE = join(REPO_ROOT, "scripts", "release-probe.sh");
const WINDOWS = process.platform === "win32";

/// The environment the contract allows the probe, and nothing else: a search path
/// and a home directory. That the probe needs no credential is not asserted
/// anywhere — it is simply never given one.
function probeEnv(path) {
  const env = { PATH: path };
  if (WINDOWS) {
    // The home directory under the name this OS gives it, plus the one variable
    // curl on Windows cannot open its TLS backend without.
    env.USERPROFILE = process.env.USERPROFILE;
    env.SystemRoot = process.env.SystemRoot;
  } else {
    env.HOME = process.env.HOME;
  }
  return env;
}

/// A direct subprocess with no shell interposed, from the repository root. On
/// Windows the shebang is not honoured, so bash is named as the interpreter —
/// still an argv, never a command line for a shell to re-parse.
function command(args) {
  return WINDOWS ? ["bash", [PROBE, ...args]] : [PROBE, args];
}

/// Run the real probe with the environment the contract allows it.
export function probe(...args) {
  return probeWithPath(process.env.PATH, ...args);
}

/// The same spawn, with a search path of the caller's choosing — for the one
/// case whose subject *is* the search path: a machine with no transport on it.
export function probeWithPath(path, ...args) {
  const [file, argv] = command(args);
  const started = Date.now();
  const result = spawnSync(file, argv, {
    cwd: REPO_ROOT,
    env: probeEnv(path),
    encoding: "utf8",
    // Generously past the sixty seconds the contract allows, so a probe that
    // overran is reported as an overrun rather than as a killed process.
    timeout: 90_000,
  });
  assert.equal(result.error, undefined, `the probe could not be spawned: ${result.error}`);
  return { ...result, elapsedMs: Date.now() - started };
}

/// The probe, answered by a registry this suite serves on the loopback interface.
///
/// `routes` maps a registry URL's host and path (`/crates.io/api/v1/crates/x`) to
/// `{ status, body }`; a request for anything else answers 418, so a case that
/// reached the wrong URL fails loudly instead of reading as "no release".
///
/// What the contract governs runs for real: the probe, the real curl with every
/// flag the probe passes, an HTTP exchange, its status and its body. The one
/// thing moved is where the connection goes — a `curl` ahead of the real one on
/// the search path rewrites the probe's `https://<host>/<path>` to
/// `http://127.0.0.1:<port>/<host>/<path>` and execs the real binary with the rest
/// of the argv untouched. POSIX only: the rewrite is a bash script named `curl`.
export async function probeRegistry(routes, ...args) {
  const server = createServer((request, response) => {
    const route = routes[request.url];
    if (!route) {
      response.writeHead(418, { "content-type": "text/plain" });
      response.end(`this suite has no route for ${request.url}\n`);
      return;
    }
    response.writeHead(route.status ?? 200, { "content-type": "application/json" });
    response.end(typeof route.body === "string" ? route.body : JSON.stringify(route.body ?? {}));
  });
  await new Promise((listening) => server.listen(0, "127.0.0.1", listening));
  try {
    return await probeThrough(server.address().port, args);
  } finally {
    await new Promise((closed) => server.close(closed));
  }
}

/// The probe, pointed at a loopback port nothing is listening on.
export async function probeUnreachable(...args) {
  const server = createServer();
  await new Promise((listening) => server.listen(0, "127.0.0.1", listening));
  const { port } = server.address();
  await new Promise((closed) => server.close(closed));
  return probeThrough(port, args);
}

async function probeThrough(port, args) {
  const realCurl = execFileSync("bash", ["-c", "command -v curl"], { encoding: "utf8" }).trim();
  const bin = mkdtempSync(join(tmpdir(), "release-probe-loopback-"));
  try {
    const shim = join(bin, "curl");
    writeFileSync(
      shim,
      [
        "#!/usr/bin/env bash",
        "args=()",
        'for arg in "$@"; do',
        `  case $arg in https://*) arg="http://127.0.0.1:${port}/\${arg#https://}" ;; esac`,
        '  args+=("$arg")',
        "done",
        `exec '${realCurl}' "\${args[@]}"`,
        "",
      ].join("\n"),
    );
    chmodSync(shim, 0o755);
    return await spawnProbe(`${bin}${delimiter}${process.env.PATH}`, args);
  } finally {
    rmSync(bin, { recursive: true, force: true });
  }
}

/// `probeWithPath`, without blocking the event loop the registry answers on.
function spawnProbe(path, args) {
  const [file, argv] = command(args);
  return new Promise((settle, fail) => {
    const child = spawn(file, argv, { cwd: REPO_ROOT, env: probeEnv(path) });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8").on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.setEncoding("utf8").on("data", (chunk) => {
      stderr += chunk;
    });
    const overrun = setTimeout(() => child.kill(), 90_000);
    child.on("error", fail);
    child.on("close", (status) => {
      clearTimeout(overrun);
      settle({ status, stdout, stderr });
    });
  });
}

/// The third answer: a non-zero exit, nothing on stdout — a caller reads stdout
/// as the answer, so one byte there would be read as a version — and a reason
/// with a next action on stderr.
export function assertNotAnswered(result, because) {
  assert.notEqual(result.status, 0, `${because}: expected a non-zero exit, got 0`);
  assert.equal(result.stdout, "", `${because}: wrote to stdout while not answering`);
  assert.match(result.stderr, /ACTION:/, `${because}: gave no next action on stderr`);
}
