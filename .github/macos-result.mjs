#!/usr/bin/env node
// GitHub's job outcomes are the boundary: omission is valid only by policy.
const [required, changes, gate, proof] = process.argv.slice(2);
if (
  !["true", "false"].includes(required) ||
  changes !== "success" ||
  gate !== "success" ||
  (required === "true" ? proof !== "success" : proof !== "skipped")
) {
  console.error(
    `Mac proof refused: required=${required}, changes=${changes}, gate=${gate}, proof=${proof}; inspect the prerequisite and Mac job logs`,
  );
  process.exitCode = 1;
} else
  console.log(
    required === "true"
      ? "Mac assertions passed"
      : "Mac assertions omitted by semantic release-only policy",
  );
