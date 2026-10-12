// Production release builder and installers, publishing disabled.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";
const root = resolve(import.meta.dirname, "..");
const run = (command, args) =>
  execFileSync(command, args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
test("one real native build supplies identical wheel/archive/npm bytes and installed CLI/SDKs", {
  timeout: 600000,
}, () => {
  const work = mkdtempSync(
    join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "release-stage-"),
  );
  try {
    const target = run("rustc", ["-vV"]).match(/^host: (.+)$/m)[1];
    const stage = join(work, target),
      sdks = join(work, "sdks");
    run("bash", ["scripts/release-build.sh", target, stage]);
    run("just", ["python-sdk-dist", sdks]);
    run("just", ["node-sdk-dist", sdks]);
    const inventory = JSON.parse(readFileSync(join(stage, "binary.json"), "utf8"));
    assert.equal(inventory.target, target);
    const contract = JSON.parse(
      run("python3", [
        "-c",
        `
import json,zipfile,tarfile,hashlib,sys
from pathlib import Path
p=Path(sys.argv[1]); info=json.loads((p/'binary.json').read_text())
with zipfile.ZipFile(p/info['wheel']) as z:
 binary=z.read(next(n for n in z.namelist() if '.data/scripts/onemessagebus' in n))
 metadata=z.read(next(n for n in z.namelist() if n.endswith('/METADATA'))).decode()
 tag=z.read(next(n for n in z.namelist() if n.endswith('/WHEEL'))).decode()
with tarfile.open(p/info['archive']) as t:
 archive=t.extractfile('onemessagebus').read(); contents=t.getnames()
with tarfile.open(next(p.glob('*.tgz'))) as t:
 npm=t.extractfile('package/bin/onemessagebus').read()
 package=json.load(t.extractfile('package/package.json'))
print(json.dumps(dict(hashes=[hashlib.sha256(b).hexdigest() for b in [binary,archive,npm]],metadata=metadata,tag=tag,contents=contents,package=package)))
`,
        stage,
      ]),
    );
    assert.deepEqual(contract.hashes, [inventory.sha256, inventory.sha256, inventory.sha256]);
    assert.match(
      contract.metadata,
      new RegExp(`Version: ${inventory.version.replaceAll(".", "\\.")}\\n`),
    );
    assert.match(contract.tag, /Tag: py3-none-/);
    assert.deepEqual(
      contract.contents.sort(),
      ["CHANGELOG.md", "LICENSE", "README.md", "onemessagebus"].sort(),
    );
    assert.equal(contract.package.version, inventory.version);
    assert.deepEqual(contract.package.cpu, [process.arch]);
    assert.deepEqual(contract.package.os, [process.platform]);
    assert.deepEqual(Object.values(inventory.hashes), contract.hashes);
    run("bash", [
      "scripts/release-local-install.sh",
      stage,
      sdks,
      run("which", ["python3"]).trim(),
      process.execPath,
    ]);
    // Corrupt a packed carrier and drive the production hash boundary.
    run("python3", [
      "-c",
      `
import io,tarfile,sys
from pathlib import Path
p=next(Path(sys.argv[1]).glob('*.tgz')); replacement=p.with_suffix('.changed')
with tarfile.open(p) as source, tarfile.open(replacement,'w:gz') as out:
 for member in source.getmembers():
  content=source.extractfile(member).read() if member.isfile() else None
  if member.name=='package/bin/onemessagebus': content+=b'corrupt'; member.size=len(content)
  out.addfile(member,io.BytesIO(content) if content is not None else None)
replacement.replace(p)
`,
      stage,
    ]);
    const mismatch = spawnSync(
      "python3",
      ["scripts/release-stage.py", "--target", target, "--directory", stage, "--verify-npm"],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(mismatch.status, 2);
    assert.match(mismatch.stderr, /npm payload differs/);
    // The standalone stage boundary rejects unsafe targets and lookalike scripts.
    const unsafe = spawnSync(
      "python3",
      ["scripts/release-stage.py", "--target", "../../apple-darwin", "--directory", stage],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(unsafe.status, 2);
    assert.match(unsafe.stderr, /unsupported target/);
    run("python3", [
      "-c",
      `
import zipfile,sys
from pathlib import Path
p=next(Path(sys.argv[1]).glob('*.whl')); replacement=p.with_suffix('.changed')
with zipfile.ZipFile(p) as source, zipfile.ZipFile(replacement,'w') as out:
 for member in source.infolist():
  data=source.read(member.filename)
  if '.data/scripts/onemessagebus' in member.filename: member.filename+='-other'
  out.writestr(member,data)
replacement.replace(p)
`,
      stage,
    ]);
    const lookalike = spawnSync(
      "python3",
      ["scripts/release-stage.py", "--target", target, "--directory", stage],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(lookalike.status, 2);
    assert.match(lookalike.stderr, /exact onemessagebus script/);
    // Refuse malformed target before producing an artifact.
    const refused = spawnSync(
      "bash",
      ["scripts/release-build.sh", "unsupported-target", join(work, "bad")],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(refused.status, 2);
    assert.match(refused.stderr, /declared Darwin or Linux target/);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
});
