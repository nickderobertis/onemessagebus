import assert from "node:assert/strict";
import { test } from "node:test";
import { workflows, read } from "../workflow-graph.mjs";
const release = workflows()["release.yml"];
const targets = [...read("scripts/npm-build.mjs").matchAll(/^ {2}"([^"]+)": \{ platform:/gm)].map(
  (m) => m[1],
);
const darwin = targets.filter((target) => target.endsWith("apple-darwin"));
test("all declared targets retain archives, wheels and npm carriers, and both SDK distributions", () => {
  const staged = release.jobs["build-darwin"];
  const commands = staged.steps.map((step) => step.run ?? "").join("\n");
  for (const producer of ["upload", "build-wheels", "build-npm"]) {
    assert.deepEqual(
      [
        ...release.jobs[producer].strategy.matrix.include.map((leg) => leg.target),
        ...darwin,
      ].sort(),
      [...targets].sort(),
    );
  }
  for (const target of darwin) {
    assert.equal(commands.split(`bash scripts/release-build.sh ${target} `).length - 1, 1);
    assert.match(commands, new RegExp(`bash scripts/release-local-install.sh staged/${target} `));
    for (const kind of ["wheels", "npm"])
      assert.ok(
        staged.steps.some(
          (step) =>
            step.uses === "actions/upload-artifact@v4" && step.with.name === `${kind}-${target}`,
        ),
      );
  }
  for (const [publisher, producer] of [
    ["publish-pypi", "build-wheels"],
    ["publish-npm", "build-npm"],
  ])
    assert.deepEqual(release.jobs[publisher].needs, [producer, "build-darwin"]);
  for (const sdk of ["python", "node"]) {
    assert.ok(release.jobs[`build-${sdk}-sdk`]);
    assert.ok(release.jobs[`publish-${sdk}-sdk`]);
    assert.match(commands, new RegExp(`just ${sdk}-sdk-dist staged/sdks`));
  }
  assert.deepEqual(release.jobs["publish-crate"].needs, ["guard", "build-darwin"]);
  assert.equal(release.jobs["upload-darwin"]["runs-on"], "ubuntu-latest");
  assert.equal(release.jobs["upload-darwin"].needs, "build-darwin");
  assert.match(commands, /softwareupdate|release-local-install/);
  assert.ok(staged.steps.some((step) => step.run?.includes("softwareupdate --install-rosetta")));
  for (const arch of ["x64", "arm64"])
    for (const action of ["actions/setup-python@v5", "actions/setup-node@v4"])
      assert.ok(
        staged.steps.some((step) => step.uses === action && step.with.architecture === arch),
      );
  const builder = read("scripts/release-build.sh");
  assert.equal(builder.split("maturin build ").length - 1, 1);
  assert.doesNotMatch(builder, /cargo build/);
  assert.match(builder, /--release --locked --strip true/);
  const installer = read("scripts/release-local-install.sh");
  assert.ok(installer.includes('PATH="$(dirname "$node"):$PATH"\nexport PATH'));
  for (const proof of ["smoke-published.sh", "sdk-install/smoke.py", "sdk-install/smoke.mjs"])
    assert.ok(installer.includes(proof));
  assert.doesNotMatch(installer, /\b(?:npm|cargo) publish\b|retry-install/);
});
test("helper context exclusion requires three honest always-running Linux carriers", () => {
  const ci = workflows()["ci.yml"];
  const carriers = Object.values(ci.jobs).filter((job) => job.needs?.includes("macos-proof"));
  assert.equal(carriers.length, 3);
  for (const job of carriers) {
    assert.equal(job.if, "always()");
    assert.equal(job["runs-on"], "ubuntu-latest");
    const step = job.steps.find((step) => step.run?.includes("node .github/macos-result.mjs"));
    assert.equal(step.env.PROOF, `\${{ needs.macos-proof.result }}`);
    assert.equal(step.env.REQUIRED, `\${{ needs.changes.outputs.macos }}`);
  }
});
