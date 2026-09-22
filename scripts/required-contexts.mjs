#!/usr/bin/env node
// Holds main's branch protection to the contexts ci.yml actually emits.
//
// Branch protection is the authoritative list of what must be green to merge,
// and it lives on GitHub rather than in this tree. Nothing here may restate it:
// a restated list drifts, and it drifts silently in the dangerous direction —
// a document or a test claiming a job gates merges while protection has never
// heard of it, which is how a broken journey merges. So this script derives one
// side and reads the other, and refuses anything but an exact match.
//
// The derived side is every context ci.yml emits: one per job, and for a matrix
// job one per leg, named the way GitHub names a matrix check run
// (`install (macos-latest)`). Add a job or a matrix leg and this fails until
// protection requires it; drop one and it fails the other way.
//
// The read side is `GET /repos/{repo}/branches/{branch}/protection`, which needs
// repository-administration read — a reach `GITHUB_TOKEN` does not have at any
// `permissions:` setting. `RELEASE_PLZ_TOKEN` is the PAT that does, and every
// repository here already holds it, so nothing new is provisioned. A refused read
// is reported as a refused read, naming that secret and the permission: reading
// it as "no contexts required" would turn the one gate that can catch this drift
// into a gate that passes when it cannot see.
//
// Usage:
//   node scripts/required-contexts.mjs --repo OWNER/NAME [--branch main] [--workflow PATH]
//   node scripts/required-contexts.mjs --list   # the derived contexts, one per line
//
// `--list` reaches no network and needs no credential: it is what a maintainer
// pastes into the branch-protection setting, and what this script's own tests
// drive to build a protection answer without transcribing a list of their own.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_WORKFLOW = join(REPO_ROOT, ".github", "workflows", "ci.yml");

/// The one secret that can read branch protection, and what it needs to be able
/// to. Named in every refusal, because the fix is always to provision or widen
/// exactly this.
const SECRET = "RELEASE_PLZ_TOKEN";
const PERMISSION =
  "repository administration (read), which is what branch-protection read requires";

/// This repository's exit codes are a contract: `0` did it, `1` a well-formed
/// no, `2` refused input. Drift is the well-formed no — the comparison ran and
/// its answer is that protection does not require these contexts. Everything
/// else here is input this gate refuses to work from: an argument it cannot
/// read, a workflow it cannot derive from, a protection read it could not make.
const REFUSED_INPUT = 2;
const WELL_FORMED_NO = 1;

class Refusal extends Error {
  constructor(message, action, code = REFUSED_INPUT) {
    super(message);
    this.action = action;
    this.code = code;
  }
}

/// A GitHub owner/repository pair and a branch name, as the API will take them.
/// Both reach the endpoint this script builds, so both are checked at the
/// boundary rather than interpolated on trust: a path segment that is not one
/// would silently ask a different question.
function identifier(flag, value, pattern) {
  if (typeof value !== "string" || !pattern.test(value) || value.includes("..")) {
    throw usage(
      `\`${flag}\` is not a value this script will put in an API path: ${JSON.stringify(value)}`,
    );
  }
  return value;
}

function usage(problem) {
  return new Refusal(
    problem,
    "run `node scripts/required-contexts.mjs --repo OWNER/NAME [--branch main] [--workflow PATH]`, or `--list` for the derived contexts alone",
  );
}

function parseArgv(argv) {
  const options = { branch: "main", workflow: DEFAULT_WORKFLOW, repo: null, list: false };
  for (let at = 0; at < argv.length; at += 1) {
    const flag = argv[at];
    const value = () => {
      at += 1;
      if (at >= argv.length) throw usage(`\`${flag}\` takes a value`);
      return argv[at];
    };
    if (flag === "--list") options.list = true;
    else if (flag === "--repo") options.repo = identifier(flag, value(), /^[\w.-]+\/[\w.-]+$/);
    else if (flag === "--branch") options.branch = identifier(flag, value(), /^[\w./-]+$/);
    else if (flag === "--workflow") options.workflow = value();
    else throw usage(`unknown argument \`${flag}\``);
  }
  return options;
}

/// A matrix value GitHub can put in a check-run name: a scalar it renders as
/// itself. An object leg (from an `include` that adds a dimension) or a value
/// built by an expression is refused rather than guessed at — a context name
/// guessed wrong is the very drift this script exists to catch.
function leg(job, key, value) {
  if (typeof value === "string" && value.includes("${{")) {
    throw new Refusal(
      `job \`${job}\`'s matrix value for \`${key}\` is an expression (${value}), so the context names it emits cannot be derived from the workflow alone`,
      "give the matrix literal values, or name the job's contexts some other way this script can read",
    );
  }
  if (value === null || typeof value === "object") {
    throw new Refusal(
      `job \`${job}\`'s matrix value for \`${key}\` is not a scalar (${JSON.stringify(value)}), so the context name GitHub would render for it cannot be derived`,
      "flatten the matrix to scalar legs, so each check run's name is readable from the workflow",
    );
  }
  return String(value);
}

/// The legs GitHub expands a `strategy.matrix` into, in declaration order — the
/// order it also renders them in a check-run name.
function matrixLegs(job, matrix) {
  for (const unsupported of ["include", "exclude"]) {
    if (matrix[unsupported] !== undefined) {
      throw new Refusal(
        `job \`${job}\`'s matrix uses \`${unsupported}\`, whose expansion this script does not derive`,
        `express \`${job}\`'s matrix as plain dimensions, or this gate cannot know which contexts it emits`,
      );
    }
  }
  let legs = [[]];
  for (const [key, values] of Object.entries(matrix)) {
    if (!Array.isArray(values)) {
      throw new Refusal(
        `job \`${job}\`'s matrix dimension \`${key}\` is not a list`,
        "give every matrix dimension a list of scalar values",
      );
    }
    legs = legs.flatMap((soFar) => values.map((value) => [...soFar, leg(job, key, value)]));
  }
  return legs;
}

/// Every context the workflow emits, in the workflow's own job order.
///
/// A matrix job with a job-level `if` is refused outright. Skipped there it
/// reports one bare `sdk-install`, never `sdk-install (macos-latest)`, so a
/// per-leg context branch protection requires would stay pending for ever and
/// the pull request could never merge. The skip belongs inside the job, on its
/// steps, where every leg still reports.
function contextsEmittedBy(workflowText, where) {
  let workflow;
  try {
    workflow = parse(workflowText);
  } catch (error) {
    throw new Refusal(
      `${where} is not readable YAML: ${error.message}`,
      "fix the workflow's syntax; until it parses, nothing can say which contexts it emits",
    );
  }
  const jobs = workflow?.jobs;
  if (!jobs || typeof jobs !== "object") {
    throw new Refusal(
      `${where} declares no jobs`,
      "point --workflow at the workflow that gates merges",
    );
  }
  const contexts = [];
  for (const [id, job] of Object.entries(jobs)) {
    const name = job?.name ?? id;
    if (typeof name !== "string" || name.includes("${{")) {
      throw new Refusal(
        `job \`${id}\`'s name is an expression (${name}), so the context it reports cannot be derived from the workflow alone`,
        `give \`${id}\` a literal name`,
      );
    }
    const matrix = job?.strategy?.matrix;
    if (!matrix) {
      contexts.push(name);
      continue;
    }
    if (job.if !== undefined) {
      throw new Refusal(
        `job \`${id}\` is a matrix job with a job-level \`if\`, so a change that skips it reports one bare \`${name}\` and none of its per-leg contexts`,
        `move \`${id}\`'s condition onto its steps, so every leg is scheduled and reports its context whatever the diff reaches`,
      );
    }
    for (const values of matrixLegs(id, matrix)) {
      contexts.push(`${name} (${values.join(", ")})`);
    }
  }
  // Two jobs (or two legs) that render one name would collapse in the
  // comparison, and the set protection required would silently cover one fewer
  // check than the workflow runs.
  const duplicated = contexts.filter((context, at) => contexts.indexOf(context) !== at);
  if (duplicated.length > 0) {
    throw new Refusal(
      `${where} emits the same context more than once: ${[...new Set(duplicated)].map((context) => `\`${context}\``).join(", ")}`,
      "give each job a distinct name, so every check run protection requires is one this gate can account for",
    );
  }
  if (contexts.length === 0) {
    throw new Refusal(
      `${where} emits no contexts`,
      "point --workflow at the workflow that gates merges",
    );
  }
  return contexts;
}

/// The protection document GitHub answers with, read for the contexts alone.
/// `checks` is the current shape and `contexts` the deprecated one; either is
/// accepted, neither is required to be present in the other's place.
function requiredContexts(document, where) {
  const required = document?.required_status_checks;
  if (!required) {
    throw new Refusal(
      `${where} requires no status checks at all, while the workflow emits contexts to require`,
      "turn on required status checks for this branch and set them to the contexts `--list` gives; until then nothing holds a merge to a green CI run",
      WELL_FORMED_NO,
    );
  }
  const named = (values, field) =>
    values.map((value, at) => {
      const context = field === "checks" ? value?.context : value;
      if (typeof context !== "string" || context.trim() === "") {
        throw new Refusal(
          `${where} lists a required check with no context name at \`${field}[${at}]\`: ${JSON.stringify(value)}`,
          "check the API answer by hand; this gate will not guess at which check an unnamed entry stands for",
        );
      }
      return context;
    });
  if (Array.isArray(required.checks)) return named(required.checks, "checks");
  if (Array.isArray(required.contexts)) return named(required.contexts, "contexts");
  throw new Refusal(
    `${where} answered with no readable list of required contexts`,
    "check the API answer by hand; this gate cannot compare against a shape it does not recognise",
  );
}

/// The live protection, read through `gh` so the credential handling is the CLI's.
function readProtection(repo, branch) {
  if (!repo) throw usage("--repo OWNER/NAME is required to read branch protection");
  const refused = (detail) =>
    new Refusal(
      `could not read ${repo}'s \`${branch}\` branch protection: ${detail}`,
      `give this step \`GH_TOKEN: \${{ secrets.${SECRET} }}\` and make sure that token still grants ${PERMISSION} on ${repo}; GITHUB_TOKEN cannot read branch protection at any \`permissions:\` setting, so an unset or under-scoped ${SECRET} is the usual cause`,
    );
  let answer;
  try {
    answer = execFileSync(
      "gh",
      [
        "api",
        "-H",
        "Accept: application/vnd.github+json",
        `repos/${repo}/branches/${branch}/protection`,
      ],
      { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    );
  } catch (error) {
    throw refused(String(error.stderr || error.message).trim());
  }
  try {
    return JSON.parse(answer);
  } catch (_error) {
    throw refused("the answer was not JSON");
  }
}

/// Exactly the derived contexts, or a refusal naming every name that differs.
function compare(emitted, required, where) {
  const emittedSet = new Set(emitted);
  const requiredSet = new Set(required);
  const missing = emitted.filter((context) => !requiredSet.has(context));
  const extra = required.filter((context) => !emittedSet.has(context));
  if (missing.length === 0 && extra.length === 0) return;
  const lines = [`${where} does not require exactly the contexts the workflow emits.`];
  for (const context of missing) {
    lines.push(`  missing: \`${context}\` is emitted by the workflow and not required to merge`);
  }
  for (const context of extra) {
    lines.push(
      `  extra:   \`${context}\` is required to merge and emitted by no job in the workflow`,
    );
  }
  throw new Refusal(
    lines.join("\n"),
    `set this branch's required status checks to exactly these ${emitted.length}:\n${emitted.map((context) => `  ${context}`).join("\n")}`,
    WELL_FORMED_NO,
  );
}

/// The workflow's text, or a refusal naming the path — an unreadable workflow is
/// a gate that cannot see, and it must say so rather than dump a stack trace.
function readWorkflow(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (error) {
    throw new Refusal(
      `could not read the workflow at ${path}: ${error.message}`,
      "point --workflow at the workflow whose jobs emit the contexts branch protection requires",
    );
  }
}

function main(argv) {
  const options = parseArgv(argv);
  const emitted = contextsEmittedBy(readWorkflow(options.workflow), options.workflow);
  if (options.list) {
    process.stdout.write(`${emitted.join("\n")}\n`);
    return;
  }
  const where = `${options.repo}'s \`${options.branch}\` branch protection`;
  const required = requiredContexts(readProtection(options.repo, options.branch), where);
  compare(emitted, required, where);
  process.stdout.write(
    `required-contexts: ${where} requires exactly the ${emitted.length} contexts ${options.workflow} emits\n`,
  );
}

try {
  main(process.argv.slice(2));
} catch (error) {
  if (!(error instanceof Refusal)) throw error;
  process.stderr.write(`required-contexts: ${error.message}\nACTION: ${error.action}\n`);
  process.exit(error.code);
}
