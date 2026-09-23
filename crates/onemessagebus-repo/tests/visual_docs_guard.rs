//! The committed pre-push visual guard (`.githooks/pre-push`), driven the way
//! git drives it.
//!
//! The guard is the local half of the strict visual-docs gate: it decides
//! whether a push is screenshot-relevant, re-captures when it is, and blocks the
//! push when the capture drifted from the committed baseline. Every one of those
//! decisions is this repository's own shell, so every one of them is exercised
//! here — over a throwaway repository with a real git history, the real
//! `screenshots/host-arch.sh` and the real `screenshots/bless-baseline.sh`.
//!
//! What is stood in for is the subprocess seam: `screencomp` and `freeze` on
//! `PATH`, and `screenshots/capture.sh`. Running the real capture would put a
//! screenshot step inside `just check`, which the adoption keeps it out of
//! (`screenshots/AGENTS.md`), and would pay two minutes of `cargo build` per
//! case for output whose byte-identity the committed baseline already gates.
//! The stand-ins record what they were asked to do, so the journeys assert the
//! guard's decisions rather than its wording.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway repository carrying the committed guard, its scripts, a baseline
/// and a git history — plus stand-ins for the three tools the guard shells out
/// to, on a `PATH` of its own.
struct Guarded {
    dir: tempfile::TempDir,
    lane: String,
}

/// This repository's root, from which the committed guard and scripts are taken.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory is made");
    std::fs::write(path, body).expect("the file is written");
}

fn executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    write(path, body);
    let mut mode = std::fs::metadata(path)
        .expect("the file is there")
        .permissions();
    mode.set_mode(0o755);
    std::fs::set_permissions(path, mode).expect("the file is made executable");
}

fn git(dir: &Path, args: &[&str]) {
    let run = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "guard")
        .env("GIT_AUTHOR_EMAIL", "guard@example.invalid")
        .env("GIT_COMMITTER_NAME", "guard")
        .env("GIT_COMMITTER_EMAIL", "guard@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        run.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// How the stand-in tools answer: which lanes `screencomp arches` names and with
/// what status, whether `scope` calls the push relevant, whether `classify` sees
/// drift, and whether the renderer is installed at all.
#[derive(Clone, Copy)]
struct Stand<'a> {
    lanes: &'a str,
    arches_status: i32,
    drifted: bool,
    renderer: bool,
}

impl<'a> Stand<'a> {
    /// A working setup that declares `lanes` and sees no drift.
    fn declaring(lanes: &'a str) -> Self {
        Self {
            lanes,
            arches_status: 0,
            drifted: false,
            renderer: true,
        }
    }
}

impl Guarded {
    /// Stand up the repository with one commit that changes `changed`, so the
    /// guard has a real range to diff.
    fn new(changed: &str) -> Self {
        let root = repo_root();
        let dir = tempfile::tempdir().expect("a scratch repository");
        let at = dir.path();

        for name in [
            ".githooks/pre-push",
            "screenshots/host-arch.sh",
            "screenshots/bless-baseline.sh",
            "screencomp.toml",
        ] {
            let target = at.join(name);
            std::fs::create_dir_all(target.parent().expect("a parent")).expect("the directory");
            std::fs::copy(root.join(name), &target).expect("the committed file is copied");
        }

        let lane = String::from_utf8(
            Command::new("bash")
                .arg(at.join("screenshots/host-arch.sh"))
                .output()
                .expect("host-arch runs")
                .stdout,
        )
        .expect("a UTF-8 lane")
        .trim()
        .to_owned();

        // The capture stand-in: records that it ran and where it was told to
        // write, and leaves a capture tree for `bless-baseline.sh` to read.
        executable(
            &at.join("screenshots/capture.sh"),
            "#!/usr/bin/env bash\nset -euo pipefail\n\
             echo \"$SHOTS_OUT\" >>\"$GUARD_LOG.capture\"\n\
             [ -z \"${CAPTURE_FAILS:-}\" ] || { echo 'capture: no' >&2; exit 1; }\n\
             mkdir -p \"$SHOTS_OUT\"\n\
             printf '{\"schema\":1,\"shots\":[]}\\n' >\"$SHOTS_OUT/captures.json\"\n",
        );

        write(
            &at.join(format!("shots/baseline/{lane}.json")),
            "{\"schema\":1,\"shots\":[]}\n",
        );
        write(
            &at.join("README.md"),
            "the tree this push is computed over\n",
        );
        write(&at.join(changed), "one\n");

        git(at, &["init", "--quiet", "--initial-branch", "main"]);
        git(at, &["add", "-A"]);
        git(at, &["commit", "--quiet", "-m", "base"]);
        // The branch as the remote has it, which a push of a NEW branch has no
        // `remote_sha` for: the guard falls back to the merge base with
        // `origin/HEAD`, so give it one to find.
        git(at, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            at,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        write(&at.join(changed), "two\n");
        git(at, &["add", "-A"]);
        git(at, &["commit", "--quiet", "-m", "change"]);

        Self { dir, lane }
    }

    fn at(&self) -> &Path {
        self.dir.path()
    }

    /// The stand-in tools, on a `PATH` of their own.
    ///
    /// `screencomp scope` decides relevance from the changed paths it is handed,
    /// the way the real one decides it against `[guard].paths` — so a journey
    /// that gets the range wrong is a journey where nothing is captured.
    /// `$SCOPE_FORCE` overrides it, for the statuses the guard cannot act on.
    fn tools(&self, stand: Stand<'_>) -> PathBuf {
        let bin = self.at().join("stand-ins");
        executable(
            &bin.join("screencomp"),
            &format!(
                "#!/usr/bin/env bash\nset -euo pipefail\n\
                 echo \"$1\" >>\"$GUARD_LOG.screencomp\"\n\
                 case \"$1\" in\n\
                 arches) printf '{lanes}'; exit {arches} ;;\n\
                 scope) if [ -n \"${{SCOPE_FORCE:-}}\" ]; then cat >/dev/null; \
                   exit \"$SCOPE_FORCE\"; fi; \
                   if grep -q '^screenshots/'; then exit 3; else exit 0; fi ;;\n\
                 classify) exit \"${{CLASSIFY_FORCE:-{classify}}}\" ;;\n\
                 manifest) shift; while [ \"$1\" != --output ]; do shift; done; \
                   printf 'blessed\\n' >\"$2\" ;;\n\
                 gallery) shift; while [ \"$1\" != --output ]; do shift; done; \
                   mkdir -p \"$2\"; printf 'gallery\\n' >\"$2/index.html\" ;;\n\
                 *) echo \"stand-in screencomp: unexpected $*\" >&2; exit 64 ;;\n\
                 esac\n",
                lanes = stand.lanes,
                arches = stand.arches_status,
                classify = if stand.drifted { 3 } else { 0 },
            ),
        );
        if stand.renderer {
            executable(&bin.join("freeze"), "#!/usr/bin/env bash\nexit 0\n");
        }
        bin
    }

    /// Run the guard over this repository's one change, as git runs it.
    ///
    /// `PATH` is the stand-ins and the system tools the guard's own shell needs
    /// and nothing else: inheriting this machine's would let a real `screencomp`
    /// or `freeze` in `~/.local/bin` answer for a stand-in that is deliberately
    /// absent, and the journey would then prove nothing.
    fn push(&self, path: &Path, extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new("bash");
        command
            .arg(".githooks/pre-push")
            .args(["origin", "https://example.invalid/guarded.git"])
            .current_dir(self.at())
            .env("PATH", format!("{}:/usr/bin:/bin", path.display()))
            .env("GUARD_LOG", self.at().join("guard"))
            .env("SCREENCOMP_GUARD_RANGE", "HEAD~1..HEAD")
            .env_remove("CI")
            .env_remove("SCREENCOMP_GUARD_REQUIRE");
        for (key, value) in extra {
            command.env(key, value);
        }
        command.output().expect("the guard runs")
    }

    /// The commit `revision` names, as git spells it on a hook's stdin.
    fn sha(&self, revision: &str) -> String {
        let run = Command::new("git")
            .args(["rev-parse", revision])
            .current_dir(self.at())
            .output()
            .expect("git runs");
        assert!(run.status.success(), "rev-parse {revision}");
        String::from_utf8(run.stdout)
            .expect("a UTF-8 sha")
            .trim()
            .to_owned()
    }

    /// Run the guard the way git really invokes it: the ref lines on stdin and
    /// no range override, so the hook's own range arithmetic decides.
    fn push_over_stdin(&self, path: &Path, remote: &str, lines: &str) -> Output {
        use std::io::Write as _;
        let mut child = Command::new("bash")
            .arg(".githooks/pre-push")
            .args([remote, "https://example.invalid/guarded.git"])
            .current_dir(self.at())
            .env("PATH", format!("{}:/usr/bin:/bin", path.display()))
            .env("GUARD_LOG", self.at().join("guard"))
            .env_remove("CI")
            .env_remove("SCREENCOMP_GUARD_RANGE")
            .env_remove("SCREENCOMP_GUARD_REQUIRE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the guard spawns");
        child
            .stdin
            .take()
            .expect("a stdin pipe")
            .write_all(lines.as_bytes())
            .expect("the ref lines are written");
        child.wait_with_output().expect("the guard exits")
    }

    /// What a stand-in recorded, or nothing when it never ran.
    fn log(&self, tool: &str) -> String {
        std::fs::read_to_string(self.at().join(format!("guard.{tool}"))).unwrap_or_default()
    }

    fn baseline(&self) -> String {
        std::fs::read_to_string(self.at().join(format!("shots/baseline/{}.json", self.lane)))
            .expect("the baseline is there")
    }
}

fn stderr(run: &Output) -> String {
    String::from_utf8_lossy(&run.stderr).into_owned()
}

fn stdout(run: &Output) -> String {
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn a_push_touching_nothing_screenshot_relevant_passes_without_capturing() {
    let guarded = Guarded::new("docs/unrelated.md");
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    let run = guarded.push(&tools, &[]);

    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "", "a silent pass says nothing");
    assert_eq!(
        guarded.log("capture"),
        "",
        "it captured on an irrelevant change"
    );
}

#[test]
fn a_relevant_push_captures_and_passes_when_the_capture_has_not_drifted() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    let run = guarded.push(&tools, &[]);

    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(
        guarded.log("capture").trim(),
        format!("shots/current/{}", guarded.lane),
        "the capture wrote somewhere other than this host's lane"
    );
    assert!(
        stdout(&run).contains("ok to push"),
        "a clean classify did not say so: {}",
        stdout(&run)
    );
    assert_eq!(
        guarded.baseline(),
        "{\"schema\":1,\"shots\":[]}\n",
        "a clean push re-blessed"
    );
}

#[test]
fn drift_re_blesses_this_lane_builds_a_gallery_and_blocks_the_push() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand {
        drifted: true,
        ..Stand::declaring(&guarded.lane)
    });
    let run = guarded.push(&tools, &[]);

    assert_eq!(run.status.code(), Some(1), "drift did not block the push");
    assert_eq!(
        guarded.baseline(),
        "blessed\n",
        "the drifted lane was not re-blessed"
    );
    assert_eq!(
        std::fs::read_to_string(guarded.at().join("shots/review/index.html")).unwrap_or_default(),
        "gallery\n",
        "no review gallery was built for the blocked push"
    );
    let said = stderr(&run);
    assert!(said.contains("shots/review/index.html"), "{said}");
    assert!(said.contains("docs/screenshots"), "{said}");
}

#[test]
fn a_relevant_push_refuses_when_the_renderer_is_not_installed() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand {
        renderer: false,
        ..Stand::declaring(&guarded.lane)
    });
    let run = guarded.push(&tools, &[]);

    assert_eq!(
        run.status.code(),
        Some(1),
        "a missing renderer did not refuse"
    );
    assert!(
        stderr(&run).contains("just screenshots-tools"),
        "{}",
        stderr(&run)
    );
    assert_eq!(guarded.log("capture"), "", "it captured without a renderer");
}

#[test]
fn a_host_whose_arch_no_lane_declares_is_refused_by_name() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand::declaring("s390x\\n"));
    let run = guarded.push(&tools, &[]);

    assert_eq!(
        run.status.code(),
        Some(1),
        "an undeclared lane was not refused"
    );
    let said = stderr(&run);
    assert!(
        said.contains(&guarded.lane),
        "the refusal does not name this host: {said}"
    );
    assert!(
        said.contains("s390x"),
        "the refusal does not name what is declared: {said}"
    );
    assert_eq!(
        guarded.log("capture"),
        "",
        "it captured for a lane with no baseline"
    );
}

#[test]
fn without_screencomp_it_warns_loudly_and_only_fails_when_told_to() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let bare = guarded.at().join("no-tools");
    std::fs::create_dir_all(&bare).expect("an empty bin");

    let warned = guarded.push(&bare, &[]);
    assert!(
        warned.status.success(),
        "a missing screencomp refused by default"
    );
    assert!(
        stderr(&warned).contains("NOT on PATH"),
        "{}",
        stderr(&warned)
    );

    let required = guarded.push(&bare, &[("SCREENCOMP_GUARD_REQUIRE", "1")]);
    assert_eq!(
        required.status.code(),
        Some(1),
        "SCREENCOMP_GUARD_REQUIRE did not make a missing screencomp fatal"
    );
}

#[test]
fn under_ci_the_guard_stands_down_for_the_workflow() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand {
        drifted: true,
        ..Stand::declaring(&guarded.lane)
    });
    let run = guarded.push(&tools, &[("CI", "true")]);

    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(guarded.log("capture"), "", "it captured under CI");
}

/// The zero sha git writes for the side of a push that does not exist.
const ABSENT: &str = "0000000000000000000000000000000000000000";

#[test]
fn an_ordinary_update_diffs_what_the_remote_already_has_against_what_is_pushed() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    let run = guarded.push_over_stdin(
        &tools,
        "origin",
        &format!(
            "refs/heads/main {} refs/heads/main {}\n",
            guarded.sha("HEAD"),
            guarded.sha("HEAD~1"),
        ),
    );

    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        !guarded.log("capture").is_empty(),
        "the update's own commit was never diffed, so nothing was captured"
    );
}

#[test]
fn a_new_branch_falls_back_to_its_merge_base_with_the_remote_head() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    // No `remote_sha`: the remote has never seen this branch.
    let run = guarded.push_over_stdin(
        &tools,
        "origin",
        &format!(
            "refs/heads/shots {} refs/heads/shots {ABSENT}\n",
            guarded.sha("HEAD"),
        ),
    );

    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        !guarded.log("capture").is_empty(),
        "a new branch's commits were not diffed against origin/HEAD"
    );
}

#[test]
fn a_branch_deletion_carries_nothing_to_capture() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    // No `local_sha`: this push removes the branch. The sha the remote holds it
    // at is one whose tree differs from this one in a screenshot-relevant file,
    // so a guard that read the deletion as a range would capture.
    let run = guarded.push_over_stdin(
        &tools,
        "origin",
        &format!(
            "(delete) {ABSENT} refs/heads/gone {}\n",
            guarded.sha("HEAD~1")
        ),
    );

    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(
        guarded.log("capture"),
        "",
        "a deletion captured screenshots"
    );
}

#[test]
fn a_screencomp_that_names_no_lane_is_refused_rather_than_read_as_an_empty_set() {
    for stand in [
        Stand {
            arches_status: 1,
            ..Stand::declaring("")
        },
        Stand::declaring("\\n"),
    ] {
        let guarded = Guarded::new("screenshots/capture-inputs.txt");
        let tools = guarded.tools(stand);
        let run = guarded.push(&tools, &[]);

        assert_eq!(
            run.status.code(),
            Some(1),
            "a screencomp answering no lane did not refuse: {}",
            stderr(&run)
        );
        assert!(
            stderr(&run).contains("named no capture lane"),
            "{}",
            stderr(&run)
        );
        assert_eq!(
            guarded.log("capture"),
            "",
            "it captured with no lane to classify"
        );
    }
}

#[test]
fn a_new_branch_with_no_merge_base_captures_rather_than_guessing() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    // A clone with no `origin/HEAD` — a fresh remote, or one whose default
    // branch was never fetched — leaves no fork point to diff from. The working
    // tree is clean and matches the pushed commit, so `git diff <local_sha>`
    // would report nothing at all: the guard must not read that as "no
    // screenshot changed".
    git(
        guarded.at(),
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );

    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    let run = guarded.push_over_stdin(
        &tools,
        "origin",
        &format!(
            "refs/heads/shots {} refs/heads/shots {ABSENT}\n",
            guarded.sha("HEAD"),
        ),
    );

    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        !guarded.log("capture").is_empty(),
        "an underivable push was let through uncaptured"
    );
    assert!(
        stderr(&run).contains("no merge base"),
        "it captured without saying why: {}",
        stderr(&run)
    );
    assert!(
        !guarded.log("screencomp").contains("scope"),
        "it asked `scope` about a changed-path list it could not derive"
    );
}

#[test]
fn a_range_override_that_names_no_revision_is_refused() {
    for range in ["--output=/dev/null", "refs/heads/never-existed..HEAD"] {
        let guarded = Guarded::new("screenshots/capture-inputs.txt");
        let tools = guarded.tools(Stand::declaring(&guarded.lane));
        let run = guarded.push(&tools, &[("SCREENCOMP_GUARD_RANGE", range)]);

        assert_eq!(
            run.status.code(),
            Some(1),
            "{range} was accepted: {}",
            stderr(&run)
        );
        assert!(
            stderr(&run).contains("SCREENCOMP_GUARD_RANGE"),
            "the refusal does not name the override: {}",
            stderr(&run)
        );
        assert_eq!(guarded.log("capture"), "", "it captured for {range}");
    }
}

#[test]
fn a_scope_it_cannot_act_on_lets_the_push_through_rather_than_capturing_blindly() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    // Neither 0 (nothing relevant) nor 3 (relevant): a screencomp too old to
    // answer, which is not this push's fault and which CI still gates.
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    let run = guarded.push(&tools, &[("SCOPE_FORCE", "64")]);

    assert!(run.status.success(), "an unusable scope blocked the push");
    assert!(
        stderr(&run).contains("skipping the"),
        "the skip was silent: {}",
        stderr(&run)
    );
    assert_eq!(
        guarded.log("capture"),
        "",
        "it paid for a capture it could not decide it needed"
    );
}

#[test]
fn a_push_to_another_remote_forks_from_that_remote_rather_than_origin() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    // A fork, with `origin` already carrying this commit and `upstream` a commit
    // behind: measuring against `origin` would report nothing changed at all.
    // The push is going to `upstream`, and that is whose history the range must
    // be taken against.
    git(
        guarded.at(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(
        guarded.at(),
        &["update-ref", "refs/remotes/upstream/main", "HEAD~1"],
    );
    git(
        guarded.at(),
        &[
            "symbolic-ref",
            "refs/remotes/upstream/HEAD",
            "refs/remotes/upstream/main",
        ],
    );

    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    let run = guarded.push_over_stdin(
        &tools,
        "upstream",
        &format!(
            "refs/heads/shots {} refs/heads/shots {ABSENT}\n",
            guarded.sha("HEAD"),
        ),
    );

    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        !guarded.log("capture").is_empty(),
        "the fork point came from a remote this push is not going to"
    );
    assert!(
        !stderr(&run).contains("no merge base"),
        "it fell back instead of forking from upstream: {}",
        stderr(&run)
    );
}

#[test]
fn a_capture_that_fails_blocks_the_push_rather_than_passing_it() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    let run = guarded.push(&tools, &[("CAPTURE_FAILS", "1")]);

    assert_ne!(
        run.status.code(),
        Some(0),
        "a push whose capture never ran was let through: {}",
        stderr(&run)
    );
    assert!(
        !guarded.log("screencomp").contains("classify"),
        "it classified a capture that was never written"
    );
}

#[test]
fn a_classify_that_cannot_answer_refuses_in_this_repositorys_exit_codes() {
    let guarded = Guarded::new("screenshots/capture-inputs.txt");
    let tools = guarded.tools(Stand::declaring(&guarded.lane));
    // Neither 0 (clean) nor 3 (drift): screencomp failing rather than answering.
    let run = guarded.push(&tools, &[("CLASSIFY_FORCE", "64")]);

    assert_eq!(
        run.status.code(),
        Some(1),
        "the tool's own status escaped a hook whose codes are 0/1/2: {}",
        stderr(&run)
    );
    assert!(
        stderr(&run).contains("could not evaluate the capture"),
        "the refusal does not say what went wrong: {}",
        stderr(&run)
    );
    assert_eq!(
        guarded.baseline(),
        "{\"schema\":1,\"shots\":[]}\n",
        "it re-blessed a lane it could not classify"
    );
}

#[test]
fn bootstrapping_a_clone_leaves_the_committed_guard_active() {
    // The command is read from the manifest rather than restated, so this proves
    // what `just bootstrap` really runs: Nx fans `bootstrap` across every
    // project, and this project's is the whole of the activation.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("screenshots/project.json"))
            .expect("the visual-docs project manifest"),
    )
    .expect("the manifest is JSON");
    let command = manifest["targets"]["bootstrap"]["command"]
        .as_str()
        .expect("screenshots/project.json declares no bootstrap command");

    let clone = tempfile::tempdir().expect("a fresh clone");
    git(
        clone.path(),
        &["init", "--quiet", "--initial-branch", "main"],
    );
    assert!(
        Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .current_dir(clone.path())
            .output()
            .expect("git runs")
            .stdout
            .is_empty(),
        "a fresh clone already had a hooks path"
    );

    let ran = Command::new("bash")
        .args(["-c", command])
        .current_dir(clone.path())
        .output()
        .expect("the bootstrap command runs");
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let configured = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .current_dir(clone.path())
        .output()
        .expect("git runs");
    let path = String::from_utf8_lossy(&configured.stdout)
        .trim()
        .to_owned();
    assert_eq!(
        path, ".githooks",
        "bootstrapping did not point git at the committed hooks directory"
    );
    // And that directory carries the guard — and, per Contract 3, nothing else.
    let hooks: Vec<String> = std::fs::read_dir(repo_root().join(&path))
        .expect("the committed hooks directory")
        .map(|entry| {
            entry
                .expect("a hook")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        hooks,
        vec!["pre-push".to_owned()],
        "the guard directory carries something other than the visual guard"
    );
}
