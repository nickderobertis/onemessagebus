// .github/bootstrap.sh's two routes to actionlint, run as `just bootstrap` runs
// it, in a scratch checkout.
//
// The PATH holds only the tools the script needs, so no `go` or `actionlint` of
// this host leaks in. Doubled are `go` (records its argv and drops a binary
// into GOBIN), `curl` (serves the release assets from a local directory and
// records each URL, so nothing leaves this machine) and `uname` (stands in for
// the hosted macOS runner).
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, describe, it } from "node:test";
import { root } from "../workflow-graph.mjs";

const version = readFileSync(join(root, ".github/bootstrap.sh"), "utf8").match(
  /^actionlint_version=(\S+)$/m,
)[1];
const release = `https://github.com/rhysd/actionlint/releases/download/v${version}`;
const archive = `actionlint_${version}_darwin_arm64.tar.gz`;
const checksums = `actionlint_${version}_checksums.txt`;
const REAL = [
  "bash",
  "mkdir",
  "mktemp",
  "rm",
  "grep",
  "shasum",
  "perl",
  "tar",
  "gzip",
  "cat",
  "cp",
  "chmod",
];

const which = (tool) =>
  spawnSync("bash", ["-c", `command -v ${tool}`], { encoding: "utf8" }).stdout.trim();

const script = (path, body) => {
  writeFileSync(path, `#!${which("bash")}\n${body}`);
  chmodSync(path, 0o755);
};

let scratch;
let assets;

before(() => {
  scratch = mkdtempSync(join(tmpdir(), "bootstrap-"));
  // The release as GitHub serves it: an archive holding `actionlint`, and the
  // checksums file naming it.
  assets = join(scratch, "assets");
  const payload = join(scratch, "payload");
  mkdirSync(assets);
  mkdirSync(payload);
  script(join(payload, "actionlint"), "echo prebuilt actionlint\n");
  spawnSync("tar", ["-czf", join(assets, archive), "-C", payload, "actionlint"]);
  const sha = createHash("sha256")
    .update(readFileSync(join(assets, archive)))
    .digest("hex");
  writeFileSync(join(assets, checksums), `${sha}  ${archive}\n${"0".repeat(64)}  other.tar.gz\n`);
});

after(() => rmSync(scratch, { recursive: true, force: true }));

/// Runs a fresh checkout's bootstrap.sh with `go` present or absent and `uname`
/// reporting `machine`; answers its exit, stderr, recorded calls and checkout.
function bootstrap({ go, machine = "Darwin arm64", served = assets }) {
  const run = mkdtempSync(join(scratch, "run-"));
  const checkout = join(run, "checkout");
  const bin = join(run, "bin");
  const record = join(run, "calls");
  mkdirSync(join(checkout, ".github"), { recursive: true });
  mkdirSync(bin);
  cpSync(join(root, ".github/bootstrap.sh"), join(checkout, ".github/bootstrap.sh"));
  for (const tool of REAL) symlinkSync(which(tool), join(bin, tool));
  writeFileSync(record, "");
  const [system, arch] = machine.split(" ");
  script(join(bin, "uname"), `case "$1" in -s) echo ${system} ;; -m) echo ${arch} ;; esac\n`);
  script(
    join(bin, "curl"),
    `out= url=
while [ $# -gt 0 ]; do case "$1" in -o) out=$2; shift 2 ;; -*) shift ;; *) url=$1; shift ;; esac; done
printf 'curl %s\\n' "$url" >> ${JSON.stringify(record)}
cp ${JSON.stringify(served)}/"\${url##*/}" "$out"
`,
  );
  if (go) {
    script(
      join(bin, "go"),
      `printf 'go %s GOBIN=%s\\n' "$*" "$GOBIN" >> ${JSON.stringify(record)}
printf '#!/bin/sh\\necho built actionlint\\n' > "$GOBIN/actionlint"
chmod +x "$GOBIN/actionlint"
`,
    );
  }
  const result = spawnSync(which("bash"), [".github/bootstrap.sh"], {
    cwd: checkout,
    encoding: "utf8",
    env: { PATH: bin, HOME: run },
  });
  const tool = join(checkout, ".github/.tools/actionlint");
  return {
    status: result.status,
    stderr: result.stderr,
    calls: readFileSync(record, "utf8").trim().split("\n").filter(Boolean),
    installed: existsSync(tool)
      ? spawnSync(tool, [], { encoding: "utf8", env: { PATH: bin } }).stdout.trim()
      : null,
    tools: join(checkout, ".github/.tools"),
  };
}

describe("bootstrap.sh installs actionlint", () => {
  it("builds the pinned version with go where Go is present, fetching nothing", () => {
    const run = bootstrap({ go: true });
    assert.equal(run.status, 0, run.stderr);
    assert.deepEqual(run.calls, [
      `go install github.com/rhysd/actionlint/cmd/actionlint@v${version} GOBIN=${run.tools}`,
    ]);
    assert.equal(run.installed, "built actionlint");
  });

  it("installs the same version's prebuilt arm64 macOS binary where Go is absent", () => {
    const run = bootstrap({ go: false });
    assert.equal(run.status, 0, run.stderr);
    assert.deepEqual(run.calls, [`curl ${release}/${archive}`, `curl ${release}/${checksums}`]);
    assert.equal(run.installed, "prebuilt actionlint");
  });

  it("refuses an archive that does not match the release's checksums", () => {
    const tampered = join(scratch, "tampered");
    cpSync(assets, tampered, { recursive: true });
    writeFileSync(join(tampered, archive), "not the release's archive");
    const run = bootstrap({ go: false, served: tampered });
    assert.equal(run.status, 1);
    assert.match(run.stderr, new RegExp(`${archive} does not match .*${checksums} — refusing`));
    assert.equal(run.installed, null);
  });

  it("names the fix where no prebuilt binary exists for the machine and Go is absent", () => {
    const run = bootstrap({ go: false, machine: "Darwin x86_64" });
    assert.equal(run.status, 1);
    assert.match(
      run.stderr,
      /no Go and no prebuilt actionlint for Darwin\/x86_64 — install Go or actionlint/,
    );
    assert.deepEqual(run.calls, []);
  });
});
