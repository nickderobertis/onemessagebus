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
  const count = (value) => Number.isInteger(value) && value >= 0;
  if (
    !count(telemetry.jobsPerRun) ||
    !count(telemetry.runsPerChange) ||
    !Array.isArray(telemetry.runs) ||
    !telemetry.runs.length ||
    !Array.isArray(telemetry.lifecycles) ||
    !telemetry.lifecycles.length
  )
    throw new Error("invalid telemetry counts or runs; regenerate github-workflows:test");
  const validMacJob = (job) =>
    job &&
    Array.isArray(job.runner) &&
    job.runner.some((label) => typeof label === "string" && label.startsWith("macos-"));
  for (const run of telemetry.runs)
    if (!run || !Array.isArray(run.jobs) || run.jobs.some((job) => !validMacJob(job)))
      throw new Error("invalid Mac job rows; regenerate github-workflows:test");
  for (const lifecycle of telemetry.lifecycles) {
    if (
      !lifecycle ||
      !Array.isArray(lifecycle.runs) ||
      lifecycle.runs.some(
        (run) =>
          !run || !Array.isArray(run.macJobs) || run.macJobs.some((job) => !validMacJob(job)),
      ) ||
      !count(lifecycle.macSpendingRuns) ||
      lifecycle.macSpendingRuns !== lifecycle.runs.filter((run) => run.macJobs.length).length
    )
      throw new Error("inconsistent lifecycle count; regenerate github-workflows:test");
  }
  if (
    telemetry.jobsPerRun !== Math.max(...telemetry.runs.map((run) => run.jobs.length)) ||
    telemetry.runsPerChange !==
      Math.max(...telemetry.lifecycles.map((lifecycle) => lifecycle.macSpendingRuns))
  )
    throw new Error("inconsistent telemetry measurements; regenerate github-workflows:test");
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
