// How the launcher journeys start `npm` and the launcher `npm install` put in a
// project's `node_modules/.bin`.
//
// On Linux and macOS both are executables the kernel runs by name. On Windows
// both are batch files — `npm.cmd`, and the `onemessagebus.cmd` shim npm writes —
// and Node refuses to start a batch file without a shell (CVE-2024-27980),
// failing with ENOENT for the bare name and EINVAL for the `.cmd`. The two need
// different answers there:
//
// - `npm` is started as the Node running the journeys on the `npm-cli.js` that
//   ships beside it, so no batch file is involved. Running `npm.cmd` through
//   `cmd.exe` by its bare name does not work: the shim finds its own directory
//   with `%~dp0`, which for a quoted name found on PATH is the working
//   directory, so it looks for `npm-cli.js` under the repository and fails.
// - The launcher shim is the thing under test, so it is run as a `.cmd` by
//   `cmd.exe` on a line quoted here — what a shell spawn does underneath, without
//   `shell: true` splicing argv into a command line unquoted. It is always given
//   by full path, which is how a batch file resolves its own directory reliably.
//
// The platform is an argument, never `process.platform` read here, so the
// Windows answers are held by `npm/test/invocation.test.mjs` on any host.

import { win32 } from "node:path";

/// The batch-file extension npm gives a shim on Windows, where it writes one.
const WINDOWS_SHIM = ".cmd";

/// Characters `cmd.exe` interprets inside a double-quoted word: a quote ends the
/// word and `%` expands a variable. No argument the journeys pass holds either —
/// Windows paths cannot contain `"` — so one that does is refused rather than
/// passed through altered.
const CMD_UNQUOTABLE = /["%]/;

/// The form that starts `npm` with `args` on `platform`: a `command`, its `args`,
/// and the spawn options that form needs, to spread into `execFileSync`.
/// `execPath` is the Node binary whose bundled npm Windows runs
/// (`process.execPath`); npm's Windows installs put `node_modules\npm` beside it.
export function npmInvocation(args, { platform, execPath }) {
  if (platform !== "win32") return { command: "npm", args, options: {} };
  const npmCli = win32.join(win32.dirname(execPath), "node_modules", "npm", "bin", "npm-cli.js");
  return { command: execPath, args: [npmCli, ...args], options: {} };
}

/// The form that starts the shim npm wrote at `path` — given without its Windows
/// extension — with `args` on `platform`.
export function shimInvocation(path, args, platform) {
  if (platform !== "win32") return { command: path, args, options: {} };
  if (!win32.isAbsolute(path)) {
    throw new Error(
      `cannot run the shim ${JSON.stringify(path)} through cmd.exe: give its full path, since a batch file started by a bare name misreads its own directory`,
    );
  }
  const words = [`${path}${WINDOWS_SHIM}`, ...args].map((word) => {
    if (CMD_UNQUOTABLE.test(word)) {
      throw new Error(
        `cannot pass ${JSON.stringify(word)} through cmd.exe: it holds a " or %, which cmd.exe would reinterpret`,
      );
    }
    return `"${word}"`;
  });
  return {
    command: "cmd.exe",
    // `/d` skips AutoRun, `/s` strips exactly the outer quotes and keeps the
    // quoted words inside them verbatim, `/c` runs the line and exits with its code.
    args: ["/d", "/s", "/c", `"${words.join(" ")}"`],
    options: { windowsVerbatimArguments: true },
  };
}
