#!/usr/bin/env node
// Launcher for the `onemessagebus` command installed from the
// `onemessagebus-cli` npm package.
//
// Like the PyPI wheels (maturin `bindings = "bin"`, see pyproject.toml), the npm
// distribution carries the *prebuilt* Rust binary — no Rust toolchain, no
// compile, no download at install time. The platform-specific binary ships
// inside a per-platform package (`onemessagebus-cli-<platform>-<arch>`) the
// published package lists in its `optionalDependencies`; npm installs only the
// one whose `os`/`cpu` match the host, and this shim resolves it and execs it
// with the caller's argv.
//
// This file is committed source; the version and the optionalDependencies are
// written at publish time by scripts/npm-build.mjs, from Cargo.toml and its
// TARGETS table, and that script also generates the per-platform packages from
// the release binaries.

const path = require("node:path");
const { spawnSync } = require("node:child_process");

// process.platform-process.arch -> the platform package that carries the binary.
// The keys mirror the Rust target matrix in .github/workflows/release.yml and
// the TARGETS table in scripts/npm-build.mjs, which generates the published
// optionalDependencies; npm/test/platform-matrix.test.mjs holds them in lockstep.
const PACKAGES = {
  "linux-x64": "onemessagebus-cli-linux-x64",
  "linux-arm64": "onemessagebus-cli-linux-arm64",
  "darwin-arm64": "onemessagebus-cli-darwin-arm64",
  "win32-x64": "onemessagebus-cli-win32-x64",
};

// Every failure here is a failed install, so say what to do about it rather than
// only what went wrong.
const OTHER_INSTALLS =
  "Install another way instead: 'pip install onemessagebus-cli', or " +
  "'cargo install --git https://github.com/nickderobertis/onemessagebus onemessagebus-cli --locked'.";

function fail(message) {
  process.stderr.write(`onemessagebus: ${message}\n`);
  process.exit(1);
}

// Platforms a release once carried and no longer builds: refused by name, so an
// install there says it is unsupported rather than reporting a missing package.
const DISCONTINUED = {
  "darwin-x64":
    "Intel macOS (darwin-x64) is no longer supported: releases ship prebuilt " +
    "binaries for Apple Silicon macOS (darwin-arm64) only. Build from source " +
    "instead: 'cargo install --git https://github.com/nickderobertis/onemessagebus onemessagebus-cli --locked'.",
};

/// What the launcher does on `platform`/`arch`: the platform package carrying the
/// binary, or the refusal to print. They are arguments rather than `process` read
/// here, so npm/test/launcher-platform.test.mjs holds every answer on any host.
function platformPackage(platform, arch) {
  const key = `${platform}-${arch}`;
  if (DISCONTINUED[key]) return { refusal: DISCONTINUED[key] };
  if (!PACKAGES[key]) {
    return {
      refusal:
        `unsupported platform ${key}. Prebuilt binaries exist for: ` +
        `${Object.keys(PACKAGES).join(", ")}. ${OTHER_INSTALLS}`,
    };
  }
  return { package: PACKAGES[key] };
}

function binaryPath() {
  const { package: pkg, refusal } = platformPackage(process.platform, process.arch);
  if (refusal) fail(refusal);

  const binName = process.platform === "win32" ? "onemessagebus.exe" : "onemessagebus";
  try {
    // Resolve the platform package's manifest, then locate the binary beside it.
    // Resolving package.json (rather than the binary file directly) is portable
    // across Node resolution modes and does not require an `exports` entry for a
    // non-JS asset.
    const manifest = require.resolve(`${pkg}/package.json`);
    return path.join(path.dirname(manifest), "bin", binName);
  } catch (_err) {
    return fail(
      `the platform package ${pkg} is not installed. This usually means npm ` +
        "skipped optional dependencies (e.g. --no-optional / --omit=optional) " +
        "or the install was for a different platform. Reinstall with optional " +
        `dependencies enabled. ${OTHER_INSTALLS}`,
    );
  }
}

module.exports = { platformPackage };

// Run only as the installed command; a test that requires this file for
// platformPackage launches nothing.
if (require.main === module) launch();

function launch() {
  const result = spawnSync(binaryPath(), process.argv.slice(2), { stdio: "inherit" });

  if (result.error) {
    fail(`failed to launch the onemessagebus binary: ${result.error.message}`);
  }

  // Re-raise a terminating signal so callers observe the true cause; otherwise
  // propagate the child's exit code verbatim — the contract assigns meaning to 1
  // and 2, and a caller reads them.
  if (result.signal) {
    process.kill(process.pid, result.signal);
  }
  process.exit(result.status === null ? 1 : result.status);
}
