//! The visual-docs scripts that stand apart from the capture, driven for real.
//!
//! `capture.sh` itself is not here and deliberately not: exercising it means
//! rendering screenshots, which the adoption keeps out of `just check`,
//! `just gate` and CI's gate job (`screenshots/AGENTS.md`), and what it produces
//! is gated instead by the committed digest baseline. What *can* be driven
//! cheaply and offline is everything around it — the renderer's installer over a
//! stand-in release tree, the fixture stager, the normalizer, and the blessing
//! command's refusals — so each is.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// This repository's root, where the scripts under test live.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

fn script(name: &str) -> PathBuf {
    repo_root().join("screenshots").join(name)
}

/// Run a script with `args`, `env`, and `stdin`, from `cwd`.
fn run(name: &str, args: &[&str], env: &[(&str, &str)], cwd: &Path, stdin: &str) -> Output {
    use std::io::Write as _;
    let mut command = Command::new("bash");
    command
        .arg(script(name))
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("the script spawns");
    child
        .stdin
        .take()
        .expect("a stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("stdin is written");
    child.wait_with_output().expect("the script exits")
}

fn stdout(run: &Output) -> String {
    String::from_utf8_lossy(&run.stdout).into_owned()
}

fn stderr(run: &Output) -> String {
    String::from_utf8_lossy(&run.stderr).into_owned()
}

#[test]
fn the_normalizer_fixes_every_per_run_value_and_leaves_the_rest_alone() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let captured = concat!(
        "correlation: c-aa0c643d5613f4eaf184cade12551980\n",
        r#"{"id":0,"raised_at":1790140877906,"at":1790140879909,"#,
        r#""correlation":"c-aa0c643d5613f4eaf184cade12551980","#,
        r#""message":"the base moved","bytes":48213,"position":354}"#,
        "\n",
    );

    let first = run("normalize.sh", &[], &[], dir.path(), captured);
    assert!(first.status.success(), "{}", stderr(&first));
    let out = stdout(&first);

    assert!(
        !out.contains("c-aa0c643d5613f4eaf184cade12551980"),
        "the minted correlation survived: {out}"
    );
    assert_eq!(
        out.matches("c-4f3c1d92a08b47e6b1d5c0a7e93f2b18").count(),
        2,
        "both correlations were not rewritten to the one placeholder: {out}"
    );
    assert!(out.contains(r#""raised_at":1789300000000"#), "{out}");
    assert!(out.contains(r#""at":1789300000000"#), "{out}");
    // Numbers that are not instants are content, not variance: an artifact's
    // size and a record's byte position must survive untouched.
    assert!(out.contains(r#""bytes":48213"#), "{out}");
    assert!(out.contains(r#""position":354"#), "{out}");
    assert!(out.contains(r#""message":"the base moved""#), "{out}");

    // Already-normalized text is a fixed point, which is what lets a second
    // capture of one build produce the same bytes.
    let again = run("normalize.sh", &[], &[], dir.path(), &out);
    assert_eq!(stdout(&again), out, "normalizing twice moved the text");
}

#[test]
fn the_stager_writes_one_configuration_both_renderers_can_read() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let root = dir.path().join("fixture");
    let staged = run(
        "stage-fixture.sh",
        &[root.to_str().expect("a UTF-8 path")],
        &[],
        dir.path(),
        "",
    );
    assert!(staged.status.success(), "{}", stderr(&staged));
    assert_eq!(
        stdout(&staged).trim(),
        root.join("bus.yaml").to_str().expect("a UTF-8 path"),
        "the stager named a configuration other than the one it wrote"
    );

    let config = std::fs::read_to_string(root.join("bus.yaml")).expect("the configuration");
    assert!(
        config.contains(&format!("dir: \"{}/bus\"", root.display())),
        "the transport is not kept under the directory asked for: {config}"
    );
    assert!(config.contains("profile: desk"), "{config}");
    // The journeys' own layout document, linked rather than copied.
    assert!(
        config.contains("crates/onemessagebus-e2e/tests/layouts/desk.json@1"),
        "the desk layout is not the journeys' own: {config}"
    );
    // The codec's frames come over a `file://` link, so no scene needs HTTP.
    assert!(
        config.contains("file://") && config.contains("screenshots/fixture/frames.json@1"),
        "{config}"
    );
    assert!(config.contains("checkout.frame.quote@1"), "{config}");
}

#[test]
fn the_stager_refuses_a_directory_the_configuration_could_not_name() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    for (root, what) in [
        ("-rf", "reads as an option"),
        ("a\"quote", "carries a quote"),
    ] {
        let refused = run("stage-fixture.sh", &[root], &[], dir.path(), "");
        assert_eq!(
            refused.status.code(),
            Some(1),
            "{root} was accepted: {}",
            stderr(&refused)
        );
        assert!(
            stderr(&refused).contains(what),
            "the refusal of {root} does not say why: {}",
            stderr(&refused)
        );
        assert!(
            !dir.path().join(root).exists(),
            "{root} was created before it was refused"
        );
    }

    let missing = run("stage-fixture.sh", &[], &[], dir.path(), "");
    assert_ne!(missing.status.code(), Some(0), "a nameless fixture staged");
    assert!(
        stderr(&missing).contains("stage-fixture"),
        "{}",
        stderr(&missing)
    );
}

/// A release tree the installer can fetch over `file://`: one archive holding
/// `freeze_<version>_Linux_<arch>/freeze`, and a digest pin file for it.
struct Release {
    dir: tempfile::TempDir,
    stem: String,
}

impl Release {
    fn new(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("a scratch release tree");
        let version = pinned_version();
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            other => other,
        };
        let stem = format!("freeze_{version}_Linux_{arch}");
        let staging = dir.path().join(&stem);
        std::fs::create_dir_all(&staging).expect("the archive's directory");
        std::fs::write(staging.join("freeze"), body).expect("the stand-in renderer");

        let versioned = dir.path().join(format!("v{version}"));
        std::fs::create_dir_all(&versioned).expect("the release directory");
        let archive = versioned.join(format!("{stem}.tar.gz"));
        let tar = Command::new("tar")
            .args(["-czf", archive.to_str().expect("a UTF-8 path"), "-C"])
            .arg(dir.path())
            .arg(&stem)
            .output()
            .expect("tar runs");
        assert!(
            tar.status.success(),
            "{}",
            String::from_utf8_lossy(&tar.stderr)
        );
        Self { dir, stem }
    }

    /// Replace the archive's bytes, keeping its name — for the paths after the
    /// digest check.
    fn replace_archive(&self, bytes: &[u8]) {
        std::fs::write(self.archive(), bytes).expect("the archive is replaced");
    }

    fn base_url(&self) -> String {
        format!("file://{}", self.dir.path().display())
    }

    fn archive(&self) -> PathBuf {
        self.dir
            .path()
            .join(format!("v{}", pinned_version()))
            .join(format!("{}.tar.gz", self.stem))
    }

    /// A digest pin file naming this archive's real digest, or `digest` when one
    /// is given — which is how a tampered download is staged.
    fn sums(&self, digest: Option<&str>) -> PathBuf {
        let actual = sha256(&self.archive());
        let path = self.dir.path().join("freeze.sha256");
        std::fs::write(
            &path,
            format!("{}  {}.tar.gz\n", digest.unwrap_or(&actual), self.stem),
        )
        .expect("the pin file");
        path
    }
}

/// The version `install-freeze.sh` pins, read from the script itself — the one
/// place it is stated.
fn pinned_version() -> String {
    let script = std::fs::read_to_string(script("install-freeze.sh")).expect("the installer");
    script
        .lines()
        .find_map(|line| {
            line.strip_prefix("freeze_version=\"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .expect("screenshots/install-freeze.sh no longer states freeze_version")
        .to_owned()
}

fn sha256(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum runs");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("a digest")
        .to_owned()
}

#[test]
fn the_installer_puts_the_pinned_renderer_where_it_was_asked_to() {
    let release = Release::new("#!/usr/bin/env bash\necho stand-in freeze\n");
    let sums = release.sums(None);
    let into = tempfile::tempdir().expect("an install directory");
    let bin = into.path().join("bin");

    let installed = run(
        "install-freeze.sh",
        &[],
        &[
            ("FREEZE_BASE_URL", &release.base_url()),
            ("FREEZE_INSTALL_DIR", bin.to_str().expect("a UTF-8 path")),
            ("FREEZE_SHA256_FILE", sums.to_str().expect("a UTF-8 path")),
        ],
        into.path(),
        "",
    );

    assert!(installed.status.success(), "{}", stderr(&installed));
    assert!(bin.join("freeze").is_file(), "no renderer was installed");
    assert!(
        stderr(&installed).contains(&pinned_version()),
        "the installer did not say which version it installed: {}",
        stderr(&installed)
    );
    let ran = Command::new(bin.join("freeze"))
        .output()
        .expect("the installed renderer runs");
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout).trim(),
        "stand-in freeze"
    );
}

#[test]
fn the_installer_refuses_an_archive_that_does_not_match_its_pinned_digest() {
    let release = Release::new("#!/usr/bin/env bash\necho tampered\n");
    let sums = release.sums(Some(&"0".repeat(64)));
    let into = tempfile::tempdir().expect("an install directory");
    let bin = into.path().join("bin");

    let refused = run(
        "install-freeze.sh",
        &[],
        &[
            ("FREEZE_BASE_URL", &release.base_url()),
            ("FREEZE_INSTALL_DIR", bin.to_str().expect("a UTF-8 path")),
            ("FREEZE_SHA256_FILE", sums.to_str().expect("a UTF-8 path")),
        ],
        into.path(),
        "",
    );

    assert_eq!(
        refused.status.code(),
        Some(1),
        "a tampered archive installed"
    );
    assert!(
        stderr(&refused).contains("sha256 mismatch"),
        "{}",
        stderr(&refused)
    );
    assert!(
        !bin.join("freeze").exists(),
        "the renderer was installed despite the mismatch"
    );
}

#[test]
fn the_installer_refuses_a_pin_file_that_names_no_archive() {
    let release = Release::new("#!/usr/bin/env bash\n");
    let empty = release.dir.path().join("empty.sha256");
    std::fs::write(&empty, "# no lines\n").expect("an empty pin file");
    let into = tempfile::tempdir().expect("an install directory");

    let refused = run(
        "install-freeze.sh",
        &[],
        &[
            ("FREEZE_BASE_URL", &release.base_url()),
            (
                "FREEZE_INSTALL_DIR",
                into.path().to_str().expect("a UTF-8 path"),
            ),
            ("FREEZE_SHA256_FILE", empty.to_str().expect("a UTF-8 path")),
        ],
        into.path(),
        "",
    );

    assert_eq!(
        refused.status.code(),
        Some(1),
        "an unpinned archive installed"
    );
    assert!(
        stderr(&refused).contains("no pinned sha256"),
        "{}",
        stderr(&refused)
    );
    assert!(!into.path().join("freeze").exists());
}

#[test]
fn the_installer_refuses_a_base_url_and_a_destination_it_cannot_trust() {
    let into = tempfile::tempdir().expect("an install directory");
    for (key, value, what) in [
        (
            "FREEZE_BASE_URL",
            "ftp://example.invalid",
            "https:// or file://",
        ),
        ("FREEZE_INSTALL_DIR", "--help", "must name a directory"),
        (
            "FREEZE_SHA256_FILE",
            "/nonexistent/freeze.sha256",
            "digest pin file",
        ),
    ] {
        let refused = run("install-freeze.sh", &[], &[(key, value)], into.path(), "");
        assert_eq!(
            refused.status.code(),
            Some(1),
            "{key}={value} was accepted: {}",
            stderr(&refused)
        );
        assert!(
            stderr(&refused).contains(what) && stderr(&refused).contains(key),
            "the refusal of {key} does not name it and why: {}",
            stderr(&refused)
        );
    }
}

#[test]
fn blessing_refuses_when_there_is_no_capture_to_bless() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let refused = run(
        "bless-baseline.sh",
        &[],
        &[(
            "SHOTS_CURRENT",
            dir.path()
                .join("never-captured")
                .to_str()
                .expect("a UTF-8 path"),
        )],
        dir.path(),
        "",
    );

    assert_eq!(
        refused.status.code(),
        Some(1),
        "it blessed nothing into a baseline"
    );
    assert!(
        stderr(&refused).contains("no capture to bless"),
        "{}",
        stderr(&refused)
    );
    assert!(
        stderr(&refused).contains("just screenshots"),
        "the refusal does not say how to get one: {}",
        stderr(&refused)
    );
}

#[test]
fn blessing_refuses_without_the_tool_that_writes_the_baseline() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let current = dir.path().join("current");
    std::fs::create_dir_all(&current).expect("a capture root");

    // A PATH with the shell's own tools and no screencomp.
    let refused = run(
        "bless-baseline.sh",
        &[],
        &[
            ("PATH", "/usr/bin:/bin"),
            ("SHOTS_CURRENT", current.to_str().expect("a UTF-8 path")),
        ],
        dir.path(),
        "",
    );

    assert_eq!(
        refused.status.code(),
        Some(1),
        "it blessed without screencomp"
    );
    assert!(
        stderr(&refused).contains("screencomp is not installed"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn the_lane_name_is_the_same_for_every_spelling_of_one_architecture() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a stand-in bin");

    // `uname -m` is the only input, and the lane name is what `screencomp.toml`
    // declares and `shots/baseline/<arch>.json` is named for — so the two
    // spellings of each architecture must not name two lanes.
    for (reported, lane) in [
        ("x86_64", "x86_64"),
        ("amd64", "x86_64"),
        ("aarch64", "arm64"),
        ("arm64", "arm64"),
        ("riscv64", "riscv64"),
    ] {
        std::fs::write(
            bin.join("uname"),
            format!("#!/usr/bin/env bash\nprintf '{reported}\\n'\n"),
        )
        .expect("the stand-in uname");
        let mut mode = std::fs::metadata(bin.join("uname"))
            .expect("the stand-in is there")
            .permissions();
        {
            use std::os::unix::fs::PermissionsExt as _;
            mode.set_mode(0o755);
        }
        std::fs::set_permissions(bin.join("uname"), mode).expect("it is executable");

        let named = run(
            "host-arch.sh",
            &[],
            &[(
                "PATH",
                &format!("{}:/usr/bin:/bin", bin.to_str().expect("a UTF-8 path")),
            )],
            dir.path(),
            "",
        );
        assert!(named.status.success(), "{}", stderr(&named));
        assert_eq!(
            stdout(&named).trim(),
            lane,
            "a host reporting {reported} was given the wrong lane"
        );
    }
}

/// The three overrides that point the installer at a stand-in release tree.
fn installing<'a>(release: &'a Release, sums: &'a Path, into: &'a Path) -> Vec<(&'a str, String)> {
    vec![
        ("FREEZE_BASE_URL", release.base_url()),
        (
            "FREEZE_INSTALL_DIR",
            into.to_str().expect("a UTF-8 path").to_owned(),
        ),
        (
            "FREEZE_SHA256_FILE",
            sums.to_str().expect("a UTF-8 path").to_owned(),
        ),
    ]
}

fn install(env: &[(&str, String)], cwd: &Path) -> Output {
    let borrowed: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    run("install-freeze.sh", &[], &borrowed, cwd, "")
}

#[test]
fn the_installer_refuses_a_release_that_has_no_archive_to_download() {
    let release = Release::new("#!/usr/bin/env bash\n");
    let sums = release.sums(None);
    std::fs::remove_file(release.archive()).expect("the archive is taken away");
    let into = tempfile::tempdir().expect("an install directory");

    let refused = install(&installing(&release, &sums, into.path()), into.path());

    assert_eq!(
        refused.status.code(),
        Some(1),
        "a missing archive installed"
    );
    assert!(
        stderr(&refused).contains("could not download"),
        "{}",
        stderr(&refused)
    );
    assert!(
        stderr(&refused).contains("install freeze from"),
        "the refusal does not say how to get one another way: {}",
        stderr(&refused)
    );
    assert!(!into.path().join("freeze").exists());
}

#[test]
fn the_installer_refuses_an_archive_that_matches_its_pin_but_does_not_unpack() {
    let release = Release::new("#!/usr/bin/env bash\n");
    release.replace_archive(b"not a gzip stream at all");
    // Pinned to what is really there, so the digest check passes and the failure
    // is the one after it.
    let sums = release.sums(None);
    let into = tempfile::tempdir().expect("an install directory");

    let refused = install(&installing(&release, &sums, into.path()), into.path());

    assert_eq!(
        refused.status.code(),
        Some(1),
        "an unreadable archive installed"
    );
    assert!(
        stderr(&refused).contains("did not\n                unpack"),
        "{}",
        stderr(&refused)
    );
    assert!(!into.path().join("freeze").exists());
}

#[test]
fn the_installer_refuses_an_archive_that_carries_no_renderer() {
    let release = Release::new("#!/usr/bin/env bash\n");
    // A well-formed archive of the right name holding something else entirely.
    let elsewhere = tempfile::tempdir().expect("a scratch tree");
    std::fs::write(elsewhere.path().join("README"), "no freeze here\n").expect("a decoy");
    let tar = Command::new("tar")
        .args([
            "-czf",
            release.archive().to_str().expect("a UTF-8 path"),
            "-C",
        ])
        .arg(elsewhere.path())
        .arg("README")
        .output()
        .expect("tar runs");
    assert!(tar.status.success());
    let sums = release.sums(None);
    let into = tempfile::tempdir().expect("an install directory");

    let refused = install(&installing(&release, &sums, into.path()), into.path());

    assert_eq!(refused.status.code(), Some(1), "an empty archive installed");
    assert!(
        stderr(&refused).contains("upstream changed the archive layout"),
        "{}",
        stderr(&refused)
    );
    assert!(!into.path().join("freeze").exists());
}

#[test]
fn the_installer_refuses_a_destination_it_cannot_write() {
    let release = Release::new("#!/usr/bin/env bash\n");
    let sums = release.sums(None);
    let into = tempfile::tempdir().expect("an install directory");
    let readonly = into.path().join("readonly");
    std::fs::create_dir(&readonly).expect("a directory");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut mode = std::fs::metadata(&readonly)
            .expect("it is there")
            .permissions();
        mode.set_mode(0o500);
        std::fs::set_permissions(&readonly, mode).expect("it is made read-only");
    }

    let refused = install(
        &installing(&release, &sums, &readonly.join("bin")),
        into.path(),
    );

    assert_eq!(
        refused.status.code(),
        Some(1),
        "it installed into a read-only tree"
    );
    assert!(
        stderr(&refused).contains("could not install into"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn the_installer_refuses_an_architecture_it_pins_no_build_for() {
    let release = Release::new("#!/usr/bin/env bash\n");
    let sums = release.sums(None);
    let into = tempfile::tempdir().expect("an install directory");
    let bin = into.path().join("stand-ins");
    std::fs::create_dir_all(&bin).expect("a stand-in bin");
    std::fs::write(
        bin.join("uname"),
        "#!/usr/bin/env bash\nprintf 's390x\\n'\n",
    )
    .expect("a stand-in uname");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut mode = std::fs::metadata(bin.join("uname"))
            .expect("it is there")
            .permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(bin.join("uname"), mode).expect("it is executable");
    }

    let mut env = installing(&release, &sums, into.path());
    env.push((
        "PATH",
        format!("{}:/usr/bin:/bin", bin.to_str().expect("a UTF-8 path")),
    ));
    let refused = install(&env, into.path());

    assert_eq!(
        refused.status.code(),
        Some(1),
        "an unpinned architecture installed"
    );
    assert!(stderr(&refused).contains("s390x"), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("Linux x86_64 and arm64 only"),
        "the refusal does not say what is pinned: {}",
        stderr(&refused)
    );
}

#[test]
fn the_stager_refuses_a_directory_it_cannot_create_or_write() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).expect("a directory");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut mode = std::fs::metadata(&readonly)
            .expect("it is there")
            .permissions();
        mode.set_mode(0o500);
        std::fs::set_permissions(&readonly, mode).expect("it is made read-only");
    }

    let refused = run(
        "stage-fixture.sh",
        &[readonly.join("fixture").to_str().expect("a UTF-8 path")],
        &[],
        dir.path(),
        "",
    );
    assert_eq!(
        refused.status.code(),
        Some(1),
        "it staged into a read-only tree"
    );
    assert!(
        stderr(&refused).contains("could not make the fixture directory"),
        "{}",
        stderr(&refused)
    );

    // And when the directory is there but its configuration cannot be written.
    let refused = run(
        "stage-fixture.sh",
        &[readonly.to_str().expect("a UTF-8 path")],
        &[],
        dir.path(),
        "",
    );
    assert_eq!(
        refused.status.code(),
        Some(1),
        "it wrote into a read-only directory"
    );
    assert!(
        stderr(&refused).contains("could not write the configuration"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn blessing_refuses_a_capture_root_that_reads_as_an_option() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let refused = run(
        "bless-baseline.sh",
        &[],
        &[("SHOTS_CURRENT", "--input")],
        dir.path(),
        "",
    );

    assert_eq!(
        refused.status.code(),
        Some(1),
        "an option was blessed as a capture"
    );
    assert!(
        stderr(&refused).contains("SHOTS_CURRENT"),
        "{}",
        stderr(&refused)
    );
}
