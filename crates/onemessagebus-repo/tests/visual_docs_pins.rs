//! The versions `.github/workflows/visual-docs.yml` restates, held to their sources.
//!
//! The screenshot capture (`screenshots/capture.sh`) drives the real binary, so
//! its CI lane builds in a pinned Rust container. That image tag and the
//! `RUSTUP_TOOLCHAIN` the capture exports — which bypasses `rust-toolchain.toml`
//! rather than syncing the five cross-compilation targets it also declares — are
//! both copies of the channel that file names, and a bump editing only one would
//! capture under a compiler nobody declared.
//!
//! screencomp's own version is likewise stated twice in that file, and only one
//! of the two is reconciled elsewhere: `screencomp doctor --env` reads the
//! reusable workflow's `uses:` ref and compares it with the installed CLI, and
//! says nothing about the `screencomp-version:` input beside it. This holds the
//! two to each other, so the pair moves together and `doctor --env` then covers
//! both.
//!
//! The renderer's own version needs no gate of this kind:
//! `screenshots/install-freeze.sh` is the single place it is stated, and both
//! `just screenshots-tools` and the workflow's capture step run that script.
//! The capture lane is likewise single-sourced — `[capture].arches` in
//! `screencomp.toml`, which the guard reads through `screencomp arches`.

use toml_edit::DocumentMut;

const RUST_TOOLCHAIN: &str = include_str!("../../../rust-toolchain.toml");
const VISUAL_DOCS: &str = include_str!("../../../.github/workflows/visual-docs.yml");

/// The string at `path` in a TOML document, refused by name when it is absent.
fn value(document: &str, origin: &str, path: &[&str]) -> String {
    let parsed: DocumentMut = document
        .parse()
        .unwrap_or_else(|failure| panic!("{origin} is not TOML: {failure}"));
    let mut item = parsed.as_item();
    for key in path {
        item = item
            .get(key)
            .unwrap_or_else(|| panic!("{origin} has no {}", path.join(".")));
    }
    item.as_str()
        .unwrap_or_else(|| panic!("{origin}'s {} is not a string", path.join(".")))
        .to_owned()
}

#[test]
fn the_capture_builds_under_the_channel_rust_toolchain_declares() {
    let channel = value(
        RUST_TOOLCHAIN,
        "rust-toolchain.toml",
        &["toolchain", "channel"],
    );

    let container = format!("container: rust:{channel}-bookworm");
    assert!(
        VISUAL_DOCS.contains(&container),
        ".github/workflows/visual-docs.yml does not name `{container}`, so its capture \
         container has parted from rust-toolchain.toml's channel ({channel}); set the \
         image tag to that channel"
    );

    let toolchain = format!("export RUSTUP_TOOLCHAIN={channel}");
    assert!(
        VISUAL_DOCS.contains(&toolchain),
        ".github/workflows/visual-docs.yml does not name `{toolchain}`, so its capture \
         command has parted from rust-toolchain.toml's channel ({channel}); set it to \
         that channel"
    );
}

#[test]
fn the_two_screencomp_pins_name_one_release() {
    let reference = VISUAL_DOCS
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("uses: nickderobertis/screencomp/.github/workflows/")
                .and_then(|rest| rest.split_once('@'))
                .map(|(_, tag)| tag.trim().to_owned())
        })
        .expect(
            ".github/workflows/visual-docs.yml calls no \
             nickderobertis/screencomp/.github/workflows/… reusable workflow; the visual-docs \
             gate is screencomp's to run",
        );

    let input = VISUAL_DOCS
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("screencomp-version:")
                .map(|rest| rest.trim().to_owned())
        })
        .expect(
            ".github/workflows/visual-docs.yml passes no `screencomp-version:`, so the CLI the \
             reusable workflow installs is whatever `latest` resolves to rather than the release \
             its own steps came from",
        );

    assert_eq!(
        input, reference,
        "visual-docs.yml pins the reusable workflow at {reference} but installs the \
         {input} CLI; `screencomp doctor --env` reads only the `uses:` ref, so nothing else \
         catches this — set both to the same release"
    );
}
