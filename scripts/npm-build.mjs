#!/usr/bin/env node
// Build the npm packages that distribute the prebuilt onemessagebus binary — the
// direct analogue of the maturin PyPI wheels (see pyproject.toml). The layout
// mirrors esbuild/@biomejs and every other "carry the native binary" npm tool:
//
//   onemessagebus-cli                 launcher (npm/onemessagebus-cli, committed)
//     bin/onemessagebus.js            resolves + execs the platform binary
//     optionalDependencies:           one per TARGETS entry, generated here
//       onemessagebus-cli-linux-x64
//       onemessagebus-cli-linux-arm64
//       onemessagebus-cli-darwin-x64
//       onemessagebus-cli-darwin-arm64
//       onemessagebus-cli-win32-x64   each carries the matching prebuilt binary
//
// The platform packages are UNSCOPED on purpose: a `@scope/` name needs an npm
// organization, which a publish token cannot create.
//
// npm installs only the optional dependency whose `os`/`cpu` match the host, so
// `npm install -g onemessagebus-cli` is a seconds-fast binary install — the same
// promise the wheels make on PyPI.
//
// The version is sourced from Cargo.toml (release-plz stays the single version
// driver, exactly like the wheels' `dynamic = ["version"]`); pass --version to
// override. Nothing here publishes — it only assembles package directories under
// --out; release.yml packs and publishes them.
//
// Usage:
//   node scripts/npm-build.mjs platform --target <triple> --binary <path> \
//        [--version <v>] [--out <dir>]
//   node scripts/npm-build.mjs launcher [--version <v>] [--out <dir>]
//
// Both modes print the created package directory on stdout. Exit status follows
// the repository's contract: 0 assembled, 1 a step that could not be completed
// (a filesystem failure, or a Cargo.toml whose version is unreadable), 2 an
// invocation refused before anything was touched (an unknown mode, option or
// target, a missing value, a binary that does not exist, a malformed --version).

import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// Rust target triple -> npm platform package facts. Keys must match the release
// matrix in .github/workflows/release.yml; the (platform, arch) pair must match
// the PACKAGES map in npm/onemessagebus-cli/bin/onemessagebus.js. The launcher's
// optionalDependencies are generated from this table when it is assembled.
const TARGETS = {
  "x86_64-unknown-linux-gnu": { platform: "linux", arch: "x64", exe: false },
  "aarch64-unknown-linux-gnu": { platform: "linux", arch: "arm64", exe: false },
  "x86_64-apple-darwin": { platform: "darwin", arch: "x64", exe: false },
  "aarch64-apple-darwin": { platform: "darwin", arch: "arm64", exe: false },
  "x86_64-pc-windows-msvc": { platform: "win32", arch: "x64", exe: true },
};

const REPOSITORY = "https://github.com/nickderobertis/onemessagebus";

// Every failure names what to do next: this runs inside a release job, where the
// only diagnosis anyone gets is what it printed.
function die(msg, action, status = 1) {
  process.stderr.write(`npm-build: ${msg}\nACTION: ${action}\n`);
  process.exit(status);
}

// An invocation this script will not carry out, refused before it touches
// anything — distinct from a step that failed, so a caller can tell a typo in the
// release job from a runner that ran out of disk.
function refuse(msg, action) {
  return die(msg, action, 2);
}

// Run a filesystem step, turning anything it throws into the script's own
// diagnostic. Node's raw `ENOENT: no such file or directory, open '...'` names
// the syscall and not the fix.
function attempt(what, action, step) {
  try {
    return step();
  } catch (error) {
    return die(`${what}: ${error.message}`, action);
  }
}

// The version both registries index this release under. npm rejects anything
// that is not semver, and a version with a stray specifier would publish under a
// name no consumer could ask for — so it is validated here rather than at the
// registry, whichever source it came from. The shape is SemVer 2.0.0's: numbers
// without leading zeros, then at most one `-prerelease` and one `+build`, in that
// order, each a dot-separated run of non-empty identifiers, and no leading zero on
// a numeric prerelease identifier — so `1.2.3-.`, `1.2.3-a..b`, `1.2.3-01` and
// `1.2.3+one+two` are refused here rather than by the registry mid-publish.
const NUMBER = "(?:0|[1-9]\\d*)";
const PRERELEASE_ID = "(?:0|[1-9]\\d*|\\d*[A-Za-z-][0-9A-Za-z-]*)";
const BUILD_ID = "[0-9A-Za-z-]+";
// Exported so npm/test/version-grammar.test.mjs can hold it to the same grammar
// scripts/release-probe.sh reads a registry's answer with.
export const VERSION = new RegExp(
  `^${NUMBER}\\.${NUMBER}\\.${NUMBER}` +
    `(?:-${PRERELEASE_ID}(?:\\.${PRERELEASE_ID})*)?` +
    `(?:\\+${BUILD_ID}(?:\\.${BUILD_ID})*)?$`,
);

// Read the workspace version from the root Cargo.toml [workspace.package]
// section — the one version every crate inherits. A tiny hand parser avoids a
// TOML dependency: take the first `version = "..."` after the header and before
// the next section, so a dependency's version can never be mistaken for it.
function cargoVersion() {
  const toml = attempt(
    "cannot read Cargo.toml",
    "run this from a checkout of the repository, where Cargo.toml is readable",
    () => readFileSync(join(REPO_ROOT, "Cargo.toml"), "utf8"),
  );
  // The header on its own line: a comment above it names the section too.
  const pkg = toml.search(/^\[workspace\.package\]$/m);
  if (pkg === -1) {
    die("no [workspace.package] section in Cargo.toml", "run this from the repository root");
  }
  const rest = toml.slice(pkg);
  const end = rest.indexOf("\n[", 1);
  const section = end === -1 ? rest : rest.slice(0, end);
  const found = section.match(/^\s*version\s*=\s*"([^"]+)"/m);
  if (!found) {
    die(
      "could not parse version from Cargo.toml [workspace.package]",
      'restore the `version = "X.Y.Z"` line release-plz maintains there',
    );
  }
  return found[1];
}

// The release version: Cargo.toml's unless --version overrides it, validated
// either way before it reaches a manifest.
function resolveVersion(args) {
  const version = args.version ?? cargoVersion();
  if (!VERSION.test(version)) {
    const message = `'${version}' is not a version either registry can index`;
    // A malformed --version is the caller's input; a malformed Cargo.toml is the
    // repository's state, which the invocation itself did nothing wrong to reach.
    if (args.version === undefined) {
      die(message, "fix the `version` in Cargo.toml [workspace.package]; it must read X.Y.Z");
    }
    refuse(
      message,
      "pass --version X.Y.Z (a -prerelease or +build suffix is allowed), or omit it to take Cargo.toml's",
    );
  }
  return version;
}

// Options are allowlisted per mode: an unrecognized flag is a caller that meant
// something this script will not do, and silently ignoring it would assemble a
// package that is not the one they asked for.
function parseArgs(argv, allowed) {
  const out = {};
  const usage = allowed.map((name) => `--${name}`).join(", ");
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith("--")) refuse(`unexpected argument: ${arg}`, `pass options as ${usage}`);
    const key = arg.slice(2);
    if (!allowed.includes(key)) refuse(`unknown option --${key}`, `this mode takes ${usage}`);
    const value = argv[i + 1];
    if (value === undefined || value.startsWith("--")) {
      refuse(`--${key} needs a value`, `give --${key} a value`);
    }
    out[key] = value;
    i += 1;
  }
  return out;
}

function writeJson(path, obj) {
  attempt(`cannot write ${path}`, "check that its directory is writable", () =>
    writeFileSync(path, `${JSON.stringify(obj, null, 2)}\n`),
  );
}

function buildPlatform(args) {
  const known = Object.keys(TARGETS).join(", ");
  const target =
    args.target ||
    refuse("platform: --target <triple> is required", `pass --target with one of: ${known}`);
  const binary =
    args.binary ||
    refuse(
      "platform: --binary <path> is required",
      "pass --binary the path to the built onemessagebus for that target",
    );
  const facts =
    TARGETS[target] || refuse(`platform: unknown target ${target}`, `pass one of: ${known}`);
  const version = resolveVersion(args);
  const outRoot = resolve(args.out || join(REPO_ROOT, "npm", "dist"));

  const pkgName = `onemessagebus-cli-${facts.platform}-${facts.arch}`;
  const pkgDir = join(outRoot, pkgName);
  const binDir = join(pkgDir, "bin");
  const binName = facts.exe ? "onemessagebus.exe" : "onemessagebus";

  // Resolve the source binary with a `.exe` fallback: a bash caller may pass the
  // extensionless path (Git Bash's `test -x` matches onemessagebus.exe
  // transparently, but Node's copyFileSync needs the real name).
  let srcBin = resolve(binary);
  if (!existsSync(srcBin) && existsSync(`${srcBin}.exe`)) srcBin = `${srcBin}.exe`;
  if (!existsSync(srcBin)) {
    refuse(
      `platform: binary not found: ${binary}`,
      `build it first: cargo build --release --locked -p onemessagebus-cli --target ${target}`,
    );
  }

  attempt(
    `cannot assemble ${pkgName} under ${outRoot}`,
    "check that --out names a writable directory with room for the binary",
    () => {
      rmSync(pkgDir, { recursive: true, force: true });
      mkdirSync(binDir, { recursive: true });
      copyFileSync(srcBin, join(binDir, binName));
      if (!facts.exe) chmodSync(join(binDir, binName), 0o755);
    },
  );

  writeJson(join(pkgDir, "package.json"), {
    name: pkgName,
    version,
    description: `Prebuilt onemessagebus binary for ${facts.platform} ${facts.arch}.`,
    homepage: REPOSITORY,
    license: "MIT",
    author: "Nick DeRobertis",
    repository: { type: "git", url: `git+${REPOSITORY}.git` },
    // os/cpu make npm install this package only on the matching host, so the
    // launcher's optionalDependency resolution picks exactly one.
    os: [facts.platform],
    cpu: [facts.arch],
    files: [`bin/${binName}`],
  });

  attempt(`cannot write the ${pkgName} README`, "check that --out is writable", () =>
    writeFileSync(
      join(pkgDir, "README.md"),
      `# ${pkgName}\n\nPrebuilt \`onemessagebus\` binary for ${facts.platform} ${facts.arch}.\n` +
        "This is a platform-specific dependency of " +
        "[`onemessagebus-cli`](https://www.npmjs.com/package/onemessagebus-cli); " +
        "install that instead.\n",
    ),
  );

  process.stdout.write(`${pkgDir}\n`);
}

function buildLauncher(args) {
  const version = resolveVersion(args);
  const outRoot = resolve(args.out || join(REPO_ROOT, "npm", "dist"));
  const src = join(REPO_ROOT, "npm", "onemessagebus-cli");
  const dest = join(outRoot, "onemessagebus-cli");

  attempt(
    `cannot copy the committed launcher from ${src}`,
    "restore npm/onemessagebus-cli from git, and check that --out is writable",
    () => {
      rmSync(dest, { recursive: true, force: true });
      mkdirSync(outRoot, { recursive: true });
      cpSync(src, dest, { recursive: true });
    },
  );

  // Stamp the real version, and pin exactly the platform packages this release
  // publishes, from TARGETS. The committed manifest carries neither: a real
  // version there would be a second version source, and pins there would name
  // packages npm cannot resolve until a release publishes them — which is what
  // lets the launcher be a member of the repository's npm workspace.
  const manifestPath = join(dest, "package.json");
  const manifest = attempt(
    "the committed launcher manifest is missing or is not JSON",
    "restore npm/onemessagebus-cli/package.json from git",
    () => JSON.parse(readFileSync(manifestPath, "utf8")),
  );
  manifest.version = version;
  manifest.optionalDependencies = Object.fromEntries(
    Object.values(TARGETS).map(({ platform, arch }) => [
      `onemessagebus-cli-${platform}-${arch}`,
      version,
    ]),
  );
  writeJson(manifestPath, manifest);

  process.stdout.write(`${dest}\n`);
}

// Dispatched only when run as a script, so importing it for VERSION assembles
// nothing and exits nothing.
const invokedAs = process.argv[1];
if (invokedAs && realpathSync(invokedAs) === realpathSync(fileURLToPath(import.meta.url))) {
  const [mode, ...rest] = process.argv.slice(2);
  if (mode === "platform") {
    buildPlatform(parseArgs(rest, ["target", "binary", "version", "out"]));
  } else if (mode === "launcher") {
    buildLauncher(parseArgs(rest, ["version", "out"]));
  } else {
    refuse(
      `unknown mode ${mode === undefined ? "(none given)" : mode}`,
      "run `npm-build.mjs platform --target <triple> --binary <path> [--version <v>] [--out <dir>]` or `npm-build.mjs launcher [--version <v>] [--out <dir>]`",
    );
  }
}
