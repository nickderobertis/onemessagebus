// `scripts/preserved-log.sh`, sourced into a real bash the way `scripts/nx`
// sources it, over a real directory.
//
// What this library promises is about files a person opens after a run: that the
// log is where the message said, readable only by its owner, not erased by a
// nested run that is still going, and free of the credential values the run's
// environment held. So each case asserts the file on disk, not the function's say-so.
// `npm/test/nx.test.mjs` then drives the same promises through the wrapper.

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const LIBRARY = join(REPO_ROOT, "scripts", "preserved-log.sh");

/// Run `script` in a strict bash that has sourced the library, with an environment
/// of exactly PATH, HOME and `env` — so the credentials in it are the ones a case
/// put there, and an enclosing `scripts/nx`'s claim list is not inherited.
function sourced(script, { env = {}, input } = {}) {
  const result = spawnSync(
    "bash",
    ["-c", `set -euo pipefail\n. "$PRESERVED_LOG_LIBRARY"\n${script}`],
    {
      encoding: "utf8",
      input,
      env: {
        PATH: process.env.PATH,
        HOME: process.env.HOME,
        PRESERVED_LOG_LIBRARY: LIBRARY,
        ...env,
      },
      timeout: 30_000,
    },
  );
  assert.equal(result.error, undefined, `bash could not be run: ${result.error}`);
  return result;
}

function mode(path) {
  return statSync(path).mode & 0o777;
}

describe("scripts/preserved-log.sh", {
  skip: process.platform === "win32" && "asserts POSIX permissions",
}, () => {
  let scratch;
  let roots = 0;

  before(() => {
    scratch = mkdtempSync(join(tmpdir(), "preserved-log-"));
  });
  after(() => {
    rmSync(scratch, { recursive: true, force: true });
  });

  function freshRoot() {
    roots += 1;
    const root = join(scratch, `root-${roots}`);
    mkdirSync(root);
    return root;
  }

  it("opens an owner-only log at the stable path, truncating the last run and its diverted leftovers", () => {
    const root = freshRoot();
    const logs = join(root, ".logs");
    mkdirSync(logs, { mode: 0o755 });
    writeFileSync(join(logs, "nx.log"), "the previous run's output\n", { mode: 0o644 });
    writeFileSync(join(logs, "nx.4242.log"), "a nested run of the previous one\n");
    writeFileSync(join(logs, "other.log"), "another label's log\n");

    const result = sourced(
      'preserved_log_open "$ROOT_DIR" nx\nprintf \'%s\\n\' "$PRESERVED_LOG"\necho fresh >"$PRESERVED_LOG"',
      {
        env: { ROOT_DIR: root },
      },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, "");
    const log = join(realpathSync(root), ".logs", "nx.log");
    assert.equal(
      result.stdout,
      `${log}\n`,
      "PRESERVED_LOG is not the absolute, canonical log path",
    );
    assert.equal(
      readFileSync(log, "utf8"),
      "fresh\n",
      "the previous run's output was not truncated",
    );
    assert.equal(mode(log), 0o600, "the log is readable by someone other than its owner");
    assert.equal(mode(logs), 0o700, "the log directory is open to someone other than its owner");
    assert.equal(
      existsSync(join(logs, "nx.4242.log")),
      false,
      "a diverted leftover survived a top-level run",
    );
    assert.equal(readFileSync(join(logs, "other.log"), "utf8"), "another label's log\n");
  });

  it("diverts a nested invocation rather than truncating the log its parent is still writing", () => {
    const root = freshRoot();
    // The nested run spells the same root through a symlink, as a caller that
    // resolved it differently would: the claim has to recognise the same file.
    const alias = join(scratch, `alias-${roots}`);
    symlinkSync(root, alias);
    const result = sourced(
      [
        'preserved_log_open "$ROOT_DIR" nx',
        'echo "outer evidence, still being written" >"$PRESERVED_LOG"',
        'printf "outer=%s\\n" "$PRESERVED_LOG"',
        // A child process, not a function call: the claim is inherited through the
        // environment, the way the e2e journeys spawning scripts/nx inherit it.
        'bash -c \'set -euo pipefail; . "$PRESERVED_LOG_LIBRARY"; preserved_log_open "$1" nx; echo inner >"$PRESERVED_LOG"; printf "inner=%s\\n" "$PRESERVED_LOG"\' nested "$ALIAS_DIR"',
      ].join("\n"),
      { env: { ROOT_DIR: root, ALIAS_DIR: alias } },
    );
    assert.equal(result.status, 0, result.stderr);
    const outer = result.stdout.match(/^outer=(.*)$/m)[1];
    const inner = result.stdout.match(/^inner=(.*)$/m)[1];
    assert.equal(outer, join(realpathSync(root), ".logs", "nx.log"));
    assert.match(
      inner,
      /\/\.logs\/nx\.\d+\.log$/,
      "the nested run was not diverted to a distinct log",
    );
    assert.equal(
      readFileSync(outer, "utf8"),
      "outer evidence, still being written\n",
      "the nested run truncated its parent's log",
    );
    assert.equal(readFileSync(inner, "utf8"), "inner\n");
    assert.equal(mode(inner), 0o600);
  });

  it("refuses a label that is not a plain lowercase word, before touching the directory", () => {
    const root = freshRoot();
    for (const label of ["../escape", "Nx", "", "two words", "-dash"]) {
      const result = sourced('preserved_log_open "$ROOT_DIR" "$LABEL" || echo "refused=$?"', {
        env: { ROOT_DIR: root, LABEL: label },
      });
      assert.equal(result.stdout, "refused=1\n", `the label '${label}' was accepted`);
      assert.match(
        result.stderr,
        /preserved-log: invalid log label '.*'; use lowercase words and dashes/,
      );
    }
    assert.equal(
      existsSync(join(root, ".logs")),
      false,
      "a refused label still created the log directory",
    );
  });

  it("names the directory it could not prepare", () => {
    const root = join(scratch, "a-file-not-a-directory");
    writeFileSync(root, "");
    const result = sourced('preserved_log_open "$ROOT_DIR" nx || echo "refused=$?"', {
      env: { ROOT_DIR: root },
    });
    assert.equal(result.stdout, "refused=1\n");
    assert.match(
      result.stderr,
      /preserved-log: cannot prepare '.*a-file-not-a-directory\/\.logs'; repair its parent permissions and retry/,
    );
  });

  it("names the log it could not open", () => {
    const root = freshRoot();
    mkdirSync(join(root, ".logs", "nx.log"), { recursive: true });
    const result = sourced('preserved_log_open "$ROOT_DIR" nx || echo "refused=$?"', {
      env: { ROOT_DIR: root },
    });
    assert.equal(result.stdout, "refused=1\n");
    assert.match(
      result.stderr,
      /preserved-log: cannot open '.*\/\.logs\/nx\.log'; repair its permissions and retry/,
    );
  });

  it("replaces every credential value in the environment with the name it came from", () => {
    const env = {
      GITHUB_TOKEN: "ghp_0123456789abcdef",
      // Contains GITHUB_TOKEN's value: replaced whole, not split by the shorter one.
      CARGO_REGISTRY_TOKEN: "ghp_0123456789abcdef-extended",
      // Matched by name case-insensitively.
      npm_auth: "npm-lowercase-name-secret",
      // Glob characters are matched literally, not as a pattern.
      DATABASE_PASSWORD: "pa*ss[wo]rd?",
      // Too short to hide without corrupting ordinary words.
      SHORT_TOKEN: "abc123",
      // Credential-shaped value, but the name does not say it is one.
      BUILD_TAG: "ghp_notacredentialname_12345",
      // A value spanning lines cannot occur within one line of output.
      MULTILINE_SECRET: "line-one-value\nline-two-value",
    };
    const input = [
      "token=ghp_0123456789abcdef",
      "cargo=ghp_0123456789abcdef-extended",
      "npm=npm-lowercase-name-secret",
      "glob=pa*ss[wo]rd? not paXXssordZ",
      "short=abc123 tag=ghp_notacredentialname_12345 multi=line-one-value",
      "a final chunk with no newline ghp_0123456789abcdef",
    ].join("\n");
    const result = sourced("redact_secrets", { env, input });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      result.stdout,
      [
        "token=<redacted:GITHUB_TOKEN>",
        "cargo=<redacted:CARGO_REGISTRY_TOKEN>",
        "npm=<redacted:npm_auth>",
        "glob=<redacted:DATABASE_PASSWORD> not paXXssordZ",
        "short=abc123 tag=ghp_notacredentialname_12345 multi=line-one-value",
        "a final chunk with no newline <redacted:GITHUB_TOKEN>",
        "",
      ].join("\n"),
    );
  });

  it("passes output through untouched when no credential is set", () => {
    const input = "  leading space, a tab\there, and a backslash \\n kept\n\nafter a blank line\n";
    const result = sourced("redact_secrets", { input });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, input);
  });
});
