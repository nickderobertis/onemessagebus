import assert from "node:assert/strict";
import { test } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  root,
  workflows,
  resolveRun,
  isMac,
  sourceDigest,
  matrix,
  triggered,
  screencompOutput,
  changesOutputs,
} from "../workflow-graph.mjs";
import { mkdirSync } from "node:fs";
import { parse, stringify } from "yaml";
const repository = "nickderobertis/onemessagebus";
const scratch = mkdtempSync(
  join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "macos-policy-"),
);
const git = (...args) =>
  execFileSync("git", args, {
    cwd: scratch,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
git("init", "-q");
git("config", "user.name", "Policy journey");
git("config", "user.email", "policy@example.invalid");
writeFileSync(join(scratch, "Cargo.toml"), '[workspace.package]\nversion="0.9.0"\nlicense="MIT"\n');
writeFileSync(join(scratch, "CHANGELOG.md"), "# Changelog\n");
git("add", ".");
git("commit", "-qm", "base");
const base = git("rev-parse", "HEAD");
const histories = {};
for (const diff of ["source", "version-only", "dependency"]) {
  git("checkout", "-q", "--detach", base);
  writeFileSync(
    join(scratch, "Cargo.toml"),
    `[workspace.package]\nversion="0.10.0"\nlicense="MIT"\n${diff === "dependency" ? '[workspace.dependencies]\nserde="2"\n' : ""}`,
  );
  if (diff === "source") writeFileSync(join(scratch, "lib.rs"), "pub fn new_behavior() {}\n");
  writeFileSync(join(scratch, "CHANGELOG.md"), "# Changelog\n\n## 0.10.0\n");
  git("add", ".");
  git("commit", "-qm", diff);
  histories[diff] = git("rev-parse", "HEAD");
}
const releases = {};
for (const [index, diff] of Object.keys(histories).entries()) {
  git("checkout", "-q", "--detach", histories[diff]);
  const next = `0.${11 + index}.0`;
  writeFileSync(
    join(scratch, "Cargo.toml"),
    readFileSync(join(scratch, "Cargo.toml"), "utf8").replace("0.10.0", next),
  );
  writeFileSync(join(scratch, "CHANGELOG.md"), `# Changelog\n\n## ${next}\n`);
  git("add", ".");
  git("commit", "-qm", "chore: release");
  const head = git("rev-parse", "HEAD"),
    tag = `v${next}`;
  git("tag", tag, head);
  releases[diff] = { base: histories[diff], head, tag };
}
const payload = (fork = false, branch = "release-plz-2026-10", login = "nickderobertis") => ({
  pull_request: {
    user: { login },
    title: "chore: release",
    head: { ref: branch, repo: { full_name: fork ? "fork/onemessagebus" : repository } },
    base: { repo: { full_name: repository } },
  },
});
const classifications = new Map();
function classify(
  event,
  head = histories["version-only"],
  name = "pull_request",
  comparisonBase = base,
) {
  const key = JSON.stringify([event, head, name, comparisonBase]);
  if (classifications.has(key)) return classifications.get(key);
  const path = join(scratch, ".git/event.json");
  writeFileSync(path, typeof event === "string" ? event : JSON.stringify(event));
  const result = spawnSync(
    process.execPath,
    [
      join(root, ".github/macos-policy.mjs"),
      "classify",
      "--event",
      path,
      "--base",
      comparisonBase,
      "--head",
      head,
    ],
    { cwd: scratch, encoding: "utf8", env: { ...process.env, GITHUB_EVENT_NAME: name } },
  );
  assert.equal(result.status, 0, result.stderr);
  const answer = JSON.parse(result.stdout);
  classifications.set(key, answer);
  return answer;
}
test("shared production classifier proves identities and semantic diffs through git histories", () => {
  assert.equal(classify(payload()).isReleaseOnly, true);
  for (const event of [
    payload(false, "feature", "release-plz[bot]"),
    payload(true),
    payload(false, "feature"),
    {},
    "malformed",
  ])
    assert.equal(classify(event).macosRequired, true);
  for (const diff of ["source", "dependency"])
    assert.equal(classify(payload(), histories[diff]).macosRequired, true);
  assert.equal(classify(payload(), "missing-ref").macosRequired, true);
  assert.equal(classify({}, base, "push").macosRequired, false);
  // A trusted-looking branch/title still cannot bless a source edit.
  assert.equal(
    classify(payload(false, "release-plz-forged", "attacker"), histories.source).isReleaseOnly,
    false,
  );
});
test("manifest and lock versions require unchanged dependency content", () => {
  for (const [path, before, after, allowed] of [
    [
      "package-lock.json",
      { packages: { "": { version: "0.9.0" }, "node_modules/third-party": { version: "1.0.0" } } },
      { packages: { "": { version: "0.10.0" }, "node_modules/third-party": { version: "1.0.0" } } },
      true,
    ],
    [
      "package-lock.json",
      { packages: { "": { version: "0.9.0" }, "node_modules/third-party": { version: "1.0.0" } } },
      { packages: { "": { version: "0.10.0" }, "node_modules/third-party": { version: "2.0.0" } } },
      false,
    ],
  ]) {
    git("checkout", "-q", "--detach", base);
    writeFileSync(join(scratch, path), JSON.stringify(before));
    git("add", ".");
    git("commit", "-qm", "lock baseline");
    const lockBase = git("rev-parse", "HEAD");
    writeFileSync(join(scratch, path), JSON.stringify(after));
    git("add", ".");
    git("commit", "-qm", "lock change");
    const eventPath = join(scratch, ".git/event.json");
    writeFileSync(eventPath, JSON.stringify(payload()));
    const result = spawnSync(
      process.execPath,
      [
        join(root, ".github/macos-policy.mjs"),
        "classify",
        "--event",
        eventPath,
        "--base",
        lockBase,
        "--head",
        "HEAD",
      ],
      {
        cwd: scratch,
        encoding: "utf8",
        env: { ...process.env, GITHUB_EVENT_NAME: "pull_request" },
      },
    );
    assert.equal(JSON.parse(result.stdout).isReleaseOnly, allowed, result.stderr);
  }
});
test("every admitted manifest, lock and generated version surface preserves non-version content", () => {
  const cases = [
    [
      "Cargo.lock",
      'version=4\n[[package]]\nname="onemessagebus"\nversion="0.9.0"\n[[package]]\nname="onemessagebus-cli"\nversion="0.9.0"\ndependencies=["onemessagebus 0.9.0"]\n[[package]]\nname="external"\nversion="1.0.0"\nsource="registry+https://example.invalid"\n',
      (text) => text.replaceAll("0.9.0", "0.10.0"),
      (text) => text.replace('version="1.0.0"', 'version="2.0.0"'),
    ],
    [
      "uv.lock",
      'version=1\n[[package]]\nname="onemessagebus-cli"\nversion="0.9.0"\nsource={editable="."}\n[[package]]\nname="external"\nversion="1.0.0"\nsource={registry="https://example.invalid"}\n',
      (text) => text.replaceAll("0.9.0", "0.10.0"),
      (text) => text.replace("example.invalid", "changed.invalid"),
    ],
    [
      "package-lock.json",
      JSON.stringify({
        version: "0.9.0",
        packages: {
          "": { version: "0.9.0" },
          "npm/onemessagebus-cli": { version: "0.9.0" },
          "npm/onemessagebus-sdk": { version: "0.9.0" },
          "node_modules/external": { version: "1.0.0" },
        },
      }),
      (text) => text.replaceAll("0.9.0", "0.10.0"),
      (text) => text.replace("1.0.0", "2.0.0"),
    ],
    [
      "python/onemessagebus-sdk/pyproject.toml",
      '[project]\nname="onemessagebus"\nversion="0.9.0"\ndependencies=["onemessagebus-cli==0.9.0", "pydantic>=2"]\n',
      (text) => text.replaceAll("0.9.0", "0.10.0"),
      (text) => text.replace("pydantic>=2", "pydantic>=3"),
    ],
    ...["cli", "sdk"].map((name) => [
      `npm/onemessagebus-${name}/package.json`,
      JSON.stringify({
        name: `onemessagebus-${name}`,
        version: "0.9.0",
        dependencies: { external: "1.0.0" },
      }),
      (text) => text.replaceAll("0.9.0", "0.10.0"),
      (text) => text.replace("1.0.0", "2.0.0"),
    ]),
    [
      "npm/onemessagebus-sdk/src/version.ts",
      'export const SDK_VERSION: string = "0.9.0";\nexport const CLI_VERSION: string = "0.9.0";\nexport const other = 1;\n',
      (text) => text.replaceAll("0.9.0", "0.10.0"),
      (text) => text.replace("other = 1", "other = 2"),
    ],
    [
      "python/onemessagebus-sdk/src/onemessagebus/_version.py",
      '__version__ = "0.9.0"\nCLI_VERSION = "0.9.0"\nother = 1\n',
      (text) => text.replaceAll("0.9.0", "0.10.0"),
      (text) => text.replace("other = 1", "other = 2"),
    ],
  ];
  for (const [path, before, bump, nonVersion] of cases) {
    git("checkout", "-q", "--detach", base);
    mkdirSync(join(scratch, path, ".."), { recursive: true });
    writeFileSync(join(scratch, path), before);
    git("add", ".");
    git("commit", "-qm", "surface baseline");
    const start = git("rev-parse", "HEAD");
    const after = bump(before);
    writeFileSync(join(scratch, path), after);
    git("add", ".");
    git("commit", "-qm", "plain versions");
    assert.equal(
      classify(payload(), git("rev-parse", "HEAD"), "pull_request", start).isReleaseOnly,
      true,
      path,
    );
    writeFileSync(join(scratch, path), nonVersion(after));
    git("add", ".");
    git("commit", "-qm", "mixed changes");
    assert.equal(
      classify(payload(), git("rev-parse", "HEAD"), "pull_request", start).macosRequired,
      true,
      path,
    );
    if (path.endsWith("version.ts") || path.endsWith("_version.py")) {
      writeFileSync(join(scratch, path), after.replaceAll("0.10.0", "invalid"));
      git("add", ".");
      git("commit", "-qm", "invalid declarations");
      assert.equal(
        classify(payload(), git("rev-parse", "HEAD"), "pull_request", start).macosRequired,
        true,
        path,
      );
    }
  }
});
test("invalid omission-policy configuration requires Mac coverage", () => {
  const directory = join(scratch, ".git/configuration-case");
  mkdirSync(directory, { recursive: true });
  const script = join(directory, "macos-policy.mjs");
  writeFileSync(script, readFileSync(join(root, ".github/macos-policy.mjs")));
  const event = join(directory, "event.json");
  writeFileSync(event, JSON.stringify(payload()));
  for (const config of [
    { repository, branchPrefix: "" },
    { repository: "", branchPrefix: "release-plz-" },
    null,
    "malformed",
  ]) {
    writeFileSync(
      join(directory, "macos-policy.json"),
      typeof config === "string" ? config : JSON.stringify(config),
    );
    const result = spawnSync(
      process.execPath,
      [script, "classify", "--event", event, "--base", base, "--head", histories["version-only"]],
      {
        cwd: scratch,
        encoding: "utf8",
        env: { ...process.env, GITHUB_EVENT_NAME: "pull_request" },
      },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(JSON.parse(result.stdout).macosRequired, true);
  }
});
const classes = [
  ["pr-opened", "pull_request", "opened"],
  ["pr-synchronize", "pull_request", "synchronize"],
  ["pr-reopened", "pull_request", "reopened"],
  ["base-push", "push"],
  ["release-merge", "push"],
  ["release-tag", "push"],
  ["release-published", "release", "published"],
  ["downstream", "workflow_run", "completed"],
  ["repository-dispatch", "repository_dispatch"],
  ["schedule", "schedule"],
  ["manual-default", "workflow_dispatch"],
  ["manual-opt-in", "workflow_dispatch"],
];
const graph = workflows();
const recorded = [];
test("actual workflow/call graph resolves every event, ownership, diff and publish combination", () => {
  for (const diff of Object.keys(histories))
    for (const fork of [false, true])
      for (const [label, name, action] of classes)
        for (const pypi of ["false", "true"])
          for (const npm of ["false", "true"])
            for (const crate of ["false", "true"]) {
              const pr = payload(fork, label.startsWith("pr-") ? "feature" : "release-plz-2026-10");
              const event = {
                label,
                name,
                action,
                branch: label === "release-tag" ? undefined : "main",
                tag: label === "release-tag" ? releases[diff].tag : undefined,
                sha: ["release-tag", "release-published", "release-merge", "downstream"].includes(
                  label,
                )
                  ? releases[diff].head
                  : histories[diff],
                ref: ["release-tag", "release-published"].includes(label)
                  ? `refs/tags/${releases[diff].tag}`
                  : "refs/heads/main",
                payload:
                  name === "pull_request"
                    ? pr
                    : name === "release"
                      ? {
                          action,
                          release: {
                            tag_name: releases[diff].tag,
                            target_commitish: releases[diff].head,
                          },
                        }
                      : { action },
                inputs: { macos: label === "manual-opt-in" },
              };
              const policy = classify(name === "pull_request" ? pr : {}, histories[diff], name);
              const rows = [];
              for (const [file, workflow] of Object.entries(graph)) {
                if (triggered(workflow, event))
                  rows.push(
                    ...resolveRun(file, workflow, event, policy, {
                      PYPI_PUBLISH: pypi,
                      NPM_PUBLISH: npm,
                      CARGO_PUBLISH: crate,
                    }),
                  );
              }
              for (const row of rows.filter(isMac)) {
                assert.ok(
                  (row.workflow === "ci.yml" && name === "pull_request" && policy.macosRequired) ||
                    (row.workflow === "release.yml" && name === "release") ||
                    (row.workflow === "macos-diagnostic.yml" && label === "manual-opt-in"),
                  JSON.stringify(row),
                );
              }
              if (name === "release") {
                assert.equal(git("rev-parse", event.ref), event.sha);
                for (const row of rows.filter((row) => row.workflow === "release.yml")) {
                  for (const checkout of row.checkouts ?? [])
                    assert.equal(checkout, event.ref, `${row.job} builds a different revision`);
                  for (const download of row.downloads ?? [])
                    assert.ok(
                      !download["run-id"] && !download.repository,
                      `cross-run artifacts in ${row.job}`,
                    );
                }
              }
              recorded.push({
                trigger: { name, action, ref: event.ref, sha: event.sha },
                event: label,
                diff,
                fork,
                publish: { pypi, npm, crate },
                classification: policy,
                rows,
              });
            }
  // Explicit release-only and mixed release PR routes share the same real workflow.
  for (const diff of Object.keys(histories))
    for (const fork of [false, true]) {
      const event = {
        label: "release-pr",
        name: "pull_request",
        action: "synchronize",
        branch: "main",
        payload: payload(fork),
      };
      const policy = classify(event.payload, histories[diff]);
      const rows = resolveRun("ci.yml", graph["ci.yml"], event, policy);
      assert.equal(rows.some(isMac), policy.macosRequired);
      const contexts = spawnSync(process.execPath, ["scripts/required-contexts.mjs", "--list"], {
        cwd: root,
        encoding: "utf8",
      });
      assert.equal(contexts.status, 0, contexts.stderr);
      for (const context of contexts.stdout.trim().split("\n"))
        assert.ok(
          rows.some((row) => {
            const display = row.display ?? row.job;
            const dims = Object.values(row.matrix ?? {});
            return (dims.length ? `${display} (${dims.join(", ")})` : display) === context;
          }),
          `missing ${context} for ${diff}/${fork}`,
        );
      recorded.push({ event: "release-pr", diff, fork, classification: policy, rows });
    }
  for (const pr of [{}, { pull_request: null }]) {
    const policy = classify(pr);
    const event = { label: "unknown-pr", name: "pull_request", action: "synchronize", payload: pr };
    const rows = resolveRun("ci.yml", graph["ci.yml"], event, policy);
    assert.ok(rows.some(isMac));
    recorded.push({ event: "unknown-pr", classification: policy, rows });
  }
  const diagnostic = graph["macos-diagnostic.yml"];
  assert.ok(
    !resolveRun(
      "macos-diagnostic.yml",
      diagnostic,
      { label: "automatic-opt-in", name: "repository_dispatch", inputs: { macos: true } },
      {},
    ).some(isMac),
  );
  const required = JSON.parse(
    readFileSync(join(root, ".github/tests/fixtures/required-contexts.json"), "utf8"),
  ).contexts;
  const actual = spawnSync(process.execPath, ["scripts/required-contexts.mjs", "--list"], {
    cwd: root,
    encoding: "utf8",
  });
  assert.equal(actual.status, 0, actual.stderr);
  assert.deepEqual(actual.stdout.trim().split("\n").sort(), required.sort());
  assert.equal(triggered(graph["ci.yml"], { name: "pull_request", action: "closed" }), false);
  assert.equal(
    triggered(graph["visual-docs.yml"], { name: "pull_request", action: "closed" }),
    true,
  );
  const lifecycles = [];
  // Actual release-plz automation creates the downstream release PR and published release.
  assert.match(
    readFileSync(join(root, ".github/workflows/release-plz.yml"), "utf8"),
    /release-plz release/,
  );
  for (const diff of Object.keys(histories))
    for (const fork of [false, true])
      for (const pypi of ["false", "true"])
        for (const npm of ["false", "true"])
          for (const crate of ["false", "true"]) {
            const select = (label) =>
              recorded.find(
                (r) =>
                  r.event === label &&
                  r.diff === diff &&
                  r.fork === fork &&
                  r.publish?.pypi === pypi &&
                  r.publish?.npm === npm &&
                  r.publish?.crate === crate,
              );
            const ordinary = select("pr-synchronize");
            const release = releases[diff];
            const releaseEvent = {
              label: "release-pr-lifecycle",
              name: "pull_request",
              action: "synchronize",
              payload: payload(),
              ref: "refs/pull/release/merge",
              sha: release.head,
            };
            const releasePolicy = classify(
              releaseEvent.payload,
              release.head,
              "pull_request",
              release.base,
            );
            assert.equal(releasePolicy.isReleaseOnly, true);
            const releasePr = {
              event: releaseEvent.label,
              rows: resolveRun("ci.yml", graph["ci.yml"], releaseEvent, releasePolicy),
            };
            const closures = [histories[diff], release.head].map((sha) => {
              const closed = {
                label: "merge-pr-closed",
                name: "pull_request",
                action: "closed",
                sha,
                ref: "refs/heads/main",
                payload: payload(),
              };
              return {
                event: closed.label,
                trigger: { sha },
                rows: Object.entries(graph)
                  .filter(([, workflow]) => triggered(workflow, closed))
                  .flatMap(([file, workflow]) =>
                    resolveRun(file, workflow, closed, {
                      macosRequired: false,
                      isReleaseOnly: false,
                    }),
                  ),
              };
            });
            const runs = [
              ordinary,
              select("base-push"),
              releasePr,
              select("release-merge"),
              select("release-tag"),
              select("release-published"),
              select("downstream"),
              select("repository-dispatch"),
              select("schedule"),
              ...closures,
            ].flatMap((s) => {
              const files = [...new Set(s.rows.map((r) => r.runWorkflow))];
              return files.map((workflow) => ({
                event: s.event,
                workflow,
                macJobs: s.rows.filter((r) => r.runWorkflow === workflow && isMac(r)),
                head: s.trigger?.sha ?? release.head,
                tag: s.event === "release-published" ? release.tag : null,
              }));
            });
            lifecycles.push({
              diff,
              fork,
              pypi,
              npm,
              crate,
              runs,
              macSpendingRuns: runs.filter((r) => r.macJobs.length).length,
            });
          }
  const runs = recorded.flatMap((s) =>
    [...new Set(s.rows.map((r) => r.runWorkflow))].map((workflow) => ({
      event: s.event,
      workflow,
      jobs: s.rows.filter((r) => r.runWorkflow === workflow && isMac(r)),
    })),
  );
  mkdirSync(join(root, ".github/.telemetry"), { recursive: true });
  writeFileSync(
    join(root, ".github/.telemetry/macos-policy.json"),
    JSON.stringify(
      {
        schemaVersion: 1,
        sourceDigest: sourceDigest(),
        scenarios: recorded,
        lifecycles,
        runs,
        jobsPerRun: Math.max(...runs.map((r) => r.jobs.length)),
        runsPerChange: Math.max(...lifecycles.map((l) => l.macSpendingRuns)),
      },
      null,
      2,
    ),
  );
});
test("unsupported calls, expressions and missing runner selections are explicit errors", () => {
  const event = { name: "pull_request", label: "unknown", payload: payload() };
  for (const job of [
    { uses: "unknown/repo/.github/workflows/file.yml@v1" },
    { "runs-on": `\${{ needs.missing.outputs.runner }}` },
    { "runs-on": "self-hosted" },
    { "runs-on": `\${{ unknown() }}` },
  ])
    assert.throws(() =>
      resolveRun("test", { jobs: { example: job } }, event, { macosRequired: true }),
    );
  assert.deepEqual(
    matrix(
      {
        os: ["ubuntu-latest", "windows-latest"],
        exclude: [{ os: "windows-latest" }],
        include: [{ os: "ubuntu-latest", arch: "x64" }],
      },
      {},
    ),
    [{ os: "ubuntu-latest", arch: "x64" }],
  );
});
test("real carriers fail on failed/cancelled/missing work and admit only classified omissions", () => {
  const cases = [
    ["true", "success", "success", "success"],
    ["false", "success", "success", "skipped"],
  ];
  for (const required of ["true", "false"])
    for (const proof of ["success", "failure", "cancelled", "skipped", ""])
      cases.push([required, "success", "success", proof]);
  for (const index of [0, 1, 2])
    for (const result of ["failure", "cancelled", "skipped", ""]) {
      const args = ["true", "success", "success", "success"];
      args[index] = result;
      cases.push(args);
    }
  for (const [required, changes, gate, proof] of cases) {
    const result = spawnSync(
      process.execPath,
      [join(root, ".github/macos-result.mjs"), required, changes, gate, proof],
      { encoding: "utf8" },
    );
    const valid =
      changes === "success" &&
      gate === "success" &&
      ((required === "true" && proof === "success") ||
        (required === "false" && proof === "skipped"));
    assert.equal(
      result.status,
      valid ? 0 : !["true", "false"].includes(required) ? 2 : 1,
      [required, changes, gate, proof].join("/"),
    );
    if (valid && required === "false") assert.match(result.stdout, /omitted/);
  }
});
test("reporters consume recorded telemetry and reject missing or stale data", () => {
  const path = join(root, ".github/.telemetry/macos-policy.json");
  const telemetry = JSON.parse(readFileSync(path, "utf8"));
  for (const [metric, key] of [
    ["jobs-per-run", "jobsPerRun"],
    ["runs-per-change", "runsPerChange"],
  ]) {
    const output = join(scratch, ".git", `${metric}.result`);
    const result = spawnSync(
      process.execPath,
      [join(root, ".github/macos-budget.mjs"), "--metric", metric, "--telemetry", path],
      { cwd: root, encoding: "utf8", env: { ...process.env, ONEBUDGETSPEC_RESULT: output } },
    );
    assert.equal(result.status, 0, result.stderr);
    const reported = JSON.parse(readFileSync(output, "utf8"));
    assert.equal(reported.value, telemetry[key]);
    assert.ok(JSON.parse(reported.detail).runs.length);
  }
  const stale = join(scratch, ".git/stale.json");
  writeFileSync(stale, JSON.stringify({ ...telemetry, sourceDigest: "stale" }));
  for (const path of [stale, `${stale}.missing`]) {
    const result = spawnSync(
      process.execPath,
      [join(root, ".github/macos-budget.mjs"), "--metric", "jobs-per-run", "--telemetry", path],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(result.status, 2);
  }
});
test("malformed or inconsistent count telemetry never becomes a measurement", () => {
  const telemetry = JSON.parse(
    readFileSync(join(root, ".github/.telemetry/macos-policy.json"), "utf8"),
  );
  const path = join(scratch, ".git/malformed-telemetry.json");
  for (const mutate of [
    (value) => {
      value.jobsPerRun = -1;
    },
    (value) => {
      value.runsPerChange = 0.5;
    },
    (value) => {
      value.jobsPerRun += 1;
    },
    (value) => {
      value.lifecycles[0].macSpendingRuns += 1;
    },
    (value) => {
      value.lifecycles[0].runs[0].macJobs = [null];
    },
    (value) => {
      value.lifecycles[0].runs[0].macJobs = [{ runner: ["ubuntu-latest"] }];
    },
    (value) => {
      value.runs = null;
    },
  ]) {
    const changed = structuredClone(telemetry);
    mutate(changed);
    writeFileSync(path, JSON.stringify(changed));
    const result = spawnSync(
      process.execPath,
      [join(root, ".github/macos-budget.mjs"), "--metric", "jobs-per-run", "--telemetry", path],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(result.status, 2);
    assert.match(result.stderr, /invalid|inconsistent/);
  }
});
test("real reusable matrix producer honors overrides and output contracts fail closed", () => {
  const snapshot = JSON.parse(readFileSync(join(root, ".github/reusable-workflows.json"), "utf8"));
  const workflow = parse(readFileSync(join(root, Object.values(snapshot)[0].path), "utf8"));
  const output = JSON.parse(
    screencompOutput(workflow.jobs.arches, {
      inputs: {
        arches: '["x86_64","aarch64"]',
        projects: '[{"id":"example","current":"shots/example","verify":"shots/verify"}]',
        "gallery-title": "Example",
      },
    }),
  );
  assert.deepEqual(
    output.include.map((row) => row.arch),
    ["x86_64", "aarch64"],
  );
  assert.ok(
    output.include.every((row) => row.project === "example" && row.current === "shots/example"),
  );
  for (const mutate of [
    (job) => {
      delete job.outputs.macos;
    },
    (job) => {
      job.steps.find((step) => step.id === "affected").run = "echo no-output";
    },
    (job) => {
      job.steps.find((step) => step.id === "policy").run = "echo no-classifier";
    },
  ]) {
    const job = structuredClone(graph["ci.yml"].jobs.changes);
    mutate(job);
    assert.throws(() => changesOutputs(job, { macosRequired: true }), /unresolved/);
  }
  assert.throws(
    () => triggered({ on: { push: { branches: ["main*"] } } }, { name: "push", branch: "main" }),
    /unresolved.*glob/,
  );
});
test("classifier and reporter refusal branches use their real CLI boundaries", () => {
  const event = join(scratch, ".git/refusal-event.json");
  writeFileSync(event, JSON.stringify(payload()));
  const classifier = join(root, ".github/macos-policy.mjs");
  for (const [comparisonBase, head] of [
    [base, base],
    ["-bad", histories.source],
  ]) {
    assert.equal(classify(payload(), head, "pull_request", comparisonBase).macosRequired, true);
  }
  git("checkout", "-q", "--detach", base);
  writeFileSync(join(scratch, "Cargo.lock"), "version=4\n");
  git("add", ".");
  git("commit", "-qm", "added admitted file");
  const added = git("rev-parse", "HEAD");
  assert.equal(classify(payload(), added).macosRequired, true);
  git("rm", "-q", "Cargo.lock");
  git("commit", "-qm", "deleted admitted file");
  assert.equal(
    classify(payload(), git("rev-parse", "HEAD"), "pull_request", added).macosRequired,
    true,
  );
  for (const argv of [
    [],
    ["wrong"],
    ["classify", "--event", event],
    ["classify", "--event", event, "--base", base, "--bad", base],
  ]) {
    assert.equal(
      spawnSync(process.execPath, [classifier, ...argv], { cwd: scratch, encoding: "utf8" }).status,
      2,
    );
  }
  for (const [path, text] of [
    [
      "npm/onemessagebus-sdk/src/version.ts",
      'export const SDK_VERSION: string = "0.9.0";\nexport const CLI_VERSION: string = "0.9.0";\n',
    ],
    [
      "python/onemessagebus-sdk/src/onemessagebus/_version.py",
      '__version__ = "0.9.0"\nCLI_VERSION = "0.9.0"\n',
    ],
  ]) {
    git("checkout", "-q", "--detach", base);
    mkdirSync(join(scratch, path, ".."), { recursive: true });
    writeFileSync(join(scratch, path), text);
    git("add", ".");
    git("commit", "-qm", "declaration baseline");
    const start = git("rev-parse", "HEAD");
    for (const changed of [
      `${text.split("\n")[0]}\n`,
      text.replace("CLI_VERSION", path.endsWith(".ts") ? "SDK_VERSION" : "__version__"),
    ]) {
      writeFileSync(join(scratch, path), changed);
      git("add", ".");
      git("commit", "-qm", "invalid declarations");
      assert.equal(
        classify(payload(), git("rev-parse", "HEAD"), "pull_request", start).macosRequired,
        true,
      );
    }
  }
  const reporter = join(root, ".github/macos-budget.mjs");
  for (const argv of [
    [],
    ["--metric", "wrong", "--telemetry", event],
    ["--bad", "jobs-per-run", "--telemetry", event],
  ])
    assert.equal(
      spawnSync(process.execPath, [reporter, ...argv], { cwd: root, encoding: "utf8" }).status,
      2,
    );
  const original = JSON.parse(
    readFileSync(join(root, ".github/.telemetry/macos-policy.json"), "utf8"),
  );
  const path = join(scratch, ".git/refusal-telemetry.json");
  for (const mutate of [
    (value) => {
      value.schemaVersion = 2;
    },
    (value) => {
      value.runs = [];
    },
    (value) => {
      delete value.lifecycles;
    },
    (value) => {
      value.runs[0].jobs = [null];
    },
    (value) => {
      value.runs[0].jobs = [{ runner: ["ubuntu-latest"] }];
    },
    (value) => {
      value.lifecycles[0].runs = [{ macJobs: null }];
    },
  ]) {
    const changed = structuredClone(original);
    mutate(changed);
    writeFileSync(path, JSON.stringify(changed));
    assert.equal(
      spawnSync(process.execPath, [reporter, "--metric", "jobs-per-run", "--telemetry", path], {
        cwd: root,
        encoding: "utf8",
      }).status,
      2,
    );
  }
  writeFileSync(path, "invalid JSON");
  assert.equal(
    spawnSync(process.execPath, [reporter, "--metric", "jobs-per-run", "--telemetry", path], {
      cwd: root,
      encoding: "utf8",
    }).status,
    2,
  );
});
test.after(() => rmSync(scratch, { recursive: true, force: true }));

test("source digest tracks content and deletion while excluding budget thresholds", () => {
  const original = sourceDigest(scratch);
  const file = join(scratch, "Cargo.toml");
  const content = readFileSync(file);
  writeFileSync(file, Buffer.concat([content, Buffer.from("\n# mutation\n")]));
  assert.notEqual(sourceDigest(scratch), original);
  writeFileSync(file, content);
  assert.equal(sourceDigest(scratch), original);
  rmSync(file);
  assert.notEqual(sourceDigest(scratch), original);
  writeFileSync(file, content);
  const budget = join(scratch, "budgets.yaml");
  writeFileSync(budget, "threshold: 1\n");
  assert.equal(sourceDigest(scratch), original);
  writeFileSync(budget, "threshold: 99\n");
  assert.equal(sourceDigest(scratch), original);
  rmSync(budget);
});

test("budget checker enforces recorded measurements and measurement errors", () => {
  const original = parse(readFileSync(join(root, ".github/budgets.yaml"), "utf8"));
  const fixture = join(scratch, ".git/check-budgets.yaml");
  for (const outcome of ["within", "over", "error"]) {
    const document = structuredClone(original);
    for (const budget of document.budgets) {
      budget.command[1] = join(root, ".github/macos-budget.mjs");
      budget.command[5] = join(root, ".github/.telemetry/macos-policy.json");
      if (outcome === "over") budget.threshold = 0;
      if (outcome === "error") budget.command[5] += ".missing";
    }
    writeFileSync(fixture, stringify(document));
    const result = spawnSync(process.execPath, [join(root, ".github/check-budgets.mjs"), fixture], {
      cwd: root,
      encoding: "utf8",
    });
    assert.equal(result.status, outcome === "within" ? 0 : 1, result.stderr);
    assert.match(
      result.stdout,
      outcome === "within" ? /within/ : outcome === "over" ? /over/ : /error/,
    );
  }
});

test("actual README installation command selects the PR repository and exact head", () => {
  const ci = parse(readFileSync(join(root, ".github/workflows/ci.yml"), "utf8"));
  const step = ci.jobs["macos-proof"].steps.find((step) => step.run?.includes("documented=$(grep"));
  assert.ok(step);
  const directory = join(scratch, ".git/readme-install");
  mkdirSync(directory);
  const command = readFileSync(join(root, "README.md"), "utf8")
    .split("\n")
    .find((line) => line.startsWith("cargo install --git "));
  assert.ok(command);
  writeFileSync(join(directory, "README.md"), command);
  const cargo = join(directory, "cargo");
  writeFileSync(cargo, '#!/bin/bash\nprintf "%s\n" "$@" > "$ARGV_RECORD"\n', { mode: 0o755 });
  const record = join(directory, "argv");
  const env = {
    ...process.env,
    PATH: `${directory}:${process.env.PATH}`,
    ARGV_RECORD: record,
    PR_REPOSITORY: "https://github.com/fork-owner/onemessagebus",
    PR_HEAD: "0123456789abcdef",
  };
  const result = spawnSync("bash", ["-e", "-c", step.run], {
    cwd: directory,
    env,
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  const argv = readFileSync(record, "utf8").trim().split("\n");
  assert.equal(argv[argv.indexOf("--git") + 1], env.PR_REPOSITORY);
  assert.equal(argv[argv.indexOf("--rev") + 1], env.PR_HEAD);
  assert.equal(argv.at(-1), "--force");
  rmSync(record);
  for (const content of ["", `${command}; touch unsafe`]) {
    writeFileSync(join(directory, "README.md"), content);
    const refused = spawnSync("bash", ["-e", "-c", step.run], {
      cwd: directory,
      env,
      encoding: "utf8",
    });
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /no longer|characters/);
    assert.throws(() => readFileSync(record));
  }
});
