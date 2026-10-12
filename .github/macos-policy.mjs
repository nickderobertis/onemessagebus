#!/usr/bin/env node
// The workflow and its subprocess journeys share this fail-closed classifier.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { VERSION } from "../scripts/npm-build.mjs";

let configuration;
try {
  configuration = JSON.parse(readFileSync(new URL("./macos-policy.json", import.meta.url)));
} catch {
  configuration = undefined;
}
const version = { test: (value) => VERSION.test(value.replace(/\.dev(\d+)$/, "-dev.$1")) };
const own = (name) => /^onemessagebus(?:-.*)?$/.test(name) || name === "@onemessagebus/sdk";
function git(...args) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}
function parsed(path, text) {
  if (path.endsWith(".json")) return JSON.parse(text);
  return JSON.parse(
    execFileSync(
      "python3",
      ["-c", "import sys,json,tomllib; print(json.dumps(tomllib.loads(sys.stdin.read())))"],
      { input: text, encoding: "utf8" },
    ),
  );
}
function stripVersion(object) {
  if (!version.test(object.version ?? "")) throw new Error("not a plain package version");
  delete object.version;
}
function normalized(path, text) {
  if (
    /^(?:npm\/onemessagebus-sdk\/src\/version\.ts|python\/onemessagebus-sdk\/src\/onemessagebus\/_version\.py)$/.test(
      path,
    )
  ) {
    const declarations = path.endsWith(".ts")
      ? /^(export const (?:CLI_VERSION|SDK_VERSION): string = ")([^"]+)(";)$/gm
      : /^((?:CLI_VERSION|__version__) = ")([^"]+)(")$/gm;
    const names = new Set();
    const result = text.replace(declarations, (_, prefix, literal, suffix) => {
      if (!version.test(literal)) throw new Error("invalid generated version");
      const name = prefix.match(/CLI_VERSION|SDK_VERSION|__version__/)[0];
      if (names.has(name)) throw new Error("duplicate generated version declaration");
      names.add(name);
      return `${prefix}<version>${suffix}`;
    });
    if (names.size !== 2) throw new Error("unreadable generated SDK versions");
    return result;
  }
  const value = parsed(path, text);
  if (path === "Cargo.toml") stripVersion(value.workspace.package);
  else if (path === "Cargo.lock") {
    for (const pkg of value.package) if (own(pkg.name) && !pkg.source) stripVersion(pkg);
    // Internal dependency references must still name the same package.
    for (const pkg of value.package)
      if (pkg.dependencies)
        pkg.dependencies = pkg.dependencies.map((d) =>
          d.replace(/^(onemessagebus(?:-[\w-]+)?) \d+\.\d+\.\d+(?:[-+][\w.-]+)?$/, "$1 <version>"),
        );
  } else if (path === "package-lock.json") {
    if (value.version) stripVersion(value);
    for (const [key, pkg] of Object.entries(value.packages ?? {})) {
      if (key === "" || /^npm\/onemessagebus-(cli|sdk)$/.test(key)) {
        if (pkg.version) stripVersion(pkg);
      }
    }
  } else if (path === "uv.lock") {
    for (const pkg of value.package)
      if (own(pkg.name) && (pkg.source?.editable || pkg.source?.virtual)) stripVersion(pkg);
  } else if (path === "python/onemessagebus-sdk/pyproject.toml") {
    stripVersion(value.project);
    value.project.dependencies = value.project.dependencies.map((pin) =>
      pin.replace(/^(onemessagebus-cli)==\d+\.\d+\.\d+(?:\.dev\d+|[-+][\w.-]+)?$/, "$1==<version>"),
    );
  } else if (/^npm\/onemessagebus-(?:cli|sdk)\/package\.json$/.test(path)) {
    stripVersion(value);
  } else throw new Error(`not admitted version metadata: ${path}`);
  return JSON.stringify(value);
}
export function classify(event, base, head) {
  const prEvent = process.env.GITHUB_EVENT_NAME
    ? process.env.GITHUB_EVENT_NAME === "pull_request"
    : event?.pull_request !== undefined ||
      !event ||
      typeof event !== "object" ||
      !["ref", "release", "workflow_run", "schedule", "client_payload", "inputs"].some(
        (key) => key in event,
      );
  if (!prEvent)
    return { isReleaseOnly: false, macosRequired: false, reason: "non-PR automatic event" };
  const required = (reason) => ({ isReleaseOnly: false, macosRequired: true, reason });
  try {
    if (
      !configuration ||
      typeof configuration.repository !== "string" ||
      !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(configuration.repository) ||
      configuration.repository.includes("..") ||
      typeof configuration.branchPrefix !== "string" ||
      !/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(configuration.branchPrefix) ||
      configuration.branchPrefix.includes("..")
    )
      throw new Error("invalid repository or nonempty release branch prefix in macos-policy.json");
    const pr = event.pull_request;
    if (!pr?.head || !pr.base || !pr.user) return required("unreadable PR identity");
    if (
      !pr.head.ref.startsWith(configuration.branchPrefix) ||
      !pr.head.repo?.full_name ||
      pr.head.repo.full_name !== pr.base.repo?.full_name ||
      pr.base.repo.full_name !== configuration.repository
    )
      return required("ordinary or untrusted PR identity");
    if (!base || !head || base.startsWith("-") || head.startsWith("-"))
      return required("unreadable PR refs");
    const paths = git("diff", "--name-only", "-z", base, head, "--").split("\0").filter(Boolean);
    if (!paths.length) return required("empty PR diff");
    for (const path of paths) {
      const before = git("show", `${base}:${path}`);
      const after = git("show", `${head}:${path}`);
      if (/^(?:CHANGELOG\.md|crates\/[^/]+\/CHANGELOG\.md)$/.test(path)) continue;
      if (normalized(path, before) !== normalized(path, after))
        return required(`non-version content changed: ${path}`);
    }
    return {
      isReleaseOnly: true,
      macosRequired: false,
      reason: "same-repository release branch with semantic version/changelog-only diff",
    };
  } catch (error) {
    return required(`unknown PR classification: ${error.message}`);
  }
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const [command, ...args] = process.argv.slice(2);
    if (command !== "classify" || args.length !== 6)
      throw new Error("use classify --event PATH --base REF --head REF");
    const flags = Object.fromEntries(
      Array.from({ length: 3 }, (_, i) => [args[i * 2], args[i * 2 + 1]]),
    );
    if (!flags["--event"] || !flags["--base"] || !flags["--head"])
      throw new Error("missing classifier argument");
    let event;
    try {
      event = JSON.parse(readFileSync(flags["--event"], "utf8"));
    } catch {
      event = { pull_request: null };
    }
    console.log(JSON.stringify(classify(event, flags["--base"], flags["--head"])));
  } catch (error) {
    console.error(`macos-policy: ${error.message}`);
    process.exitCode = 2;
  }
}
