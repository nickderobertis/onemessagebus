//! The fixture the journeys share: the built binary, a scratch directory, and
//! the recorded streams.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `onemessagebus` binary the journeys drive.
///
/// `ONEMESSAGEBUS_BIN` names one explicitly — the install journey points it at
/// what a user installed — and otherwise it is the one Cargo built beside this
/// test, which the `test` target's `dependsOn` guarantees is fresh.
pub fn binary() -> PathBuf {
    if let Some(path) = std::env::var_os("ONEMESSAGEBUS_BIN") {
        return PathBuf::from(path);
    }
    let path = assert_cmd::cargo::cargo_bin("onemessagebus");
    assert!(
        path.is_file(),
        "no onemessagebus binary at {}; build it with `cargo build -p onemessagebus-cli` or run `just test-e2e`",
        path.display()
    );
    path
}

/// A command over the binary, with no inherited registry, configuration,
/// transport directory or schema cache settings.
pub fn onemessagebus() -> Command {
    let mut command = Command::new(binary());
    command
        .env_remove("ONEMESSAGEBUS_REGISTRY")
        .env_remove("ONEMESSAGEBUS_CONFIG")
        .env_remove("ONEMESSAGEBUS_TRANSPORT_DIR")
        .env_remove("ONEMESSAGEBUS_SCHEMA_CACHE_DIR")
        .env_remove("ONEMESSAGEBUS_SCHEMA_TTL")
        .env_remove("ONEMESSAGEBUS_SCHEMA_REFRESH");
    command
}

/// What one run of the binary produced.
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// The stdout as JSON documents, one per line.
    pub fn lines(&self) -> Vec<serde_json::Value> {
        self.stdout
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("stdout line is not JSON: {e}: {line}"))
            })
            .collect()
    }
}

/// Run `args` against the binary with `stdin`, from the working directory
/// `cwd`, and hand back everything it said.
pub fn run_in(cwd: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Run {
    use std::io::Write as _;
    let mut command = onemessagebus();
    command
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("the binary spawns");
    {
        let mut handle = child.stdin.take().expect("a stdin pipe");
        if let Some(text) = stdin {
            // A verb that refuses its arguments exits before it reads stdin,
            // and a pipe nobody reads is not a failure of this fixture.
            match handle.write_all(text.as_bytes()) {
                Ok(()) => {}
                Err(failure) if failure.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(failure) => panic!("stdin is written: {failure}"),
            }
        }
    }
    let output = child.wait_with_output().expect("the binary exits");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

/// [`run_in`] from a scratch directory of its own.
pub fn run(args: &[&str], stdin: Option<&str>) -> Run {
    let dir = tempfile::tempdir().expect("a temp dir");
    run_in(dir.path(), args, stdin, &[])
}

/// A usage error `verb` refused as the exit-code table gives refused input:
/// exit 2, nothing on stdout, and one line on stderr — `onemessagebus: <verb>: `,
/// what was wrong (`what`), and the verb's `--help` — with nothing of clap's
/// own `error:` report or usage synopsis.
pub fn assert_usage_refused(run: &Run, verb: &str, what: &str) {
    assert_eq!(run.code, 2, "{verb}: {what}: {}", run.stderr);
    assert_eq!(run.stdout, "", "{verb}: {what}");
    assert_eq!(
        run.stderr.lines().count(),
        1,
        "{verb}: {what}: not one line: {}",
        run.stderr
    );
    let line = run.stderr.trim_end();
    assert!(
        line.starts_with(&format!("onemessagebus: {verb}: ")),
        "{verb}: {what}: {line}"
    );
    assert!(line.contains(what), "{verb}: {what}: {line}");
    assert!(
        line.ends_with(&format!("; see `onemessagebus {verb} --help`")),
        "{verb}: {what}: {line}"
    );
    assert!(
        !line.contains("error:") && !line.contains("Usage:"),
        "{verb}: {what}: clap's own report: {line}"
    );
}

/// The profile crate's recorded and golden fixtures.
pub fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        // llmlint: ignore[shared_internals_are_a_project_not_a_reach_in] the task places the recorded streams under the profile crate's tests/recorded/ and requires these journeys to run `events merge` over those same streams; they are byte-identical producer output, so a copy here would be a second one free to drift.
        .join("../onemessagebus-agent/tests")
        .join(relative)
}

/// `n` bytes of ASCII.
pub fn ascii(n: usize) -> String {
    "abcdefghij".chars().cycle().take(n).collect()
}
