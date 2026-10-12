// Repository-private resolver. Unknown calls or dynamic outputs are errors.
import { createHash } from "node:crypto";
import {
  readFileSync,
  readdirSync,
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  chmodSync,
  rmSync,
  copyFileSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve, join, delimiter } from "node:path";
import { tmpdir } from "node:os";
import { parse } from "yaml";
import { Lexer, Parser, Evaluator, data } from "@actions/expressions";
import { truthy } from "@actions/expressions/result";
export const root = resolve(import.meta.dirname, "..");
export const read = (path) => readFileSync(resolve(root, path), "utf8");
export const workflows = () =>
  Object.fromEntries(
    readdirSync(resolve(root, ".github/workflows"))
      .filter((f) => /\.ya?ml$/.test(f))
      .map((f) => [f, parse(read(`.github/workflows/${f}`))]),
  );
const convert = (x) =>
  x === null || x === undefined
    ? new data.Null()
    : typeof x === "string"
      ? new data.StringData(x)
      : typeof x === "boolean"
        ? new data.BooleanData(x)
        : typeof x === "number"
          ? new data.NumberData(x)
          : Array.isArray(x)
            ? new data.Array(...x.map(convert))
            : new data.Dictionary(
                ...Object.entries(x).map(([key, value]) => ({ key, value: convert(value) })),
              );
export function expression(source, context) {
  source = String(source).replace(/^\s*\$\{\{([\s\S]*)\}\}\s*$/, "$1");
  const statuses = new Map(
    ["always", "success", "failure", "cancelled"].map((name) => [
      name,
      {
        name,
        minArgs: 0,
        maxArgs: 0,
        call: () =>
          new data.BooleanData(
            name === "always" ||
              (name === "success"
                ? Object.values(context.needs ?? {}).every((n) => n.result === "success")
                : Object.values(context.needs ?? {}).some(
                    (n) => n.result === (name === "failure" ? "failure" : "cancelled"),
                  )),
          ),
      },
    ]),
  );
  const tree = new Parser(new Lexer(source).lex().tokens, Object.keys(context), [
    ...statuses.values(),
  ]).parse();
  return new Evaluator(tree, convert(context), statuses).evaluate();
}
export function render(value, context) {
  if (typeof value !== "string") return value;
  return value.replace(/\$\{\{([\s\S]*?)\}\}/g, (_, source) =>
    expression(source, context).coerceString(),
  );
}
const asList = (x) => (x === undefined ? [] : Array.isArray(x) ? x : [x]);
export function matrix(definition, context) {
  if (!definition) return [{}];
  if (typeof definition === "string") throw new Error("unresolved matrix expression");
  let legs = [{}];
  for (const [key, raw] of Object.entries(definition).filter(
    ([key]) => !["include", "exclude"].includes(key),
  )) {
    const values =
      typeof raw === "string" ? JSON.parse(expression(raw, context).coerceString()) : raw;
    if (!Array.isArray(values) || !values.length)
      throw new Error(`empty or unresolved matrix dimension ${key}`);
    legs = legs.flatMap((leg) => values.map((value) => ({ ...leg, [key]: value })));
  }
  legs = legs.filter(
    (leg) =>
      !asList(definition.exclude).some((ex) => Object.entries(ex).every(([k, v]) => leg[k] === v)),
  );
  let include = definition.include ?? [];
  if (typeof include === "string") {
    const value = expression(include, context);
    if (value.kind !== data.Kind.Array) throw new Error("unresolved matrix include");
    include = value
      .values()
      .map((entry) =>
        Object.fromEntries(entry.pairs().map(({ key, value }) => [key, value.coerceString()])),
      );
  }
  if (!Object.keys(definition).some((k) => !["include", "exclude"].includes(k))) {
    if (!include.length) throw new Error("empty include-only matrix cannot resolve a runner");
    return include;
  }
  const original = legs.map((leg) => ({ ...leg }));
  for (const item of include) {
    let merged = false;
    original.forEach((base, i) => {
      if (Object.entries(item).every(([k, v]) => !(k in base) || base[k] === v)) {
        Object.assign(legs[i], item);
        merged = true;
      }
    });
    if (!merged) legs.push(item);
  }
  return legs;
}
export function triggered(workflow, event) {
  const on = workflow.on;
  if (typeof on === "string") return on === event.name;
  if (Array.isArray(on)) return on.includes(event.name);
  if (!on || !(event.name in on)) return false;
  const options = on[event.name];
  if (
    event.name === "pull_request" &&
    !options?.types &&
    !["opened", "synchronize", "reopened"].includes(event.action)
  )
    return false;
  if (options?.types && !options.types.includes(event.action)) return false;
  for (const field of ["branches", "tags", "branches-ignore", "tags-ignore"]) {
    if (
      options?.[field] &&
      (!Array.isArray(options[field]) ||
        options[field].some(
          (pattern) => typeof pattern !== "string" || /[!*?[\]{}+]/.test(pattern),
        ))
    )
      throw new Error(`unresolved ${field} glob filter`);
  }
  if (options?.["branches-ignore"] || options?.["tags-ignore"])
    throw new Error("unresolved ignored ref filter");
  if (options?.branches && !options.branches.includes(event.branch)) return false;
  if (options?.tags && !options.tags.includes(event.tag)) return false;
  if (options?.workflows && !options.workflows.includes(event.workflow)) return false;
  if (options?.paths || options?.["paths-ignore"]) throw new Error("unresolved event path filter");
  return true;
}
const captureMatrices = new Map();
export function screencompOutput(job, context) {
  const step = job.steps.find((step) => step.id === "read");
  if (job.outputs?.matrix !== `\${{ steps.read.outputs.matrix }}` || !step?.run)
    throw new Error("unresolved screencomp matrix producer");
  const env = Object.fromEntries(
    Object.entries(step.env ?? {}).map(([key, value]) => [key, String(render(value, context))]),
  );
  const key = JSON.stringify([step.run, env]);
  if (captureMatrices.has(key)) return captureMatrices.get(key);
  const work = mkdtempSync(
    join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "screencomp-output-"),
  );
  try {
    mkdirSync(join(work, "bin"));
    // Only the external screencomp CLI is doubled. Its public config supplies
    // the arches; the snapshot's actual bash/jq producer handles every input.
    const shim = join(work, "bin/screencomp");
    writeFileSync(
      shim,
      `#!/usr/bin/env python3
import json,os,sys,tomllib
if sys.argv[1:] != ['arches','--format','json']: raise SystemExit('unexpected screencomp command')
with open(os.path.join(os.environ['ONEMESSAGEBUS_WORKFLOW_ROOT'],'screencomp.toml'),'rb') as source:
 print(json.dumps(tomllib.load(source)['capture']['arches']))
`,
    );
    chmodSync(shim, 0o755);
    const output = join(work, "output");
    writeFileSync(output, "");
    execFileSync("bash", ["-euo", "pipefail", "-c", step.run], {
      cwd: root,
      encoding: "utf8",
      env: {
        ...process.env,
        ...env,
        PATH: join(work, "bin") + delimiter + process.env.PATH,
        GITHUB_OUTPUT: output,
        ONEMESSAGEBUS_WORKFLOW_ROOT: root,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const lines = readFileSync(output, "utf8")
      .trim()
      .split("\n")
      .filter((line) => line.startsWith("matrix="));
    if (lines.length !== 1) throw new Error("screencomp producer did not emit one matrix");
    const matrix = lines[0].slice("matrix=".length);
    const include = JSON.parse(matrix).include;
    if (!Array.isArray(include) || !include.length)
      throw new Error("screencomp producer emitted an empty or invalid matrix");
    if (include.some((row) => !row || !["x86_64", "aarch64", "arm64"].includes(row.arch)))
      throw new Error("unsupported architecture in screencomp matrix");
    captureMatrices.set(key, matrix);
    return matrix;
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}
function declaredOutput(job, name, step, source) {
  const producer = job.steps?.find((candidate) => candidate.id === step);
  if (
    !producer ||
    producer.if !== undefined ||
    producer["continue-on-error"] !== undefined ||
    job["continue-on-error"] !== undefined
  )
    throw new Error(`conditional or masked output producer: ${name}`);
  if (
    job.outputs?.[name] !== `\${{ steps.${step}.outputs.${name} }}` ||
    !job.steps?.find((candidate) => candidate.id === step)?.run?.includes(source)
  )
    throw new Error(`unresolved production output contract: ${name}`);
}
const policyOutputs = new Map();
export function changesOutputs(job, classification, fixture) {
  declaredOutput(job, "crate", "affected", 'echo "crate=$(just affected-crate)"');
  declaredOutput(job, "sdk", "affected", 'echo "sdk=$(just affected-sdk)"');
  declaredOutput(job, "macos", "policy", 'echo "macos=');
  declaredOutput(job, "release-only", "policy", 'echo "release-only=');
  const policy = job.steps.find((step) => step.id === "policy").run;
  if (
    !policy.includes(
      'node .github/macos-policy.mjs classify --event "$GITHUB_EVENT_PATH" --base "$BASE" --head "$HEAD"',
    ) ||
    !policy.includes(")).macosRequired") ||
    !policy.includes(")).isReleaseOnly")
  )
    throw new Error("unresolved production classifier command");
  if (!fixture?.directory || !fixture.base || !fixture.head)
    throw new Error("missing git history for production classifier step");
  const key = JSON.stringify([policy, fixture, classification]);
  if (!policyOutputs.has(key)) {
    const directory = mkdtempSync(join(fixture.directory, ".git/policy-step-"));
    const eventPath = join(directory, "event.json"),
      output = join(directory, "output");
    const payload = structuredClone(fixture.event);
    if (payload.pull_request?.head && payload.pull_request?.base) {
      payload.pull_request.head.sha = fixture.head;
      payload.pull_request.base.sha = fixture.base;
    }
    writeFileSync(eventPath, JSON.stringify(payload));
    const step = job.steps.find((step) => step.id === "policy");
    const declaredEnv = Object.fromEntries(
      Object.entries(step.env ?? {}).map(([key, value]) => [
        key,
        render(value, { github: { event: payload } }),
      ]),
    );
    // Unknown PR payloads deliberately test the classifier's unreadable-input refusal.
    if (
      payload.pull_request?.head &&
      payload.pull_request?.base &&
      (declaredEnv.BASE !== fixture.base || declaredEnv.HEAD !== fixture.head)
    )
      throw new Error("production classifier revision bindings disagree with the git history");
    mkdirSync(join(directory, ".github"));
    mkdirSync(join(directory, "scripts"));
    for (const path of [
      ".github/macos-policy.mjs",
      ".github/macos-policy.json",
      "scripts/npm-build.mjs",
    ])
      copyFileSync(join(root, path), join(directory, path));
    try {
      execFileSync("bash", ["-e", "-o", "pipefail", "-c", policy], {
        cwd: directory,
        encoding: "utf8",
        env: {
          ...process.env,
          GIT_DIR: join(fixture.directory, ".git"),
          GIT_WORK_TREE: fixture.directory,
          GITHUB_EVENT_NAME: "pull_request",
          GITHUB_EVENT_PATH: eventPath,
          GITHUB_OUTPUT: output,
          BASE: declaredEnv.BASE || fixture.base,
          HEAD: declaredEnv.HEAD || fixture.head,
        },
      });
      const produced = Object.fromEntries(
        readFileSync(output, "utf8")
          .trim()
          .split("\n")
          .map((line) => line.split("=")),
      );
      if (
        produced.macos !== String(classification.macosRequired) ||
        produced["release-only"] !== String(classification.isReleaseOnly)
      )
        throw new Error("production classifier step outputs disagree with classification");
      policyOutputs.set(key, produced);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  }
  return { crate: "true", sdk: "true", ...policyOutputs.get(key) };
}
function guardOutputs(job, variables) {
  const name = Object.hasOwn(job.outputs ?? {}, "publish") ? "publish" : "enabled";
  declaredOutput(job, name, "check", `echo "${name}=true"`);
  const step = job.steps.find((step) => step.id === "check");
  if (!step.run.includes(`echo "${name}=false"`))
    throw new Error("unresolved guard negative output");
  const work = mkdtempSync(
    join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "guard-output-"),
  );
  try {
    const output = join(work, "output");
    writeFileSync(output, "");
    execFileSync("bash", ["-euo", "pipefail", "-c", step.run], {
      cwd: root,
      env: {
        ...process.env,
        TOKEN: name === "publish" && variables.CARGO_PUBLISH === "false" ? "" : "present",
        GITHUB_OUTPUT: output,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const lines = readFileSync(output, "utf8").trim().split("\n");
    if (lines.length !== 1 || !new RegExp(`^${name}=(true|false)$`).test(lines[0]))
      throw new Error("unresolved actual guard output");
    return { [name]: lines[0].split("=")[1] };
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}
const screencompCall =
  "nickderobertis/screencomp/.github/workflows/visual-docs-reusable.yml@v0.4.8";
function called(reference) {
  if (reference === screencompCall) {
    const pinned = JSON.parse(read(".github/reusable-workflows.json"))[reference];
    if (!pinned) throw new Error(`unresolved pinned reusable call: ${reference}`);
    const source = read(pinned.path).replace(
      /^ *# llmlint: ignore\[(?:comments_earn_their_place|boundary_inputs_validated)\] Upstream snapshot text[^\n]*\n/gm,
      "",
    );
    if (createHash("sha256").update(source).digest("hex") !== pinned.sha256)
      throw new Error(`reusable snapshot digest mismatch: ${reference}`);
    return parse(source);
  }
  if (reference.startsWith("./.github/workflows/") && !reference.includes(".."))
    return parse(read(reference));
  throw new Error(`unresolved reusable workflow call: ${reference}`);
}
export function resolveRun(file, workflow, event, classification, variables = {}, stack = []) {
  if (stack.includes(file)) throw new Error(`cyclic reusable call: ${file}`);
  const github = {
    event_name: event.name,
    event: event.payload ?? { action: event.action },
    head_ref: event.headRef ?? "feature",
    repository: "nickderobertis/onemessagebus",
    ref: event.ref ?? "refs/heads/main",
    sha: event.sha ?? "",
  };
  github.event.action = event.action;
  const defaults = Object.fromEntries(
    Object.entries(
      workflow.on?.workflow_call?.inputs ?? workflow.on?.workflow_dispatch?.inputs ?? {},
    ).map(([k, v]) => [k, v.default ?? ""]),
  );
  const context = {
    github,
    inputs: { ...defaults, ...event.inputs },
    vars: variables,
    needs: {},
    env: workflow.env ?? {},
    matrix: {},
    steps: {},
  };
  const jobs = workflow.jobs;
  if (!jobs) throw new Error(`workflow ${file} has no jobs`);
  const pending = new Set(Object.keys(jobs)),
    results = {},
    rows = [];
  while (pending.size) {
    let progress = false;
    for (const id of pending) {
      const job = jobs[id],
        needs = asList(job.needs);
      if (needs.some((n) => !results[n])) continue;
      progress = true;
      pending.delete(id);
      context.needs = Object.fromEntries(needs.map((n) => [n, results[n]]));
      const explicitStatus = /\b(always|success|failure|cancelled)\s*\(/.test(job.if ?? "");
      const allowed =
        (!job.if || truthy(expression(job.if, context))) &&
        (explicitStatus || needs.every((n) => results[n].result === "success"));
      if (!allowed) {
        results[id] = { result: "skipped", outputs: {} };
        rows.push({
          workflow: file,
          event: event.label,
          job: id,
          runner: null,
          result: "skipped",
          targets: [],
        });
        continue;
      }
      if (job.uses) {
        if (job.strategy?.matrix !== undefined)
          throw new Error(
            "unsupported matrix on reusable workflow call; resolve every call leg before recording telemetry",
          );
        const nested = called(job.uses);
        const inputs = Object.fromEntries(
          Object.entries(job.with ?? {}).map(([k, v]) => [k, render(v, context)]),
        );
        rows.push(
          ...resolveRun(job.uses, nested, { ...event, inputs }, classification, variables, [
            ...stack,
            file,
          ]),
        );
      } else {
        for (const leg of matrix(job.strategy?.matrix, context)) {
          context.matrix = leg;
          const runner = render(job["runs-on"], context);
          const labels = asList(runner);
          if (
            !labels.length ||
            labels.some((l) => typeof l !== "string" || !l || l.includes("${{"))
          )
            throw new Error(`unresolved runner: ${file}/${id}`);
          if (labels.some((l) => !/^(ubuntu|windows|macos)-[\w.-]+$/.test(l)))
            throw new Error(`unknown runner label: ${labels}`);
          const selectedSteps = (job.steps ?? []).filter(
            (step) => !step.if || truthy(expression(step.if, context)),
          );
          const commands = selectedSteps.map((step) => step.run ?? "").join("\n");
          const targets = leg.target
            ? [leg.target]
            : [
                ...new Set(
                  [
                    ...commands.matchAll(
                      /\b(?:x86_64|aarch64)-(?:apple-darwin|unknown-linux-gnu|pc-windows-msvc)\b/g,
                    ),
                  ].map((m) => m[0]),
                ),
              ];
          rows.push({
            workflow: file,
            event: event.label,
            job: id,
            display: render(job.name ?? id, context),
            matrix: leg,
            runner: labels,
            result: "success",
            targets,
            checkouts: selectedSteps
              .filter((step) => step.uses?.startsWith("actions/checkout@"))
              .map((step) => render(step.with?.ref ?? github.ref, context)),
            downloads: selectedSteps
              .filter((step) => step.uses?.startsWith("actions/download-artifact@"))
              .map((step) =>
                Object.fromEntries(
                  Object.entries(step.with ?? {}).map(([key, value]) => [
                    key,
                    render(value, context),
                  ]),
                ),
              ),
          });
        }
      }
      let outputs = {};
      if (id === "changes" && file === "ci.yml")
        outputs = changesOutputs(job, classification, event.classificationContext);
      else if (id === "guard") outputs = guardOutputs(job, variables);
      else if (id === "arches" && file === screencompCall)
        outputs = { matrix: screencompOutput(job, context) };
      else if (job.outputs) throw new Error(`unresolved job outputs: ${file}/${id}`);
      results[id] = { result: "success", outputs };
    }
    if (!progress) throw new Error(`unresolved job needs: ${file}: ${[...pending]}`);
  }
  return rows.map((row) => ({ ...row, runWorkflow: stack[0] ?? file }));
}
export function sourceDigest(directory = root) {
  const hash = createHash("sha256");
  const paths = execFileSync(
    "git",
    ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
    { cwd: directory, encoding: "utf8" },
  )
    .split("\0")
    .filter(Boolean);
  for (const path of [...new Set(paths)].sort()) {
    // Thresholds are judgments, not measurement inputs; the reporter never reads them.
    if (path.endsWith("budgets.yaml")) continue;
    hash.update(path);
    try {
      hash.update(readFileSync(resolve(directory, path)));
    } catch (error) {
      if (error.code === "ENOENT") hash.update("<deleted>");
      else throw error;
    }
  }
  return hash.digest("hex");
}
export const isMac = (row) => row.runner?.some((label) => label.startsWith("macos-")) ?? false;
