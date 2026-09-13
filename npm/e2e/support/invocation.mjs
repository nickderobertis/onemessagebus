// How the launcher journeys start a program that npm puts on disk as a shim.
//
// On Linux and macOS `npm` and the installed `node_modules/.bin/onemessagebus`
// are executables the kernel runs by name. On Windows both are batch files —
// `npm.cmd`, and the `onemessagebus.cmd` shim `npm install` writes — and Node
// refuses to start a batch file without a shell (CVE-2024-27980), failing with
// ENOENT for the bare name and EINVAL for the `.cmd`. Rather than lean on
// `shell: true`, which splices argv into a command line with no quoting, the
// Windows form starts `cmd.exe` itself on a line quoted here, which is exactly
// what a shell spawn does underneath and what the batch file needs to run.
//
// The platform is an argument, never `process.platform` read here, so the
// Windows answer is held by `npm/test/invocation.test.mjs` on any host.

/// The batch-file extension npm gives a shim on Windows, where it writes one.
const WINDOWS_SHIM = ".cmd";

/// Characters `cmd.exe` interprets inside a double-quoted word: a quote ends the
/// word and `%` expands a variable. No argument the journeys pass holds either —
/// Windows paths cannot contain `"` — so one that does is refused rather than
/// passed through altered.
const CMD_UNQUOTABLE = /["%]/;

/// The form that starts `program` with `args` on `platform`: a `command`, its
/// `args`, and the spawn options that form needs, to spread into
/// `execFileSync`/`spawnSync`. `program` is a bare name found on PATH (`npm`) or
/// the path of a shim npm wrote, without its Windows extension.
export function invocation(program, args, platform) {
  if (platform !== "win32") return { command: program, args, options: {} };
  const words = [`${program}${WINDOWS_SHIM}`, ...args].map((word) => {
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
