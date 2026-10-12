// Production release builder and installers, publishing disabled.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  mkdtempSync,
  rmSync,
  readFileSync,
  writeFileSync,
  cpSync,
  mkdirSync,
  copyFileSync,
} from "node:fs";
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
    const occupied = join(work, "occupied");
    mkdirSync(occupied);
    const sentinel = join(occupied, "keep");
    copyFileSync(join(root, "Cargo.toml"), sentinel);
    for (const directory of [root, occupied]) {
      const refused = spawnSync("bash", ["scripts/release-build.sh", target, directory], {
        cwd: root,
        encoding: "utf8",
      });
      assert.equal(refused.status, 2, refused.stderr);
      assert.match(refused.stderr, /empty staging directory/);
      assert.ok(readFileSync(join(root, "Cargo.toml")).length);
      assert.ok(readFileSync(sentinel).length);
    }
    const logFailure = spawnSync(
      "bash",
      ["-c", "source scripts/release-quiet.sh; release_quiet echo should-not-run"],
      {
        cwd: root,
        encoding: "utf8",
        env: { ...process.env, ONEPIPELINE_NODE_SCRATCH_DIR: join(work, "missing-scratch") },
      },
    );
    assert.equal(logFailure.status, 1);
    assert.match(logFailure.stderr, /select a writable directory and rerun/);
    assert.doesNotMatch(logFailure.stdout, /should-not-run/);
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
print(json.dumps(dict(hashes=[hashlib.sha256(b).hexdigest() for b in [binary,archive,npm]],metadata=metadata,tag=tag,contents=contents,package=package,checksum=(p/(info['archive']+'.sha256')).read_text(),archiveHash=hashlib.sha256((p/info['archive']).read_bytes()).hexdigest())))
`,
        stage,
      ]),
    );
    assert.equal(contract.checksum, `${contract.archiveHash}  ${inventory.archive}\n`);
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
    const installOutput = run("bash", [
      "scripts/release-local-install.sh",
      stage,
      sdks,
      run("which", ["python3"]).trim(),
      process.execPath,
    ]);
    assert.equal(installOutput.trim(), "staged CLI and SDK installations passed");
    const python = resolve(root, run("which", ["python3"]).trim());
    const empty = join(work, "empty");
    mkdirSync(empty);
    for (const kind of ["python-sdk", "missing-npm", "npm-cli", "missing-node"]) {
      const badStage = join(work, kind, "stage"),
        badSdks = join(work, kind, "sdks");
      cpSync(stage, badStage, { recursive: true });
      cpSync(sdks, badSdks, { recursive: true });
      run("python3", [
        "-c",
        `
import io,tarfile,zipfile,sys
from pathlib import Path
stage=Path(sys.argv[1]); sdks=Path(sys.argv[2]); kind=sys.argv[3]
if kind=='python-sdk':
 wheel=next(sdks.glob('*.whl')); replacement=wheel.with_suffix('.changed')
 with zipfile.ZipFile(wheel) as source, zipfile.ZipFile(replacement,'w') as out:
  for member in source.infolist():
   content=source.read(member.filename)
   if member.filename=='onemessagebus/__init__.py': content=b'raise RuntimeError("broken Python SDK fixture")'+bytes([10])
   out.writestr(member,content)
 replacement.replace(wheel)
if kind=='missing-npm':
 for package in stage.glob('*.tgz'): package.unlink()
if kind=='npm-cli':
 package=next(stage.glob('*.tgz')); replacement=package.with_suffix('.changed')
 with tarfile.open(package) as source, tarfile.open(replacement,'w:gz') as out:
  for member in source.getmembers():
   content=source.extractfile(member).read() if member.isfile() else None
   if member.name=='package/bin/onemessagebus': content=b'#!/bin/sh'+bytes([10])+b'exit 1'+bytes([10]); member.size=len(content)
   out.addfile(member,io.BytesIO(content) if content is not None else None)
 replacement.replace(package)
`,
        badStage,
        badSdks,
        kind,
      ]);
      const refused = spawnSync(
        "bash",
        [
          "scripts/release-local-install.sh",
          badStage,
          badSdks,
          python,
          kind === "missing-node" ? "/missing-node" : process.execPath,
        ],
        { cwd: root, encoding: "utf8" },
      );
      assert.equal(refused.status, 1, [kind, refused.stderr].join(": "));
      assert.match(refused.stderr, /command failed/);
      assert.doesNotMatch(refused.stdout, /staged CLI and SDK installations passed/);
      if (kind === "python-sdk") assert.match(refused.stderr, /broken Python SDK fixture/);
      if (kind === "missing-npm") assert.match(refused.stderr, /ENOENT|no such file/i);
      if (kind === "npm-cli") assert.match(refused.stderr, /staged npm CLI/);
      if (kind === "missing-node")
        assert.match(refused.stderr, /missing-node.*No such file|missing-node.*not found/);
    }
    const operationalFailure = spawnSync(
      "bash",
      [
        "scripts/release-local-install.sh",
        stage,
        sdks,
        run("which", ["grep"]).trim(),
        process.execPath,
      ],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(operationalFailure.status, 1);
    assert.match(operationalFailure.stderr, /command failed/);
    const invalidProject = join(work, "invalid-project");
    mkdirSync(join(invalidProject, "scripts"), { recursive: true });
    for (const file of [
      "Cargo.toml",
      "rust-toolchain.toml",
      "README.md",
      "LICENSE",
      "CHANGELOG.md",
      "justfile",
      "scripts/release-build.sh",
      "scripts/release-stage.py",
      "scripts/release-quiet.sh",
    ])
      copyFileSync(join(root, file), join(invalidProject, file));
    const project = readFileSync(join(root, "pyproject.toml"), "utf8")
      .replace('name = "onemessagebus-cli"', 'name = "wrong-cli"')
      .replace(
        'manifest-path = "crates/onemessagebus-cli/Cargo.toml"',
        [
          "manifest-path = ",
          JSON.stringify(join(root, "crates/onemessagebus-cli/Cargo.toml")),
        ].join(""),
      );
    writeFileSync(join(invalidProject, "pyproject.toml"), project);
    const refusedProject = spawnSync(
      "bash",
      ["scripts/release-build.sh", target, join(work, "wrong-wheel")],
      { cwd: invalidProject, encoding: "utf8" },
    );
    assert.equal(refusedProject.status, 2, refusedProject.stderr);
    assert.match(refusedProject.stderr, /wheel filename is not an onemessagebus CLI wheel/);
    for (const args of [
      [],
      [stage, sdks, "/missing-python", process.execPath],
      [empty, sdks, python, process.execPath],
      [join(work, "missing-stage"), sdks, python, process.execPath],
    ]) {
      const refused = spawnSync("bash", ["scripts/release-local-install.sh", ...args], {
        cwd: root,
        encoding: "utf8",
      });
      assert.equal(refused.status, args.length ? 1 : 2);
      assert.match(refused.stderr, /expected|failed|No such|not found/);
    }
    const stageScript = join(root, "scripts/release-stage.py");
    const cases = [
      "bad-json",
      "bad-record",
      "unsafe-record",
      "bad-hash",
      "wrong-target",
      "no-npm",
      "many-npm",
      "bad-tar",
      "no-npm-binary",
      "no-wheel",
      "many-wheel",
      "bad-filename",
      "bad-platform",
      "compound-architecture",
      "compound-family",
      "old-version",
      "duplicate-script",
      "bad-metadata",
      "bad-metadata-version",
      "additional-tag",
      "bad-tag",
      "invalid-version",
      "missing-manifest",
    ];
    for (const kind of cases) {
      const directory = join(work, kind);
      cpSync(stage, directory, { recursive: true });
      const cwd = join(work, `${kind}-root`);
      mkdirSync(cwd);
      for (const file of [
        "Cargo.toml",
        "rust-toolchain.toml",
        "README.md",
        "LICENSE",
        "CHANGELOG.md",
      ])
        copyFileSync(join(root, file), join(cwd, file));
      run("python3", [
        "-c",
        `
import io,json,tarfile,zipfile,sys,re
from pathlib import Path
p=Path(sys.argv[1]); cwd=Path(sys.argv[2]); kind=sys.argv[3]
inventory=p/'binary.json'
if kind=='bad-json': inventory.write_text('{')
elif kind=='bad-record': inventory.write_text('[]')
elif kind in ['unsafe-record','bad-hash','wrong-target']:
 value=json.loads(inventory.read_text())
 if kind=='unsafe-record': value['archive']='../escape'
 if kind=='bad-hash': value['hashes']['wheel']='not-a-hash'
 if kind=='wrong-target': value['target']='aarch64-apple-darwin'
 inventory.write_text(json.dumps(value))
if kind in ['no-npm','many-npm','bad-tar','no-npm-binary']:
 package=next(p.glob('*.tgz'))
 if kind=='no-npm': package.unlink()
 if kind=='many-npm': (p/'other.tgz').write_bytes(package.read_bytes())
 if kind=='bad-tar': package.write_bytes(b'broken archive')
 if kind=='no-npm-binary':
  with tarfile.open(package,'w:gz') as out: pass
if kind in ['no-wheel','many-wheel','bad-filename','bad-platform','old-version','compound-architecture','compound-family']:
 wheel=next(p.glob('*.whl'))
 if kind=='no-wheel': wheel.unlink()
 if kind=='many-wheel': (p/'other.whl').write_bytes(wheel.read_bytes())
 if kind=='bad-filename': wheel.rename(p/'other.whl')
 if kind=='bad-platform':
  fields=wheel.name.split('-'); fields[-1]='win_'+fields[-1].split('_')[-2]+'_'+fields[-1].split('_')[-1]; wheel.rename(p/'-'.join(fields))
 if kind in ['compound-architecture','compound-family']:
  fields=wheel.name.split('-'); tag=fields[-1].removesuffix('.whl')
  wrong=tag.rsplit('_',1)[0]+'_arm64' if kind=='compound-architecture' else 'win_'+tag.rsplit('_',1)[1]
  fields[-1]=wrong+'.'+tag+'.whl'; wheel.rename(p/'-'.join(fields))
  wheel=p/'-'.join(fields); replacement=wheel.with_suffix('.changed')
  with zipfile.ZipFile(wheel) as source, zipfile.ZipFile(replacement,'w') as out:
   for member in source.infolist():
    content=source.read(member.filename)
    if member.filename.endswith('/WHEEL'): content=content.replace(b'Tag:',('Tag: py3-none-'+wrong+chr(10)+'Tag:').encode(),1)
    out.writestr(member,content)
  replacement.replace(wheel)
 if kind=='old-version':
  fields=wheel.name.split('-'); fields[1]='9999.0.0'; wheel.rename(p/'-'.join(fields))
if kind in ['duplicate-script','bad-metadata','bad-metadata-version','additional-tag','bad-tag']:
 wheel=next(p.glob('*.whl')); replacement=wheel.with_suffix('.changed')
 with zipfile.ZipFile(wheel) as source, zipfile.ZipFile(replacement,'w') as out:
  for member in source.infolist():
   content=source.read(member.filename)
   if kind=='bad-metadata' and member.filename.endswith('/METADATA'): content=content.replace(b'Name: onemessagebus-cli',b'Name: wrong-package')
   if kind=='bad-metadata-version' and member.filename.endswith('/METADATA'):
    content=re.sub(rb'(?m)^Version: .*$',b'Version: 9999.0.0',content)
   if kind=='additional-tag' and member.filename.endswith('/WHEEL'):
    content=content.replace(b'Tag:',b'Tag: py3-none-win_arm64'+bytes([10])+b'Tag:',1)
   if kind=='bad-tag' and member.filename.endswith('/WHEEL'): content=content.replace(b'Tag: py3-none-',b'Tag: py2-none-')
   out.writestr(member,content)
   if kind=='duplicate-script' and '.data/scripts/onemessagebus' in member.filename: out.writestr(member,content)
 replacement.replace(wheel)
if kind=='invalid-version': (cwd/'Cargo.toml').write_text('[workspace.package]'+chr(10)+'version="../../invalid"'+chr(10))
if kind=='missing-manifest': (cwd/'Cargo.toml').unlink()
`,
        directory,
        cwd,
        kind,
      ]);
      const verify = [
        "bad-json",
        "bad-record",
        "unsafe-record",
        "bad-hash",
        "wrong-target",
        "no-npm",
        "many-npm",
        "bad-tar",
        "no-npm-binary",
      ].includes(kind);
      const refused = spawnSync(
        python,
        [
          stageScript,
          "--target",
          target,
          "--directory",
          directory,
          ...(verify ? ["--verify-npm"] : []),
        ],
        { cwd, encoding: "utf8" },
      );
      if (kind.startsWith("compound-"))
        assert.match(refused.stderr, /platform\/architecture|platform family/);
      assert.equal(
        refused.status,
        kind === "missing-manifest" ? 1 : 2,
        `${kind}: ${refused.stderr}`,
      );
      assert.match(refused.stderr, /ACTION: rebuild/);
    }
    const broken = join(work, "broken-install");
    cpSync(stage, broken, { recursive: true });
    run("python3", [
      "-c",
      `
import zipfile,sys
from pathlib import Path
p=next(Path(sys.argv[1]).glob('*.whl')); replacement=p.with_suffix('.changed')
with zipfile.ZipFile(p) as source, zipfile.ZipFile(replacement,'w') as out:
 for member in source.infolist():
  content=source.read(member.filename)
  if '.data/scripts/onemessagebus' in member.filename: content=b'broken executable'
  out.writestr(member,content)
replacement.replace(p)
`,
      broken,
    ]);
    const failedSmoke = spawnSync(
      "bash",
      ["scripts/release-local-install.sh", broken, sdks, python, process.execPath],
      { cwd: root, encoding: "utf8" },
    );
    assert.notEqual(failedSmoke.status, 0);
    assert.match(failedSmoke.stderr, /failed|not found|error|cannot/i);
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
