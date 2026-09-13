// A real Nx workspace in a temp directory, run by this repository's own wrappers.
//
// Shared by the suite whose subject is `scripts/nx` and `scripts/nx-affected.sh`.
// Both resolve the workspace from their own location, so driving them in place
// would run this repository's whole graph and truncate the log a gate in
// progress is writing. A workspace is built instead: the wrappers copied byte
// for byte, two projects, a git history with a real fork point, and this
// repository's installed `node_modules` linked in — the same pinned Nx, running
// for real, over a graph small enough that a case costs about a second.

import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const WRAPPERS = ["nx", "nx-affected.sh", "preserved-log.sh"];

/// The caller's environment, minus everything that could steer Nx, git or the
/// wrappers from outside a case. Each of these is really set where this suite
/// runs — under `scripts/nx` in the gate, on a GitHub runner — and each would
/// change an answer: an inherited `NX_*` can point Nx at another workspace,
/// `CI` and `GITHUB_BASE_REF` change which base the selection derives, and a
/// global git config can sign commits or install hooks.
export function isolatedEnv(extra = {}) {
  const env = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (name === "CI" || /^(NX_|GIT_|GITHUB_|ONEMESSAGEBUS_)/.test(name)) continue;
    env[name] = value;
  }
  return { ...env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1", ...extra };
}

export function git(cwd, ...args) {
  return execFileSync(
    "git",
    ["-c", "user.name=nx-workspace", "-c", "user.email=nx-workspace@example.invalid", "-c", "commit.gpgsign=false", ...args],
    { cwd, env: isolatedEnv(), encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  ).trim();
}

/// The upstream repository every checkout clones: `main` with projects `a` and
/// `b`, and `feature`, two commits past it that change only `a`. Two rather than
/// one, so a depth-1 clone of `feature` cannot reach the fork point.
///
/// Every target a case observes records its project in `$MARK_FILE`, so which
/// projects ran is read off the disk rather than out of Nx's own summary.
export function createUpstream(root) {
  const dir = join(root, "upstream");
  mkdirSync(join(dir, "scripts"), { recursive: true });
  for (const script of WRAPPERS) {
    copyFileSync(join(REPO_ROOT, "scripts", script), join(dir, "scripts", script));
  }
  writeFileSync(join(dir, "nx.json"), "{}\n");
  writeFileSync(join(dir, "package.json"), `${JSON.stringify({ name: "nx-workspace", private: true })}\n`);
  writeFileSync(join(dir, ".gitignore"), ".nx\n.logs\nnode_modules\n");
  const projects = {
    a: {
      mark: { command: 'echo a >> "$MARK_FILE"' },
      leak: { command: 'echo "the token is $NX_WORKSPACE_TOKEN"; exit 3' },
      // What the e2e journeys do inside `just check`: run the wrapper again from
      // inside a target the wrapper is running.
      nested: { command: "bash scripts/nx run b:mark" },
    },
    b: {
      mark: { command: 'echo b >> "$MARK_FILE"' },
    },
  };
  for (const [name, targets] of Object.entries(projects)) {
    mkdirSync(join(dir, name));
    writeFileSync(join(dir, name, "project.json"), `${JSON.stringify({ name, targets }, null, 2)}\n`);
    writeFileSync(join(dir, name, "src.txt"), `${name}\n`);
  }
  git(dir, "init", "--quiet", "--initial-branch=main");
  git(dir, "add", "--all");
  git(dir, "commit", "--quiet", "--message", "the fork point");
  const forkPoint = git(dir, "rev-parse", "HEAD");
  git(dir, "checkout", "--quiet", "-b", "feature");
  for (const change of ["one", "two"]) {
    writeFileSync(join(dir, "a", "src.txt"), `a ${change}\n`);
    git(dir, "commit", "--quiet", "--all", "--message", `change a: ${change}`);
  }
  git(dir, "checkout", "--quiet", "main");
  return { dir, forkPoint };
}

/// A checkout of `feature`, cloned the way a runner clones one. `shallow` is a
/// depth-1 fetch of both branches: `origin/main` is there, the fork point is not.
export function checkout(root, name, upstream, { shallow = false, linkNodeModules = true } = {}) {
  const dir = join(root, name);
  const depth = shallow ? ["--depth", "1", "--no-single-branch"] : [];
  git(root, "clone", "--quiet", "--branch", "feature", ...depth, `file://${upstream}`, dir);
  if (linkNodeModules) symlinkSync(join(REPO_ROOT, "node_modules"), join(dir, "node_modules"), "dir");
  return dir;
}

/// Run one of the workspace's wrappers as `just` does: `bash scripts/<name>`.
export function wrapper(cwd, script, args, env = {}) {
  const result = spawnSync("bash", [join("scripts", script), ...args], {
    cwd,
    env: isolatedEnv(env),
    encoding: "utf8",
    timeout: 120_000,
  });
  assert.equal(result.error, undefined, `scripts/${script} could not be run: ${result.error}`);
  return result;
}
