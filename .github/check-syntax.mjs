import { execFileSync } from "node:child_process";
import { readdirSync } from "node:fs";
for (const directory of [".github", ".github/tests"])
  for (const file of readdirSync(directory))
    if (file.endsWith(".mjs"))
      execFileSync(process.execPath, ["--check", `${directory}/${file}`], { stdio: "inherit" });
