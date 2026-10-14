// The macOS artifacts a release produces and publishes, proved at the artifact
// contract on any host.
//
// build-darwin compiles on an Apple Silicon runner, which no Linux gate can do,
// so a fixture wheel stands in for maturin's output: maturin's file name, tag
// and metadata around a Mach-O arm64 header. Everything after the compile is the
// real interface the workflow runs — scripts/release-stage.py, the platform
// packager in scripts/npm-build.mjs, `npm pack`, the npm verification pass — and
// the workflow's own upload globs, `gh release upload` command and npm publish
// step are resolved and run against what it staged. Only the forge (`gh`) and the
// registry (`npm view` / `npm publish`) are doubled, and publication never
// leaves this machine.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  chmodSync,
  cpSync,
  globSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, delimiter, dirname, join, relative } from "node:path";
import { after, before, describe, it } from "node:test";
import { root, workflows } from "../workflow-graph.mjs";

const release = workflows()["release.yml"];
const version = readFileSync(join(root, "Cargo.toml"), "utf8").match(/^version = "([^"]+)"/m)[1];
const tag = "macosx_11_0_arm64";
const MACHO_ARM64 = "cffaedfe0c000001";

const run = (command, args, options = {}) =>
  execFileSync(command, args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  });

/// What one Darwin `release-build.sh` target compiles to: maturin's wheel for it.
function compiledWheel(directory) {
  run("python3", [
    "-I",
    "-c",
    `
import sys, zipfile
directory, version, tag = sys.argv[1:]
binary = bytes.fromhex("${MACHO_ARM64}") + bytes(24)
data, info = f"onemessagebus_cli-{version}.data", f"onemessagebus_cli-{version}.dist-info"
with zipfile.ZipFile(f"{directory}/onemessagebus_cli-{version}-py3-none-{tag}.whl", "w") as wheel:
    wheel.writestr(f"{data}/scripts/onemessagebus", binary)
    wheel.writestr(f"{info}/METADATA", f"Metadata-Version: 2.4\\nName: onemessagebus-cli\\nVersion: {version}\\n")
    wheel.writestr(f"{info}/WHEEL", f"Wheel-Version: 1.0\\nGenerator: maturin\\nRoot-Is-Purelib: false\\nTag: py3-none-{tag}\\n")
`,
    directory,
    version,
    tag,
  ]);
}

/// A directory of doubles put first on PATH: `gh` records its argv, and `npm`
/// answers `view` with the registry's 404 and records `publish`, passing every
/// local command (`pack`) to the real npm.
function transportDoubles(directory, record) {
  // The npm installed beside this Node, not whatever `npm` is on PATH: a version
  // manager's shim there finds its target by searching PATH, which would find
  // this double and loop.
  const npm = join(dirname(process.execPath), "npm");
  writeFileSync(
    join(directory, "gh"),
    `#!/usr/bin/env bash\nprintf 'gh %s\\n' "$*" >> ${JSON.stringify(record)}\n`,
  );
  writeFileSync(
    join(directory, "npm"),
    [
      "#!/usr/bin/env bash",
      'case "$1" in',
      "  view) echo 'npm error code E404' >&2; echo 'npm error 404 Not Found' >&2; exit 1 ;;",
      `  publish) printf 'npm publish %s\\n' "$2" >> ${JSON.stringify(record)} ;;`,
      `  *) exec ${JSON.stringify(npm)} "$@" ;;`,
      "esac",
      "",
    ].join("\n"),
  );
  for (const tool of ["gh", "npm"]) chmodSync(join(directory, tool), 0o755);
}

/// The step of `job` whose `run` contains `marker`.
const step = (job, marker) => release.jobs[job].steps.find((s) => s.run?.includes(marker));

describe("the macOS release artifacts", () => {
  const darwin = release.jobs["build-darwin"];
  const commands = darwin.steps.map((s) => s.run ?? "").join("\n");
  const targets = [...commands.matchAll(/bash scripts\/release-build\.sh (\S+) staged\/\1\b/g)].map(
    (m) => m[1],
  );
  let work;
  let runner;
  let artifacts;

  before(() => {
    work = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "darwin-"));
    runner = join(work, "runner");
    for (const target of targets) {
      const stage = join(runner, "staged", target);
      mkdirSync(stage, { recursive: true });
      compiledWheel(stage);
      // release-build.sh's steps after its one compile, in its order.
      run("python3", ["scripts/release-stage.py", "--target", target, "--directory", stage]);
      const pkg = run("node", [
        "scripts/npm-build.mjs",
        "platform",
        "--target",
        target,
        "--binary",
        join(stage, "onemessagebus"),
        "--out",
        join(stage, "npm"),
      ]).trim();
      run("npm", ["pack", "--pack-destination", stage], { cwd: pkg });
      run("python3", [
        "scripts/release-stage.py",
        "--target",
        target,
        "--directory",
        stage,
        "--verify-npm",
      ]);
    }
    // Each artifact build-darwin uploads, resolved from its own path globs.
    artifacts = Object.fromEntries(
      darwin.steps
        .filter((s) => s.uses === "actions/upload-artifact@v4")
        .map((s) => [
          s.with.name,
          s.with.path
            .split("\n")
            .filter(Boolean)
            .flatMap((pattern) => globSync(pattern, { cwd: runner }))
            .sort(),
        ]),
    );
  });

  after(() => {
    if (work) rmSync(work, { recursive: true, force: true });
  });

  it("builds Apple Silicon alone, natively", () => {
    assert.deepEqual(targets, ["aarch64-apple-darwin"]);
    assert.ok(darwin["runs-on"].startsWith("macos-"));
    assert.doesNotMatch(commands, /arch -x86_64|rosetta|x86_64-apple-darwin/i);
  });

  it("uploads exactly the arm64 wheel, npm package, archive and checksum", () => {
    const stage = "staged/aarch64-apple-darwin";
    const archive = `onemessagebus-v${version}-aarch64-apple-darwin.tar.gz`;
    assert.deepEqual(artifacts, {
      "wheels-aarch64-apple-darwin": [`${stage}/onemessagebus_cli-${version}-py3-none-${tag}.whl`],
      "npm-aarch64-apple-darwin": [`${stage}/onemessagebus-cli-darwin-arm64-${version}.tgz`],
      "darwin-archives": [
        `${stage}/${archive}`,
        `${stage}/${archive}.sha256`,
        `${stage}/binary.json`,
      ].sort(),
    });
    const inventory = JSON.parse(readFileSync(join(runner, stage, "binary.json"), "utf8"));
    assert.equal(inventory.target, "aarch64-apple-darwin");
    assert.equal(inventory.archive, archive);
    // Every carrier holds the one arm64 Mach-O the wheel compiled.
    const carried = JSON.parse(
      run("python3", [
        "-I",
        "-c",
        `
import json, sys, tarfile, zipfile
from pathlib import Path
stage = Path(sys.argv[1])
wheel = zipfile.ZipFile(next(stage.glob("*.whl")))
with tarfile.open(next(stage.glob("*.tar.gz"))) as t: archive = t.extractfile("onemessagebus").read()
with tarfile.open(next(stage.glob("*.tgz"))) as t:
    npm = t.extractfile("package/bin/onemessagebus").read()
    manifest = json.load(t.extractfile("package/package.json"))
wheel_binary = wheel.read(next(n for n in wheel.namelist() if n.endswith(".data/scripts/onemessagebus")))
tags = [l for l in wheel.read(next(n for n in wheel.namelist() if n.endswith("/WHEEL"))).decode().splitlines() if l.startswith("Tag:")]
print(json.dumps(dict(headers=[b[:8].hex() for b in (wheel_binary, archive, npm)], tags=tags, manifest=manifest)))
`,
        join(runner, stage),
      ]),
    );
    assert.deepEqual(carried.headers, [MACHO_ARM64, MACHO_ARM64, MACHO_ARM64]);
    assert.deepEqual(carried.tags, [`Tag: py3-none-${tag}`]);
    assert.equal(carried.manifest.name, "onemessagebus-cli-darwin-arm64");
    assert.equal(carried.manifest.version, version);
    assert.deepEqual(carried.manifest.os, ["darwin"]);
    assert.deepEqual(carried.manifest.cpu, ["arm64"]);
  });

  it("attaches the arm64 archive and checksum to the Release, and nothing Intel", () => {
    // upload-darwin downloads darwin-archives into `staged`; upload-artifact
    // keeps paths below the globs' common root, which is `staged`.
    const job = join(work, "upload-darwin");
    for (const file of artifacts["darwin-archives"])
      cpSync(join(runner, file), join(job, file), { recursive: true });
    const doubles = join(work, "doubles-gh");
    const record = join(work, "gh.log");
    mkdirSync(doubles);
    transportDoubles(doubles, record);
    run("bash", ["-euo", "pipefail", "-c", step("upload-darwin", "gh release").run], {
      cwd: job,
      env: {
        ...process.env,
        PATH: `${doubles}${delimiter}${process.env.PATH}`,
        GITHUB_REF_NAME: `v${version}`,
      },
    });
    const archive = `staged/aarch64-apple-darwin/onemessagebus-v${version}-aarch64-apple-darwin.tar.gz`;
    assert.equal(
      readFileSync(record, "utf8"),
      `gh release upload v${version} ${archive} ${archive}.sha256 --clobber\n`,
    );
  });

  it("publishes the arm64 platform package and a launcher that names no Intel Mac package", () => {
    // publish-npm's checkout: the two scripts its step runs, and what they read.
    const checkout = join(work, "checkout");
    for (const path of [
      "Cargo.toml",
      "scripts/npm-build.mjs",
      "scripts/publish-npm.sh",
      "npm/onemessagebus-cli",
    ])
      cpSync(join(root, path), join(checkout, path), { recursive: true });
    // download-artifact's `npm-*` pattern, merged; the Linux and Windows legs'
    // artifacts come from matrices this does not run.
    const downloads = join(checkout, "npm-artifacts");
    mkdirSync(downloads);
    for (const file of artifacts["npm-aarch64-apple-darwin"])
      cpSync(join(runner, file), join(downloads, basename(file)));
    const doubles = join(work, "doubles-npm");
    const record = join(work, "npm.log");
    mkdirSync(doubles);
    transportDoubles(doubles, record);
    run("bash", ["-euo", "pipefail", "-c", step("publish-npm", "publish-npm.sh").run], {
      cwd: checkout,
      env: {
        ...process.env,
        PATH: `${doubles}${delimiter}${process.env.PATH}`,
        GITHUB_WORKSPACE: checkout,
      },
    });
    const published = readFileSync(record, "utf8")
      .trim()
      .split("\n")
      .map((line) => relative(checkout, line.replace(/^npm publish /, "")));
    assert.deepEqual(published, [
      `npm-artifacts/onemessagebus-cli-darwin-arm64-${version}.tgz`,
      "npm/dist/onemessagebus-cli",
    ]);
    const launcher = JSON.parse(
      readFileSync(join(checkout, "npm/dist/onemessagebus-cli/package.json"), "utf8"),
    );
    assert.deepEqual(
      Object.keys(launcher.optionalDependencies).filter((name) => name.includes("darwin")),
      ["onemessagebus-cli-darwin-arm64"],
    );
  });

  it("refuses the Intel macOS target at every packaging step instead of building it", () => {
    const stage = join(work, "intel");
    mkdirSync(stage);
    const build = spawnSync("bash", ["scripts/release-build.sh", "x86_64-apple-darwin", stage], {
      cwd: root,
      encoding: "utf8",
    });
    assert.equal(build.status, 2, build.stderr);
    assert.match(build.stderr, /Intel macOS \(x86_64-apple-darwin\) is no longer released/);
    assert.deepEqual(readdirSync(stage), []);
    const staged = spawnSync(
      "python3",
      ["scripts/release-stage.py", "--target", "x86_64-apple-darwin", "--directory", stage],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(staged.status, 2, staged.stderr);
    assert.match(staged.stderr, /Intel macOS \(x86_64-apple-darwin\) is no longer released/);
    const binary = join(work, "binary");
    writeFileSync(binary, "");
    const packed = spawnSync(
      "node",
      [
        "scripts/npm-build.mjs",
        "platform",
        "--target",
        "x86_64-apple-darwin",
        "--binary",
        binary,
        "--out",
        stage,
      ],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(packed.status, 2, packed.stderr);
    assert.match(packed.stderr, /x86_64-apple-darwin/);
    assert.deepEqual(readdirSync(stage), []);
  });
});
