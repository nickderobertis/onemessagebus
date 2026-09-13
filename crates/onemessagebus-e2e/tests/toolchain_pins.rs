//! The toolchain facts restated outside `Cargo.toml`, held to it.
//!
//! clippy reads its MSRV from `clippy.toml` and rustfmt its edition from
//! `rustfmt.toml` whenever either runs outside cargo — an editor, a bare
//! `rustfmt` — so each file is a second copy of a `[workspace.package]` value. A
//! bump that edits only one copy leaves clippy suggesting APIs the declared floor
//! does not have, or formats under an edition the crates do not compile as, and
//! nothing else in the gate would notice.

use toml_edit::DocumentMut;

const CARGO: &str = include_str!("../../../Cargo.toml");
const CLIPPY: &str = include_str!("../../../clippy.toml");
const RUSTFMT: &str = include_str!("../../../rustfmt.toml");

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
fn clippy_holds_the_msrv_cargo_declares() {
    let declared = value(CARGO, "Cargo.toml", &["workspace", "package", "rust-version"]);
    let clippy = value(CLIPPY, "clippy.toml", &["msrv"]);
    assert_eq!(
        clippy, declared,
        "clippy.toml's msrv is {clippy} but Cargo.toml's [workspace.package] rust-version is {declared}; set both to the same floor"
    );
}

#[test]
fn rustfmt_formats_under_the_edition_cargo_declares() {
    let declared = value(CARGO, "Cargo.toml", &["workspace", "package", "edition"]);
    let rustfmt = value(RUSTFMT, "rustfmt.toml", &["edition"]);
    assert_eq!(
        rustfmt, declared,
        "rustfmt.toml's edition is {rustfmt} but Cargo.toml's [workspace.package] edition is {declared}; set both to the same edition"
    );
}
