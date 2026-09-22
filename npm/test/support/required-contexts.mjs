// How every case drives `scripts/required-contexts.mjs`: as the subprocess CI
// runs, over a workflow file on disk, with the branch-protection read answered
// by a `gh` this suite puts on the search path.
//
// The deterministic tier is offline and credential-free, so no case here reaches
// GitHub. What is stood in for is exactly one thing — the transport — and only at
// its outermost edge: the script builds the same argv, spawns the same command
// name, and reads the same bytes back. Everything the script decides from those
// bytes is the real thing running.

import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..", "..", "..");
const SCRIPT = join(REPO_ROOT, "scripts", "required-contexts.mjs");

/// The workflow whose jobs emit the contexts branch protection must require —
/// the real one, never a copy: a gate proven only against a fixture proves
/// nothing about the file it runs on.
export const CI_WORKFLOW = join(REPO_ROOT, ".github", "workflows", "ci.yml");

/// The shim is a POSIX script named `gh`, which Windows will not exec.
export const SHIMMABLE = process.platform !== "win32";

/// This repository's `main` branch protection, read from GitHub on 2026-09-22
/// with `gh api repos/nickderobertis/onemessagebus/branches/main/protection` and
/// recorded verbatim. It is the state issue #123 describes: twelve contexts,
/// neither of them a `sdk-install` leg.
export function recordedProtection() {
  return JSON.parse(readFileSync(join(HERE, "branch-protection-main.json"), "utf8"));
}

/// That same recorded answer with a different set of required contexts — the one
/// field a case varies. Both shapes GitHub answers with are set together, because
/// a real answer carries both and a case must not depend on which one is read.
export function protectionRequiring(contexts) {
  const document = recordedProtection();
  document.required_status_checks.contexts = [...contexts];
  document.required_status_checks.checks = contexts.map((context) => ({ context, app_id: null }));
  return document;
}

/// A `gh` that answers the protection read with `answer`, or refuses it the way
/// the real one refuses a token without repository-administration reach.
///
/// Anything but the protection endpoint is refused loudly rather than answered,
/// so a case that asked the wrong question fails as that instead of reading as
/// whichever outcome it wanted.
function ghShim(bin, { answer, refusal }) {
  const path = join(bin, "gh");
  const body = refusal
    ? [`  printf '%s\\n' ${JSON.stringify(refusal)} >&2`, "  exit 1"]
    : [`  printf '%s' ${JSON.stringify(JSON.stringify(answer))}`, "  exit 0"];
  writeFileSync(
    path,
    [
      "#!/usr/bin/env bash",
      'if [[ $1 == api && $* == *"/branches/"*"/protection"* ]]; then',
      ...body,
      "fi",
      "printf 'this suite has no answer for: gh %s\\n' \"$*\" >&2",
      "exit 64",
      "",
    ].join("\n"),
  );
  chmodSync(path, 0o755);
}

/// Run the real script with the real argv.
///
/// `gh` selects what the protection read gets back: an `answer` document, a
/// `refusal` the read fails with, or — by default — a `gh` that answers nothing,
/// so a case meant to stay offline fails loudly if it ever reaches for one.
export function requiredContexts(args, gh = {}) {
  const bin = mkdtempSync(join(tmpdir(), "required-contexts-gh-"));
  try {
    if (SHIMMABLE) ghShim(bin, gh);
    const result = spawnSync(process.execPath, [SCRIPT, ...args], {
      cwd: REPO_ROOT,
      encoding: "utf8",
      env: { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, GH_TOKEN: "" },
      timeout: 60_000,
    });
    assert.equal(result.error, undefined, `the script could not be spawned: ${result.error}`);
    return result;
  } finally {
    rmSync(bin, { recursive: true, force: true });
  }
}

/// What the script derives from a workflow — the contexts it says that workflow
/// emits, which is also the list its failures tell a maintainer to set. Nothing
/// in this suite transcribes one.
///
/// The spawn result rather than the list, because a refusal is an outcome a case
/// asserts about: a workflow this gate cannot derive from must fail a named test
/// rather than crash the file as it loads.
export function deriveFrom(workflow = CI_WORKFLOW) {
  return requiredContexts(["--list", "--workflow", workflow]);
}

/// A refusal is a non-zero exit, nothing on stdout — stdout is where `--list`
/// puts an answer a maintainer pastes into a setting — and a reason with a next
/// action on stderr.
export function assertRefused(result, because) {
  assert.notEqual(result.status, 0, `${because}: expected a non-zero exit, got 0`);
  assert.equal(result.stdout, "", `${because}: wrote to stdout while refusing`);
  assert.match(result.stderr, /ACTION: /, `${because}: gave no next action on stderr`);
}

/// A copy of the real ci.yml with one edit applied to its text, so a case can ask
/// what this gate would say about a workflow this repository does not have.
export function workflowWith(edit) {
  const original = readFileSync(CI_WORKFLOW, "utf8");
  const edited = edit(original);
  assert.notEqual(edited, original, "the edit changed nothing in ci.yml");
  const directory = mkdtempSync(join(tmpdir(), "required-contexts-workflow-"));
  const path = join(directory, "ci.yml");
  writeFileSync(path, edited);
  return { path, directory };
}
