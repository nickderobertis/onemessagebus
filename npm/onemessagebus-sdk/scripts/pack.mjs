#!/usr/bin/env node
// Writes the publishable copy of @onemessagebus/sdk: the built `dist`, the README,
// and a manifest stamped with the workspace version — its own version, and an
// exact `onemessagebus-cli` dependency — with SDK_VERSION and CLI_VERSION stamped
// in the built `dist/version.js`. Cargo.toml's `[workspace.package]` is the one
// version source; nothing here invents one.
//
// It refuses (exit 1) rather than guess when a placeholder it stamps has moved:
// a stamp that silently missed would publish an SDK pinned to nothing.
//
// Usage: node scripts/pack.mjs [--from DIR] [--out DIR] [--version V]
//   --from     the package directory holding package.json, README.md and dist/ (this package)
//   --out      where the publishable copy goes (dist-pack/ in this package)
//   --version  the version to stamp (Cargo.toml's workspace version)
// Prints the directory written.
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PACKAGE = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ROOT = resolve(PACKAGE, "../..");
const PLACEHOLDER = "0.0.0-dev";

function fail(message, action) {
  process.stderr.write(`pack: ${message}\n  fix: ${action}\n`);
  process.exit(1);
}

function argument(name) {
  const at = process.argv.indexOf(name);
  if (at === -1) return undefined;
  const value = process.argv[at + 1];
  if (value === undefined || value.startsWith("--")) {
    fail(`${name} takes a value`, `pass ${name} <value>`);
  }
  return value;
}

function workspaceVersion() {
  const manifest = join(ROOT, "Cargo.toml");
  const text = existsSync(manifest) ? readFileSync(manifest, "utf8") : "";
  const section = /^\[workspace\.package\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/mu.exec(text)?.[1];
  const version = section && /^version\s*=\s*"([^"]+)"/mu.exec(section)?.[1];
  if (!version) {
    fail(
      `${manifest} declares no [workspace.package] version`,
      "restore the root Cargo.toml, or pass --version",
    );
  }
  return version;
}

const from = resolve(argument("--from") ?? PACKAGE);
const out = resolve(argument("--out") ?? join(PACKAGE, "dist-pack"));
const version = argument("--version") ?? workspaceVersion();
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u.test(version) || version === PLACEHOLDER) {
  fail(`${version} is not a version to publish`, "stamp a real semver version");
}

const built = join(from, "dist/version.js");
if (!existsSync(built)) {
  fail(
    `${built} does not exist`,
    "build first: `just node-sdk-dist OUT` builds and packs it in one step, or `just nx run onemessagebus-node-sdk:build` builds dist/ alone",
  );
}
const manifest = JSON.parse(readFileSync(join(from, "package.json"), "utf8"));
if (manifest.version !== PLACEHOLDER) {
  fail(
    `package.json's version is ${JSON.stringify(manifest.version)}, not the placeholder ${PLACEHOLDER} this packer stamps`,
    `set it back to ${PLACEHOLDER}; the version comes from Cargo.toml`,
  );
}
let versionModule = readFileSync(built, "utf8");
for (const name of ["SDK_VERSION", "CLI_VERSION"]) {
  const literal = new RegExp(
    `^export const ${name} = "${PLACEHOLDER.replaceAll(".", "\\.")}";$`,
    "gmu",
  );
  const found = versionModule.match(literal)?.length ?? 0;
  if (found !== 1) {
    fail(
      `${built} holds ${found} \`export const ${name} = "${PLACEHOLDER}";\` where the packer stamps exactly one`,
      `keep ${name} as that literal in src/version.ts, rebuild, and pack again`,
    );
  }
  versionModule = versionModule.replace(literal, `export const ${name} = "${version}";`);
}

manifest.version = version;
manifest.dependencies = { ...manifest.dependencies, "onemessagebus-cli": version };
delete manifest.scripts;
delete manifest.devDependencies;

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
cpSync(join(from, "dist"), join(out, "dist"), { recursive: true });
cpSync(join(from, "README.md"), join(out, "README.md"));
writeFileSync(join(out, "dist/version.js"), versionModule);
writeFileSync(join(out, "package.json"), `${JSON.stringify(manifest, null, 2)}\n`);
process.stdout.write(`${out}\n`);
