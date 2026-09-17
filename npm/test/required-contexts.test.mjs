// The required per-platform contexts, held to what ci.yml reports on every change.
//
// main's branch protection requires `cross (macos-latest)`, `cross (windows-latest)`,
// `install (ubuntu-latest)`, `install (macos-latest)` and `install (windows-latest)`
// by name. A matrix job skipped at job level reports only its bare name — a
// skipped `cross`, never a `cross (macos-latest)` — so a job-level condition on
// the `changes` output leaves every one of those contexts pending for ever on a
// change that reaches no crate, and the pull request can never merge. So the
// contract is: both matrix jobs are scheduled on every change, and on a crate-free
// one each leg succeeds through a single step that says so, with nothing else run.
//
// Held here by reading the workflow the way GitHub does — the YAML's jobs, their
// `if`, their steps' `if` — and evaluating each condition for both values the
// `changes` job can report, rather than by matching text: a job-level `if` spelled
// any other way, or a real step whose `if` drifts, is the same defect.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

/// The contexts branch protection requires, by job and matrix leg.
const REQUIRED = {
  cross: ["macos-latest", "windows-latest"],
  install: ["ubuntu-latest", "macos-latest", "windows-latest"],
};

/// The subset of GitHub's expression grammar a step or job condition here uses:
/// `!`, `&&`, `||`, `==`, `!=`, parentheses, string literals, `true`/`false`,
/// dotted context lookups and the string functions. Any other token is refused,
/// so a condition this evaluator cannot read fails the test rather than reading
/// as whichever outcome the case wanted.
function tokenize(expression) {
  const tokens = [];
  const pattern = /\s*(?:(\|\||&&|==|!=|[!()])|'((?:[^']|'')*)'|([A-Za-z_][\w.-]*)|(,))/y;
  let at = 0;
  while (at < expression.length) {
    pattern.lastIndex = at;
    const m = pattern.exec(expression);
    assert.ok(m && m[0].length > 0, `unreadable condition at ${at}: ${expression}`);
    at = pattern.lastIndex;
    if (m[1] !== undefined) tokens.push({ type: "op", value: m[1] });
    else if (m[2] !== undefined) tokens.push({ type: "string", value: m[2].replace(/''/g, "'") });
    else if (m[3] !== undefined) tokens.push({ type: "name", value: m[3] });
    else tokens.push({ type: "op", value: "," });
  }
  return tokens;
}

function lookup(context, path) {
  let value = context;
  for (const part of path.split(".")) {
    if (value === null || typeof value !== "object") return null;
    value = value[part] ?? null;
  }
  return value;
}

/// GitHub compares strings case-insensitively. It also coerces across types,
/// which no condition here relies on: the values compared are the strings a job
/// output and a literal are.
function equal(a, b) {
  if (typeof a === "string" && typeof b === "string") return a.toLowerCase() === b.toLowerCase();
  return a === b;
}

const FUNCTIONS = {
  startsWith: (s, prefix) =>
    String(s ?? "")
      .toLowerCase()
      .startsWith(String(prefix).toLowerCase()),
  endsWith: (s, suffix) =>
    String(s ?? "")
      .toLowerCase()
      .endsWith(String(suffix).toLowerCase()),
  contains: (s, needle) =>
    String(s ?? "")
      .toLowerCase()
      .includes(String(needle).toLowerCase()),
};

function evaluate(expression, context) {
  const tokens = tokenize(expression);
  let at = 0;
  const peek = () => tokens[at];
  const take = (type, value) => {
    const token = tokens[at];
    assert.ok(
      token && token.type === type && (value === undefined || token.value === value),
      `expected ${value ?? type} at token ${at} of: ${expression}`,
    );
    at += 1;
    return token;
  };
  const isOp = (value) => peek()?.type === "op" && peek().value === value;

  function primary() {
    if (isOp("!")) {
      take("op", "!");
      return !primary();
    }
    if (isOp("(")) {
      take("op", "(");
      const value = or();
      take("op", ")");
      return value;
    }
    const token = take(peek()?.type === "string" ? "string" : "name");
    if (token.type === "string") return token.value;
    if (token.value === "true") return true;
    if (token.value === "false") return false;
    if (isOp("(")) {
      const fn = FUNCTIONS[token.value];
      assert.ok(fn, `unknown function ${token.value} in: ${expression}`);
      take("op", "(");
      const args = [or()];
      while (isOp(",")) {
        take("op", ",");
        args.push(or());
      }
      take("op", ")");
      return fn(...args);
    }
    return lookup(context, token.value);
  }
  function equality() {
    let left = primary();
    while (isOp("==") || isOp("!=")) {
      const op = take("op").value;
      const right = primary();
      left = op === "==" ? equal(left, right) : !equal(left, right);
    }
    return left;
  }
  function and() {
    let left = equality();
    while (isOp("&&")) {
      take("op", "&&");
      const right = equality();
      left = left && right;
    }
    return left;
  }
  function or() {
    let left = and();
    while (isOp("||")) {
      take("op", "||");
      const right = and();
      left = left || right;
    }
    return left;
  }
  const value = or();
  assert.equal(at, tokens.length, `trailing tokens in: ${expression}`);
  return Boolean(value);
}

/// A step or job with no `if` runs; GitHub reads a bare `if:` the same way.
function runs(condition, context) {
  if (condition === undefined || condition === null || condition === "") return true;
  return evaluate(String(condition), context);
}

/// The context a pull request's job sees, with the `changes` job's crate output
/// set to one of the two values it reports.
function pullRequestContext(crate, os) {
  return {
    github: { event_name: "pull_request", head_ref: "feature", ref: "refs/pull/1/merge" },
    needs: { changes: { outputs: { crate, sdk: "false" }, result: "success" } },
    matrix: { os },
    runner: { os },
  };
}

describe("the required per-platform contexts", () => {
  const workflow = parse(readFileSync(join(REPO_ROOT, ".github", "workflows", "ci.yml"), "utf8"));

  it("knows the crate output the conditions read", () => {
    assert.match(
      String(workflow.jobs.changes.outputs.crate ?? ""),
      /steps\.affected\.outputs\.crate/,
      "the `changes` job no longer reports a `crate` output",
    );
  });

  for (const [job, legs] of Object.entries(REQUIRED)) {
    describe(`${job}`, () => {
      const definition = workflow.jobs[job];

      it("is a job in ci.yml with one leg per required context", () => {
        assert.ok(definition, `ci.yml has no \`${job}\` job`);
        assert.deepEqual(
          definition.strategy?.matrix?.os,
          legs,
          `\`${job}\`'s matrix does not name exactly the platforms branch protection requires`,
        );
        assert.ok(
          [].concat(definition.needs).includes("changes"),
          `\`${job}\` does not need \`changes\`, so its steps cannot read the crate output`,
        );
      });

      it("carries no job-level condition, so every leg is scheduled on every change", () => {
        assert.equal(
          definition.if,
          undefined,
          `\`${job}\` has a job-level \`if\` (${JSON.stringify(definition.if)}); skipped there, its legs report no context and the required checks wait for ever`,
        );
      });

      for (const os of legs) {
        it(`${job} (${os}) succeeds through one step, and only that step, when the diff reaches no crate`, () => {
          const context = pullRequestContext("false", os);
          const scheduled = definition.steps.filter((step) => runs(step.if, context));
          assert.equal(
            scheduled.length,
            1,
            `${job} (${os}): ${scheduled.length} steps run on a crate-free change; expected exactly the one that says there is nothing to do`,
          );
          const [only] = scheduled;
          assert.ok(only.run, `${job} (${os}): the crate-free step is not a \`run:\` step`);
          assert.equal(only.uses, undefined, `${job} (${os}): the crate-free step uses an action`);
          const lines = String(only.run).trim().split("\n");
          assert.equal(
            lines.length,
            1,
            `${job} (${os}): the crate-free step is more than one line`,
          );
          assert.match(
            lines[0],
            /^echo /,
            `${job} (${os}): the crate-free step does more than say so`,
          );
        });

        it(`${job} (${os}) runs every real step, and not the no-op, when the diff reaches a crate`, () => {
          const context = pullRequestContext("true", os);
          const scheduled = definition.steps.filter((step) => runs(step.if, context));
          const skipped = definition.steps.filter((step) => !runs(step.if, context));
          assert.equal(
            skipped.length,
            1,
            `${job} (${os}): ${skipped.length} steps are skipped on a crate change; only the no-op should be`,
          );
          assert.match(String(skipped[0].run ?? ""), /^echo /);
          assert.equal(scheduled.length, definition.steps.length - 1);
          assert.ok(
            scheduled.some((step) => step.uses === "actions/checkout@v4"),
            `${job} (${os}): no checkout runs on a crate change`,
          );
          assert.ok(
            scheduled.some((step) => step.run && !/^echo /.test(String(step.run).trim())),
            `${job} (${os}): no real work runs on a crate change`,
          );
        });
      }
    });
  }
});
