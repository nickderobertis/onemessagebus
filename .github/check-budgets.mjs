import { check } from "@onebudgetspec/sdk";
import { root } from "./workflow-graph.mjs";
try {
  const result = await check({
    paths: process.argv.slice(2).length ? process.argv.slice(2) : [".github/budgets.yaml"],
    cwd: root,
  });
  for (const budget of result.results) {
    console.log(`${budget.id}: ${budget.verdict} (${budget.actual} ${budget.unit})`);
    if (budget.verdict !== "within") {
      console.error(budget.error ?? budget.detail);
      process.exitCode = Math.max(process.exitCode ?? 0, budget.verdict === "error" ? 2 : 1);
    }
  }
} catch (error) {
  console.error(
    `check-budgets: ${error.message}; repair the budgets file or measurement input and rerun`,
  );
  process.exitCode = 2;
}
