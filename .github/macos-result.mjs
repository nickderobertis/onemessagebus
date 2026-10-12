#!/usr/bin/env node
// GitHub's job outcomes are the boundary: omission is valid only by policy.
const args = process.argv.slice(2);
if (
  args.length !== 4 ||
  !["true", "false"].includes(args[0]) ||
  args.slice(1).some((value) => !["success", "failure", "cancelled", "skipped", ""].includes(value))
) {
  console.error(
    "macos-result: use REQUIRED(true|false) CHANGES GATE PROOF; job outcomes may be empty when missing",
  );
  process.exit(2);
}
const [required, changes, gate, proof] = args;
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
