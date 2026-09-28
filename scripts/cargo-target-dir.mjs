#!/usr/bin/env node
// Where Cargo writes: its effective target directory, read from `cargo metadata`'s
// `target_directory`, which honours CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR and
// `.cargo/config.toml` alike. Anything that goes looking for a built binary asks
// here rather than assuming `target/`, because a host sharing one build cache
// across checkouts exports a directory outside the clone.
//
// A recipe resolves it once and hands it to what it runs as
// ONEMESSAGEBUS_TARGET_DIR; `cargoTargetDir`, and this script run as
// `node scripts/cargo-target-dir.mjs`, take that hand-off when it is set and
// resolve the directory from Cargo when a suite is run directly. So the recipe
// runs it with the hand-off unset: its own resolution is the one it hands on.
// tests/conftest.py in the Python SDK restates the same two steps, and
// python/onemessagebus-sdk/tests/test_support.py holds the names together.

import { execFileSync } from "node:child_process";
import { realpathSync } from "node:fs";
import { isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";

export const HANDOFF = "ONEMESSAGEBUS_TARGET_DIR";

/**
 * A target directory that could not be resolved; its message names why, and its
 * `status` is the exit code the script reports it with: 2 for a hand-off it
 * refused, 1 for a cargo that could not answer.
 */
export class TargetDirError extends Error {
  constructor(message, status = 1) {
    super(message);
    this.status = status;
  }
}

function absolute(value, source, status) {
  if (typeof value !== "string" || !isAbsolute(value)) {
    throw new TargetDirError(
      `${source} named ${JSON.stringify(value)} as Cargo's target directory, not an absolute path`,
      status,
    );
  }
  return value;
}

/** Cargo's own answer for a cargo run in `cwd` under `env`. */
export function resolveCargoTargetDir({ cwd, env = process.env }) {
  let named;
  try {
    const metadata = execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
      cwd,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    named = JSON.parse(metadata)?.target_directory;
  } catch (error) {
    throw new TargetDirError(
      `\`cargo metadata\` did not name Cargo's target directory:\n${error.stderr || error.message}`,
    );
  }
  return absolute(named, "`cargo metadata`", 1);
}

/** The directory a recipe handed on in ONEMESSAGEBUS_TARGET_DIR, else Cargo's own. */
export function cargoTargetDir({ cwd, env = process.env }) {
  const handed = env[HANDOFF];
  return handed === undefined ? resolveCargoTargetDir({ cwd, env }) : absolute(handed, HANDOFF, 2);
}

const invokedAs = process.argv[1];
if (invokedAs && realpathSync(invokedAs) === realpathSync(fileURLToPath(import.meta.url))) {
  try {
    process.stdout.write(cargoTargetDir({ cwd: process.cwd() }));
  } catch (error) {
    if (!(error instanceof TargetDirError)) throw error;
    process.stderr.write(
      `cargo-target-dir: ${error.message}\n  fix: put cargo on PATH and fix what it refused, or unset ${HANDOFF}, then rerun\n`,
    );
    process.exit(error.status);
  }
}
