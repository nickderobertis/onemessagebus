// The required per-platform contexts, held to what ci.yml reports on every change.
//
// main's branch protection requires `cross (macos-latest)`, `cross (windows-latest)`,
// `install (ubuntu-latest)`, `install (macos-latest)` and `install (windows-latest)`
// by name. A matrix job skipped at job level reports only its bare name — a
// skipped `cross`, never a `cross (macos-latest)` — so a job-level condition on
// the `changes` output leaves every one of those contexts pending for ever on a
// change that reaches no crate, and the pull request can never merge. So the
// contract is: both matrix jobs are scheduled on every change, and on a crate-free
// one each leg succeeds through a single step that says so, with nothing else run —
// and runs on `ubuntu-latest` to do it, because the context name comes from the
// matrix rather than the runner, and a leg that prints one line has no business
// waiting an hour for scarce macOS capacity. On a crate change each leg runs on its
// own `matrix.os`.
//
// Held here by reading the workflow the way GitHub does — the YAML's jobs, their
// `if`, their `runs-on`, their steps' `if` — and evaluating each expression for
// both values the `changes` job can report, rather than by matching text: a
// job-level `if` spelled any other way, a runner expression that sends a no-op
// leg back to macOS or a real one to ubuntu, or a real step whose `if` drifts, is
// the same defect.
//
// A structural assertion rather than an end-to-end one, on purpose. The behavior is
// GitHub's scheduling of these jobs on a pull request against this repository's
// branch protection, and its only end-to-end proof is such a pull request: two
// runners per matrix leg on a hosted service, driven by a push, read back through
// an authenticated API — which no offline run can stand up, and which every pull
// request already performs. What this repository authors is exactly the `if` and
// `runs-on` fields GitHub reads to decide that scheduling, so evaluating them the
// way GitHub does is the whole of the check that can run here; the steps they condition are
// the ones that ran before the condition existed, unchanged, and are proven by
// running on every crate change.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

/// The contexts branch protection requires, by job and matrix leg.
///
/// The one source is main's branch-protection setting on GitHub, which nothing in
/// this tree declares and only an authenticated API call can read; the `check`
/// tier is offline and credential-free by rule, so this list cannot be reconciled
/// with it here. It is held instead to the tree's own statement of the required
/// checks in AGENTS.md, below, so the two in-tree restatements cannot drift apart,
/// and the credentialed reconciliation with GitHub is a follow-up.
// llmlint: ignore[contracts_have_one_source_or_a_drift_gate] the authoritative source is GitHub's branch-protection configuration, outside the tree and readable only with a credential the offline deterministic tier must not require; the gate that can run here reconciles this list with AGENTS.md's declaration of the required checks (the `AGENTS.md names every context this test holds` case), and the credentialed drift gate against GitHub itself is recorded as a follow-up.
const REQUIRED = {
  cross: ["macos-latest", "windows-latest"],
  install: ["ubuntu-latest", "macos-latest", "windows-latest"],
};

/// The subset of GitHub's expression grammar a condition or runner selection here
/// uses: `!`, `&&`, `||`, `==`, `!=`, parentheses, string literals, `true`/`false`,
/// dotted context lookups and the string functions. Any other token is refused,
/// so an expression this evaluator cannot read fails the test rather than reading
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
  return value;
}

/// The expression inside a `${{ }}` wrapper, or the bare text where the field
/// carries none: GitHub reads a condition either way, and a `runs-on` that is not
/// an expression is a literal runner label.
function expression(field) {
  const text = String(field).trim();
  const wrapped = /^\$\{\{(.*)\}\}$/s.exec(text);
  return wrapped ? wrapped[1].trim() : text;
}

/// A step or job with no `if` runs; GitHub reads a bare `if:` the same way. The
/// value an `&&` or `||` yields is one of its operands, so the result is coerced
/// the way GitHub coerces a condition.
function runs(condition, context) {
  if (condition === undefined || condition === null || condition === "") return true;
  return Boolean(evaluate(expression(condition), context));
}

/// The runner a job's `runs-on` selects in a context: the literal label where it
/// is one, else the value its expression yields. A label GitHub would not read as
/// a single hosted runner — a list, a group, an empty or non-string value — is
/// refused rather than read as any platform.
function runner(runsOn, context) {
  assert.equal(
    typeof runsOn,
    "string",
    `\`runs-on\` is not a single label or expression: ${JSON.stringify(runsOn)}`,
  );
  const text = expression(runsOn);
  const value = text === runsOn.trim() ? text : evaluate(text, context);
  assert.ok(
    typeof value === "string" && value !== "",
    `\`runs-on\` resolves to no runner label: ${JSON.stringify(value)} from ${runsOn}`,
  );
  return value;
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

  it("AGENTS.md names every context this test holds", () => {
    const agents = readFileSync(join(REPO_ROOT, "AGENTS.md"), "utf8");
    const named = new Set(
      [...agents.matchAll(/`([a-z-]+) \(([a-z-]+)\)`/g)].map((m) => `${m[1]} (${m[2]})`),
    );
    for (const [job, legs] of Object.entries(REQUIRED)) {
      for (const os of legs) {
        assert.ok(
          named.has(`${job} (${os})`),
          `AGENTS.md's list of required checks no longer names \`${job} (${os})\``,
        );
      }
    }
  });

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
        it(`${job} (${os}) is scheduled on ubuntu-latest when the diff reaches no crate`, () => {
          assert.equal(
            runner(definition["runs-on"], pullRequestContext("false", os)),
            "ubuntu-latest",
            `${job} (${os}): a crate-free change selects a runner other than ubuntu-latest for a leg that only reports its context`,
          );
        });

        it(`${job} (${os}) is scheduled on ${os} when the diff reaches a crate`, () => {
          assert.equal(
            runner(definition["runs-on"], pullRequestContext("true", os)),
            os,
            `${job} (${os}): a crate change does not run this leg on its own platform`,
          );
        });

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
