//! `scripts/path-lengths.sh`, the guard that keeps every tracked path short
//! enough for a Windows `cargo install --git` checkout, driven over real git
//! repositories: a throwaway one at the limit and one past it, and this one.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The longest tracked path the script accepts.
const LIMIT: usize = 150;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

/// Run the guard from `cwd`, the way `just path-lengths` does.
fn guard(cwd: &Path) -> Output {
    Command::new("bash")
        .arg(repo_root().join("scripts/path-lengths.sh"))
        .current_dir(cwd)
        .output()
        .expect("the script runs")
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

/// A repository tracking one file whose path is exactly `length` characters,
/// nested the way a fixture is.
fn repository_tracking(length: usize) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    git(dir.path(), &["init", "--quiet"]);
    let prefix = "crates/some-crate/tests/fixtures/";
    let suffix = ".pass.json";
    let path = format!(
        "{prefix}{}{suffix}",
        "a".repeat(length - prefix.len() - suffix.len())
    );
    assert_eq!(path.len(), length);
    let file = dir.path().join(&path);
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("directories");
    std::fs::write(&file, "{}\n").expect("the file is written");
    git(dir.path(), &["add", "--all"]);
    (dir, path)
}

#[test]
fn a_path_at_the_limit_passes_quietly() {
    let (dir, _) = repository_tracking(LIMIT);
    let ran = guard(dir.path());
    assert_eq!(
        ran.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(ran.stdout.is_empty() && ran.stderr.is_empty());
}

#[test]
fn a_path_past_the_limit_is_refused_by_name_and_length() {
    let (dir, path) = repository_tracking(LIMIT + 1);
    // Checked from a subdirectory: the guard covers the whole checkout.
    let ran = guard(&dir.path().join("crates"));
    assert_eq!(ran.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&ran.stderr);
    assert!(
        stderr.contains(&format!("{}\t{path}", LIMIT + 1)),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("longer than {LIMIT}")), "{stderr}");
    assert!(stderr.contains("ACTION:"), "{stderr}");
}

#[test]
fn a_directory_outside_any_checkout_is_refused_as_input() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let ran = Command::new("bash")
        .arg(repo_root().join("scripts/path-lengths.sh"))
        .current_dir(dir.path())
        .env(
            "GIT_CEILING_DIRECTORIES",
            dir.path().parent().expect("a parent"),
        )
        .output()
        .expect("the script runs");
    assert_eq!(ran.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&ran.stderr).contains("not inside a git checkout"));
}

#[test]
fn this_repository_has_no_path_too_long_for_a_windows_checkout() {
    let ran = guard(&repo_root());
    assert_eq!(
        ran.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
}
