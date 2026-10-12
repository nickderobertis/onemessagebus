// Repository-private resolver. Unknown calls or dynamic outputs are errors.
import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
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
    if (!Array.isArray(values)) throw new Error(`unresolved matrix dimension ${key}`);
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
  if (!Object.keys(definition).some((k) => !["include", "exclude"].includes(k)))
    return include.length ? include : [{}];
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
  if (options?.branches && !options.branches.includes(event.branch)) return false;
  if (options?.tags && !options.tags.includes(event.tag)) return false;
  if (options?.workflows && !options.workflows.includes(event.workflow)) return false;
  if (options?.paths || options?.["paths-ignore"]) throw new Error("unresolved event path filter");
  return true;
}
let captureMatrix;
function screencompOutput() {
  if (captureMatrix) return captureMatrix;
  // The external CLI boundary is its committed capture config, never a count fixture.
  const config = JSON.parse(
    execFileSync(
      "python3",
      ["-c", 'import json,tomllib; print(json.dumps(tomllib.load(open("screencomp.toml","rb"))))'],
      { cwd: root, encoding: "utf8" },
    ),
  );
  if (!Array.isArray(config.capture?.arches) || !config.capture.arches.length)
    throw new Error("unresolved screencomp arches");
  captureMatrix = JSON.stringify({
    include: config.capture.arches.map((arch) => ({
      arch,
      project: "",
      current: "shots/current",
      verify: "shots/verify",
      manifest: "captures.json",
      gallery_title: "Visual docs",
    })),
  });
  return captureMatrix;
}
const screencompCall =
  "nickderobertis/screencomp/.github/workflows/visual-docs-reusable.yml@v0.4.8";
function called(reference) {
  if (reference === screencompCall) {
    const pinned = JSON.parse(read(".github/reusable-workflows.json"))[reference];
    if (!pinned) throw new Error(`unresolved pinned reusable call: ${reference}`);
    const source = read(pinned.path);
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
        outputs = {
          crate: "true",
          sdk: "true",
          macos: String(classification.macosRequired),
          "release-only": String(classification.isReleaseOnly),
        };
      else if (id === "guard")
        outputs = { publish: variables.CARGO_PUBLISH ?? "true", enabled: "true" };
      else if (id === "arches" && file === screencompCall) outputs = { matrix: screencompOutput() };
      else if (job.outputs) throw new Error(`unresolved job outputs: ${file}/${id}`);
      results[id] = { result: "success", outputs };
    }
    if (!progress) throw new Error(`unresolved job needs: ${file}: ${[...pending]}`);
  }
  return rows.map((row) => ({ ...row, runWorkflow: stack[0] ?? file }));
}
export function sourceDigest() {
  const hash = createHash("sha256");
  const paths = execFileSync(
    "git",
    ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
    { cwd: root, encoding: "utf8" },
  )
    .split("\0")
    .filter(Boolean);
  for (const path of [...new Set(paths)].sort()) {
    // Thresholds are judgments, not measurement inputs; the reporter never reads them.
    if (path.endsWith("budgets.yaml")) continue;
    hash.update(path);
    try {
      hash.update(readFileSync(resolve(root, path)));
    } catch (error) {
      if (error.code === "ENOENT") hash.update("<deleted>");
      else throw error;
    }
  }
  return hash.digest("hex");
}
export const isMac = (row) => row.runner?.some((label) => label.startsWith("macos-")) ?? false;
