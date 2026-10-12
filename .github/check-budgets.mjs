import { check } from "@onebudgetspec/sdk";
import { root } from "./workflow-graph.mjs";
const result = await check({
  paths: process.argv.slice(2).length ? process.argv.slice(2) : [".github/budgets.yaml"],
  cwd: root,
});
for (const budget of result.results) {
  console.log(`${budget.id}: ${budget.verdict} (${budget.actual} ${budget.unit})`);
  if (budget.verdict !== "within") {
    console.error(budget.error ?? budget.detail);
    process.exitCode = 1;
  }
}
