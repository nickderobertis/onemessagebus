// The contexts that must be green to merge, held to the two things that decide
// them: the jobs ci.yml declares, and main's branch protection on GitHub.
//
// Nothing here transcribes a list. The old shape of this file did — it named the
// five per-platform contexts it believed were required and held AGENTS.md to the
// same names — and the two in-tree restatements agreed with each other while both
// disagreed with GitHub, which required neither `sdk-install` leg. A test that can
// say a journey gates merges while protection has never heard of it is worse than
// no test: it is how a broken install journey merges (issue #123).
//
// So the list has one derivation, `scripts/required-contexts.mjs`, and this file
// drives that script:
//
//   * over the real ci.yml, for the contexts it says the workflow emits, and for
//     the one workflow shape that would make a required context unreportable — a
//     matrix job with a job-level `if`, which GitHub skips to one bare
//     `sdk-install` rather than to its two legs;
//   * over recorded branch-protection answers, for the comparison: an exact match,
//     a context the workflow emits that protection does not require, a context
//     protection requires that no job emits, and a read that was refused.
//
// A refused read is the case that matters most, for the same reason "not
// answered" matters most to the release probe: read as "no contexts required" it
// would turn the one gate that can catch this drift into a gate that passes
// whenever it cannot see. It must refuse, and name the secret whose reach it
// needed.
//
// The second half of the file is the scheduling those per-leg contexts depend on,
// and it is driven by the same derivation rather than by a list of its own. A
// matrix job skipped at job level reports only its bare name, so every matrix job
// here is scheduled on every change and its skip lives on its steps: on a change
// that reaches nothing the job needs, each leg succeeds through a single step that
// says so — and on ubuntu-latest, because the context name comes from the matrix
// rather than the runner and a leg that prints one line has no business waiting
// for scarce macOS capacity. On a change that does reach it, each leg runs on its
// own platform with every real step.
//
// That half is a structural assertion rather than an end-to-end one, on purpose.
// The behavior is GitHub's scheduling of these jobs on a pull request, and its
// only end-to-end proof is such a pull request: runners on a hosted service,
// driven by a push, read back through an authenticated API — which no offline run
// can stand up, and which every pull request already performs. What this
// repository authors is exactly the `if` and `runs-on` fields GitHub reads to
// decide that scheduling, so evaluating them the way GitHub does is the whole of
// the check that can run here.

import { readFileSync } from "node:fs";
import { rmSync } from "node:fs";
import { describe, it } from "node:test";
import assert from "node:assert/strict";

import { parse } from "yaml";

import {
  CI_WORKFLOW,
  REFUSED_INPUT,
  SHIMMABLE,
  WELL_FORMED_NO,
  assertRefused,
  deriveFrom,
  protectionRequiring,
  recordedProtection,
  requiredContexts,
  workflowWith,
} from "./support/required-contexts.mjs";

/// The repository the recorded answers were read from, passed the way the
/// workflow passes `$GITHUB_REPOSITORY`.
const REPO = "nickderobertis/onemessagebus";

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

/// The context a pull request's job sees, with the `changes` job's outputs set
/// to what it reports for a diff that reaches everything, or nothing, the matrix
/// jobs select on. Those are the two answers that decide whether a leg does real
/// work; a leg reports its context either way, which is the point.
function pullRequestContext(reaches, matrix) {
  const affected = reaches ? "true" : "false";
  return {
    github: { event_name: "pull_request", head_ref: "feature", ref: "refs/pull/1/merge" },
    needs: { changes: { outputs: { crate: affected, sdk: affected }, result: "success" } },
    matrix,
    runner: { os: matrix.os },
  };
}

/// The matrix jobs of a workflow, by the contexts the script says they emit: one
/// entry per leg, carrying the `matrix` context GitHub would give it. The leg
/// values are read back out of the context name in the workflow's own dimension
/// order, which is the order GitHub renders them in.
function matrixLegsByJob(workflow, contexts) {
  const jobs = new Map();
  for (const context of contexts) {
    const named = /^(?<job>.+?) \((?<values>.+)\)$/.exec(context);
    if (!named) continue;
    const { job, values } = named.groups;
    const definition = workflow.jobs[job];
    assert.ok(
      definition,
      `the derivation named a context for \`${job}\`, which ci.yml has no job for`,
    );
    const keys = Object.keys(definition.strategy.matrix);
    const leg = values.split(", ");
    assert.equal(
      leg.length,
      keys.length,
      `\`${context}\` names ${leg.length} of ${job}'s ${keys.length} matrix dimensions`,
    );
    const entry = jobs.get(job) ?? [];
    entry.push({ context, matrix: Object.fromEntries(keys.map((key, at) => [key, leg[at]])) });
    jobs.set(job, entry);
  }
  return jobs;
}

const workflow = parse(readFileSync(CI_WORKFLOW, "utf8"));
/// Every context ci.yml emits, derived by the script the push-to-main gate runs.
const DERIVATION = deriveFrom();
const CONTEXTS = DERIVATION.status === 0 ? DERIVATION.stdout.trim().split("\n") : [];
const LEGS = matrixLegsByJob(workflow, CONTEXTS);

describe("the contexts ci.yml emits", () => {
  it("can be derived from ci.yml at all, which no matrix job's job-level `if` would allow", () => {
    // The derivation refuses a matrix job whose skip is at job level, so that it
    // answers here is the standing proof ci.yml has none: GitHub skips such a job
    // to one bare name, and the per-leg contexts protection requires by name would
    // never appear on a change that skipped it.
    assert.equal(
      DERIVATION.status,
      0,
      `the derivation refused the real ci.yml: ${DERIVATION.stderr}`,
    );
  });

  it("names one context per job, and one per leg of a matrix job", () => {
    const expected = Object.entries(workflow.jobs).flatMap(([job, definition]) => {
      const matrix = definition.strategy?.matrix;
      if (!matrix) return [job];
      return Object.values(matrix)[0].map((leg) => `${job} (${leg})`);
    });
    assert.deepEqual(CONTEXTS, expected);
  });

  it("names both sdk-install legs, which branch protection has never required", () => {
    // The defect issue #123 found, stated as the two facts that made it: the
    // workflow emits these, and the answer GitHub gave does not require them.
    assert.deepEqual(
      LEGS.get("sdk-install")?.map((leg) => leg.context),
      ["sdk-install (ubuntu-latest)", "sdk-install (macos-latest)"],
    );
    const recorded = new Set(recordedProtection().required_status_checks.contexts);
    assert.ok(!recorded.has("sdk-install (ubuntu-latest)"));
    assert.ok(!recorded.has("sdk-install (macos-latest)"));
  });

  it("refuses a matrix job whose skip is at job level, naming it", () => {
    // What ci.yml said until this change: skipped there, GitHub reports one bare
    // `sdk-install` and neither required leg, and the pull request waits for ever.
    const { path, directory } = workflowWith((text) =>
      text.replace(
        "  sdk-install:\n    needs: [changes, gate]\n",
        "  sdk-install:\n    needs: [changes, gate]\n    if: needs.changes.outputs.sdk == 'true'\n",
      ),
    );
    try {
      const result = requiredContexts(["--list", "--workflow", path]);
      assertRefused(result, REFUSED_INPUT, "a matrix job with a job-level `if`");
      assert.match(result.stderr, /sdk-install/);
      assert.match(result.stderr, /job-level `if`/);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("refuses a workflow two jobs would report one context for", () => {
    // Collapsed into one name, the set protection requires covers one fewer
    // check than the workflow runs, and nothing downstream could tell.
    const { path, directory } = workflowWith((text) =>
      text.replace(
        "  msrv:\n    needs: changes\n",
        "  msrv:\n    name: gate\n    needs: changes\n",
      ),
    );
    try {
      const result = requiredContexts(["--list", "--workflow", path]);
      assertRefused(result, REFUSED_INPUT, "two jobs reporting one context");
      assert.match(result.stderr, /the same context more than once: `gate`/);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("refuses a workflow it cannot read, rather than dumping a stack trace", () => {
    const { path, directory } = workflowWith((text) => `${text}\n  : : not: yaml\n`);
    try {
      assertRefused(
        requiredContexts(["--list", "--workflow", path]),
        REFUSED_INPUT,
        "a workflow that is not readable YAML",
      );
      assertRefused(
        requiredContexts(["--list", "--workflow", `${path}.absent`]),
        REFUSED_INPUT,
        "a workflow that is not there",
      );
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("knows the outputs the matrix jobs' conditions read", () => {
    for (const output of ["crate", "sdk"]) {
      assert.match(
        String(workflow.jobs.changes.outputs[output] ?? ""),
        new RegExp(`steps\\.affected\\.outputs\\.${output}`),
        `the \`changes\` job no longer reports a \`${output}\` output`,
      );
    }
  });
});

describe("branch protection against the contexts ci.yml emits", () => {
  const skipUnshimmable = (t) => {
    if (!SHIMMABLE) t.skip("the `gh` that answers the protection read is a POSIX script");
    return !SHIMMABLE;
  };

  it("passes when protection requires exactly those contexts", (t) => {
    if (skipUnshimmable(t)) return;
    const result = requiredContexts(["--repo", REPO, "--branch", "main"], {
      answer: protectionRequiring(CONTEXTS),
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /requires exactly the \d+ contexts/);
    assert.equal(result.stdout.trim().split("\n").length, 1, "said more than a line on success");
  });

  it("fails on a context the workflow emits and protection does not require", (t) => {
    if (skipUnshimmable(t)) return;
    // The recorded answer, unedited: this repository's own drift, reported as it.
    const result = requiredContexts(["--repo", REPO, "--branch", "main"], {
      answer: recordedProtection(),
    });
    assertRefused(result, WELL_FORMED_NO, "protection missing two emitted contexts");
    assert.match(result.stderr, /missing: `sdk-install \(ubuntu-latest\)`/);
    assert.match(result.stderr, /missing: `sdk-install \(macos-latest\)`/);
    // And the next action is the list to set, so nobody has to assemble one.
    for (const context of CONTEXTS) assert.ok(result.stderr.includes(context), context);
  });

  it("fails on a context protection requires that no job emits", (t) => {
    if (skipUnshimmable(t)) return;
    const result = requiredContexts(["--repo", REPO, "--branch", "main"], {
      answer: protectionRequiring([...CONTEXTS, "cross (freebsd-latest)"]),
    });
    assertRefused(result, WELL_FORMED_NO, "protection requiring a context no job emits");
    assert.match(result.stderr, /extra: +`cross \(freebsd-latest\)`/);
  });

  it("refuses a read it could not make, naming the secret and the permission", (t) => {
    if (skipUnshimmable(t)) return;
    const result = requiredContexts(["--repo", REPO, "--branch", "main"], {
      refusal: "gh: Resource not accessible by integration (HTTP 403)",
    });
    assertRefused(result, REFUSED_INPUT, "a refused protection read");
    assert.match(result.stderr, /RELEASE_PLZ_TOKEN/);
    assert.match(result.stderr, /administration \(read\)/);
    assert.match(result.stderr, /Resource not accessible by integration/);
    // Never as "nothing is required": that reading is the gate passing blind.
    assert.doesNotMatch(result.stderr, /requires exactly/);
  });

  it("refuses a repository or branch it will not put in an API path", () => {
    // Both reach the endpoint the read is built from, so neither is interpolated
    // on trust: a segment that is not an identifier asks a different question.
    for (const argv of [
      ["--repo", "nickderobertis"],
      ["--repo", "nickderobertis/one messagebus"],
      ["--repo", "../../etc"],
      ["--repo", REPO, "--branch", "../main"],
      ["--repo", REPO, "--branch", ""],
    ]) {
      assertRefused(requiredContexts(argv), REFUSED_INPUT, `the arguments ${argv.join(" ")}`);
    }
  });

  it("refuses a protection answer whose required check has no name", (t) => {
    if (skipUnshimmable(t)) return;
    const unnamed = recordedProtection();
    unnamed.required_status_checks.checks = [{ app_id: null }];
    const result = requiredContexts(["--repo", REPO, "--branch", "main"], { answer: unnamed });
    assertRefused(result, REFUSED_INPUT, "a required check with no context name");
    assert.match(result.stderr, /`checks\[0\]`/);
  });

  it("refuses a branch that requires no status checks at all", (t) => {
    if (skipUnshimmable(t)) return;
    const unprotected = recordedProtection();
    unprotected.required_status_checks = undefined;
    const result = requiredContexts(["--repo", REPO, "--branch", "main"], { answer: unprotected });
    assertRefused(result, WELL_FORMED_NO, "a branch with no required status checks");
    assert.match(result.stderr, /requires no status checks/);
  });
});

describe("every leg reports its context, whatever the diff reaches", () => {
  for (const [job, legs] of LEGS) {
    describe(`${job}`, () => {
      const definition = workflow.jobs[job];

      for (const { context, matrix } of legs) {
        it(`${context} is scheduled on ubuntu-latest when the diff reaches nothing it needs`, () => {
          assert.equal(
            runner(definition["runs-on"], pullRequestContext(false, matrix)),
            "ubuntu-latest",
            `${context}: a leg that only reports its context selects a runner other than ubuntu-latest`,
          );
        });

        it(`${context} is scheduled on ${matrix.os} when the diff reaches it`, () => {
          assert.equal(
            runner(definition["runs-on"], pullRequestContext(true, matrix)),
            matrix.os,
            `${context}: a change this job selects on does not run this leg on its own platform`,
          );
        });

        it(`${context} succeeds through one step, and only that step, when the diff reaches nothing it needs`, () => {
          const context_ = pullRequestContext(false, matrix);
          const scheduled = definition.steps.filter((step) => runs(step.if, context_));
          assert.equal(
            scheduled.length,
            1,
            `${context}: ${scheduled.length} steps run; expected exactly the one that says there is nothing to do`,
          );
          const [only] = scheduled;
          assert.ok(only.run, `${context}: the no-op step is not a \`run:\` step`);
          assert.equal(only.uses, undefined, `${context}: the no-op step uses an action`);
          const lines = String(only.run).trim().split("\n");
          assert.equal(lines.length, 1, `${context}: the no-op step is more than one line`);
          assert.match(lines[0], /^echo /, `${context}: the no-op step does more than say so`);
          assert.match(
            lines[0],
            /\$\{\{\s*matrix\.os\s*\}\}/,
            `${context}: the no-op step's line does not name the matrix leg it reports for`,
          );
        });

        it(`${context} runs every real step, and not the no-op, when the diff reaches it`, () => {
          const context_ = pullRequestContext(true, matrix);
          const scheduled = definition.steps.filter((step) => runs(step.if, context_));
          const skipped = definition.steps.filter((step) => !runs(step.if, context_));
          assert.equal(
            skipped.length,
            1,
            `${context}: ${skipped.length} steps are skipped; only the no-op should be`,
          );
          assert.match(String(skipped[0].run ?? ""), /^echo /);
          assert.equal(scheduled.length, definition.steps.length - 1);
          assert.ok(
            scheduled.some((step) => step.uses === "actions/checkout@v4"),
            `${context}: no checkout runs on a change this job selects on`,
          );
          assert.ok(
            scheduled.some((step) => step.run && !/^echo /.test(String(step.run).trim())),
            `${context}: no real work runs on a change this job selects on`,
          );
        });
      }
    });
  }
});
