#!/usr/bin/env node
// Consume recorded telemetry; measuring and judging remain separate operations.
import { readFileSync } from "node:fs";
import { report } from "@onebudgetspec/sdk";
import { sourceDigest } from "./workflow-graph.mjs";
try {
  const args = process.argv.slice(2);
  if (
    args.length !== 4 ||
    args[0] !== "--metric" ||
    args[2] !== "--telemetry" ||
    !["jobs-per-run", "runs-per-change"].includes(args[1])
  )
    throw new Error("use --metric jobs-per-run|runs-per-change --telemetry PATH");
  const telemetry = JSON.parse(readFileSync(args[3], "utf8"));
  if (telemetry.schemaVersion !== 1 || telemetry.sourceDigest !== sourceDigest())
    throw new Error("missing or stale telemetry; run github-workflows:test against this tree");
  const value = args[1] === "jobs-per-run" ? telemetry.jobsPerRun : telemetry.runsPerChange;
  const detail = JSON.stringify({
    metric: args[1],
    runs: telemetry.runs.filter((r) => r.jobs.length),
    lifecycles: telemetry.lifecycles,
  });
  report(value, detail);
  console.log(`${args[1]}: ${value}`);
} catch (error) {
  console.error(`macos-budget: ${error.message}`);
  process.exitCode = 2;
}
