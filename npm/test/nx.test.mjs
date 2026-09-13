// `scripts/nx` and `scripts/nx-affected.sh`, run for real.
//
// Every case runs the wrappers over a real Nx workspace — this repository's
// pinned Nx and a real git history with a fork point, built by
// ./support/nx-workspace.mjs — and asserts what a person running `just check`
// sees: the exit status, the one line or the replayed log, the file it names,
// and which projects' targets actually ran.

import { execFileSync, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, describe, it } from "node:test";
import { stripVTControlCharacters } from "node:util";
import assert from "node:assert/strict";

import { checkout, createUpstream, wrapper } from "./support/nx-workspace.mjs";

const POSIX_ONLY = process.platform === "win32" && "builds its workspace with POSIX tools";

let scratch;
let upstream;
let marks = 0;

before(() => {
  scratch = mkdtempSync(join(tmpdir(), "nx-wrappers-"));
  upstream = createUpstream(scratch);
});
after(() => {
  if (scratch) rmSync(scratch, { recursive: true, force: true });
});

function markFile() {
  marks += 1;
  return join(scratch, `ran-${marks}`);
}

/// The projects whose `mark` target ran, as they recorded themselves.
function ran(mark) {
  return existsSync(mark) ? readFileSync(mark, "utf8").split("\n").filter(Boolean).sort() : [];
}

describe("scripts/nx", { skip: POSIX_ONLY }, () => {
  let workspace;
  before(() => {
    workspace = checkout(scratch, "wrapper", upstream.dir);
  });

  it("runs the requested target and owes one line naming the owner-only log", () => {
    const mark = markFile();
    const result = wrapper(workspace, "nx", ["run", "b:mark"], { MARK_FILE: mark });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, "");
    const log = join(realpathSync(workspace), ".logs", "nx.log");
    assert.equal(result.stdout, `nx: requested targets succeeded (full output: ${log})\n`);
    assert.deepEqual(ran(mark), ["b"]);
    assert.match(readFileSync(log, "utf8"), /nx run b:mark/, "the log does not hold Nx's output");
    assert.equal(statSync(log).mode & 0o777, 0o600);
  });

  it("on failure replays the log with credential values redacted, then names it", () => {
    const secret = "nx-workspace-secret-0123456789";
    const result = wrapper(workspace, "nx", ["run", "a:leak"], { NX_WORKSPACE_TOKEN: secret });
    assert.equal(result.status, 1);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /the token is <redacted:NX_WORKSPACE_TOKEN>/);
    assert.ok(!result.stderr.includes(secret), "the credential value reached the terminal");
    const log = join(realpathSync(workspace), ".logs", "nx.log");
    assert.ok(
      !readFileSync(log, "utf8").includes(secret),
      "the credential value reached the log on disk",
    );
    assert.match(
      result.stderr,
      new RegExp(
        `nx: targets failed; fix the reported findings above and rerun the same 'just' recipe \\(full output: ${log.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\)\\n$`,
      ),
    );
  });

  it("streams Nx's own stdout untouched when a caller reads it", () => {
    const result = wrapper(workspace, "nx", ["show", "projects", "--json"], {
      ONEMESSAGEBUS_NX_SHOW_OUTPUT: "1",
    });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(JSON.parse(result.stdout).sort(), ["a", "b"]);
  });

  it("keeps the log of a run whose target runs the wrapper again", () => {
    const mark = markFile();
    const result = wrapper(workspace, "nx", ["run", "a:nested"], { MARK_FILE: mark });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(ran(mark), ["b"]);
    const outer = readFileSync(join(workspace, ".logs", "nx.log"), "utf8");
    assert.match(outer, /nx run a:nested/, "the nested run truncated the outer run's log");
    const inner = outer.match(
      /nx: requested targets succeeded \(full output: (.*\/\.logs\/nx\.\d+\.log)\)/,
    );
    assert.ok(inner, `the nested run did not report a diverted log:\n${outer}`);
    // A nested run inherits the colour Nx forces on its children; the claim is
    // about what it wrote, not how it was styled.
    assert.match(stripVTControlCharacters(readFileSync(inner[1], "utf8")), /nx run b:mark/);
  });

  it("says npm is missing, with what to install, on a clone that has no Nx", () => {
    const bare = checkout(scratch, "no-npm", upstream.dir, { linkNodeModules: false });
    // A machine with bash and nothing else this reaches for before npm.
    const bin = join(scratch, "bin-without-npm");
    execFileSync("mkdir", [bin]);
    const which = (tool) =>
      execFileSync("bash", ["-c", `command -v ${tool}`], { encoding: "utf8" }).trim();
    for (const tool of ["bash", "dirname"]) symlinkSync(which(tool), join(bin, tool));
    const result = spawnSync(join(bin, "bash"), ["scripts/nx", "show", "projects"], {
      cwd: bare,
      env: { PATH: bin, HOME: process.env.HOME },
      encoding: "utf8",
    });
    assert.equal(result.status, 1);
    assert.match(
      result.stderr,
      /nx: npm not found; cannot install the pinned Nx the project graph needs/,
    );
    assert.match(result.stderr, /ACTION: install Node\.js 20\+ .* and re-run 'just bootstrap'/);
  });

  it("reports a locked install that failed, with the next action", () => {
    // A clone with no lockfile: `npm ci` refuses it without reaching a registry.
    const bare = checkout(scratch, "no-lockfile", upstream.dir, { linkNodeModules: false });
    const result = wrapper(bare, "nx", ["show", "projects"]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /nx: 'npm ci' failed in /);
    assert.match(
      result.stderr,
      /ACTION: check network access to the npm registry, then re-run 'just bootstrap'/,
    );
  });
});

describe("scripts/nx-affected.sh", { skip: POSIX_ONLY }, () => {
  let full;
  let shallow;
  before(() => {
    full = checkout(scratch, "full", upstream.dir);
    shallow = checkout(scratch, "shallow", upstream.dir, { shallow: true });
  });

  function affects(dir, project, env = {}) {
    const result = wrapper(dir, "nx-affected.sh", ["--affects", project], env);
    assert.equal(result.status, 0, result.stderr);
    return result;
  }

  it("scopes to the projects the branch changed since its fork point", () => {
    assert.deepEqual([affects(full, "a").stdout, affects(full, "b").stdout], ["true\n", "false\n"]);
    const mark = markFile();
    const result = wrapper(full, "nx-affected.sh", ["-t", "mark"], { MARK_FILE: mark });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^nx: requested targets succeeded/);
    assert.deepEqual(ran(mark), ["a"], "the affected run was not scoped to the changed project");
  });

  it("scopes a pull-request build against the base branch GitHub names, fetched first", () => {
    const result = affects(full, "b", { CI: "true", GITHUB_BASE_REF: "main" });
    assert.equal(result.stdout, "false\n", result.stderr);
  });

  it("scopes a push against the commit it replaced", () => {
    const result = affects(full, "b", { ONEMESSAGEBUS_NX_BASE_SHA: upstream.forkPoint });
    assert.equal(result.stdout, "false\n", result.stderr);
  });

  it("fails closed in a shallow checkout that cannot reach the fork point", () => {
    const answer = affects(shallow, "b");
    assert.equal(answer.stdout, "true\n");
    assert.match(
      answer.stderr,
      /no merge base, so 'b' counts as affected \(git fetch --unshallow to narrow it\)/,
    );
    const mark = markFile();
    const result = wrapper(shallow, "nx-affected.sh", ["-t", "mark"], { MARK_FILE: mark });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stderr, /no merge base, so every project runs/);
    assert.deepEqual(
      ran(mark),
      ["a", "b"],
      "a shallow checkout ran a scoped set as if it were the whole",
    );
  });

  it("fails closed when the base branch does not exist", () => {
    const result = affects(full, "b", { ONEMESSAGEBUS_NX_BASE_REF: "release" });
    assert.equal(result.stdout, "true\n");
    assert.match(result.stderr, /no merge base/);
  });

  it("fails closed on a CI build that is not a pull request", () => {
    const answer = affects(full, "b", { CI: "true" });
    assert.equal(answer.stdout, "true\n");
    assert.match(
      answer.stderr,
      /not a pull-request build, so every project runs \(set ONEMESSAGEBUS_NX_BASE_REF to scope one\)/,
    );
  });

  it("refuses a base branch that is not a plain branch name, and fails closed", () => {
    for (const ref of ["--upload-pack=touch pwned", "main;id", ".hidden"]) {
      const result = affects(full, "b", { CI: "true", ONEMESSAGEBUS_NX_BASE_REF: ref });
      assert.equal(result.stdout, "true\n", `'${ref}' was used as a base`);
      assert.match(result.stderr, /is not a usable branch name/);
    }
    assert.equal(existsSync(join(full, "pwned")), false);
  });

  it("fails closed on a base commit that is not in this checkout", () => {
    for (const sha of ["0123456789abcdef0123456789abcdef01234567", "HEAD~1"]) {
      const result = affects(full, "b", { ONEMESSAGEBUS_NX_BASE_SHA: sha });
      assert.equal(result.stdout, "true\n", `'${sha}' was used as a base`);
      assert.match(
        result.stderr,
        new RegExp(
          `ONEMESSAGEBUS_NX_BASE_SHA '${sha.replace("~", "\\~")}' is not a commit in this checkout`,
        ),
      );
    }
  });

  it("counts a project as affected when Nx cannot list the graph", () => {
    const broken = checkout(scratch, "broken-graph", upstream.dir);
    writeFileSync(join(broken, "b", "project.json"), "{ this is not a project\n");
    const result = affects(broken, "b");
    assert.equal(result.stdout, "true\n");
    assert.match(
      result.stderr,
      /Nx could not list the affected projects, so 'b' counts as affected \(reproduce with 'just nx show projects --affected --base=[0-9a-f]+ --head=HEAD'\)/,
    );
  });

  it("refuses an invocation with nothing to run", () => {
    const none = wrapper(full, "nx-affected.sh", []);
    assert.equal(none.status, 2);
    assert.match(none.stderr, /pass the Nx arguments to run/);
    const unnamed = wrapper(full, "nx-affected.sh", ["--affects"]);
    assert.equal(unnamed.status, 2);
    assert.match(unnamed.stderr, /--affects needs a project name/);
  });
});
