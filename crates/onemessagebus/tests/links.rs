//! Schema links (Contract L): the bundle document and what it refuses, the
//! link grammar, the pin rule, and a configuration naming links that loads
//! without resolving any of them.
//!
//! The cache and the transport are proven against the compiled binary, in the
//! journeys' `links` module; what is here needs no network at all.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use onemessagebus::{
    BundleVersion, Config, ConfigError, Freshness, LinkError, LinkLocation, LinkResolver, Outcome,
    Registry, SchemaBundle, SchemaLink,
};
use serde_json::{json, Value};

fn bundle(value: Value) -> Result<SchemaBundle, String> {
    SchemaBundle::from_json(&value.to_string()).map_err(|failure| failure.to_string())
}

fn frame(version: &str) -> Value {
    json!({
        "version": version,
        "description": "frames",
        "schemas": [{"id": "agent.example-frame.hello@8", "schema": {"type": "object", "required": ["hello"]}}]
    })
}

#[test]
fn a_bundle_reads_the_documented_shape_and_round_trips() {
    let read = bundle(frame("8.1")).expect("the documented shape reads");
    assert_eq!(read.version().to_string(), "8.1");
    assert_eq!(read.description(), Some("frames"));
    assert_eq!(read.schemas().len(), 1);
    assert_eq!(
        read.schemas()[0].id.to_string(),
        "agent.example-frame.hello@8"
    );
    let written = serde_json::to_value(&read).expect("it serializes");
    assert_eq!(written, frame("8.1"));
    let again: SchemaBundle = serde_json::from_value(written).expect("it reads back");
    assert_eq!(again, read);
    let bare = bundle(json!({"version": "8", "schemas": [{"id": "a.b@1", "schema": {}}]}))
        .expect("description is optional");
    assert_eq!(bare.description(), None);
    assert!(
        !serde_json::to_string(&bare)
            .expect("json")
            .contains("description"),
        "an absent description is omitted"
    );
    let mut registry = Registry::new();
    read.register_into(&mut registry).expect("it registers");
    assert!(registry
        .check(
            &"agent.example-frame.hello@8".parse().expect("id"),
            &json!({})
        )
        .is_err());
    let schema = serde_json::to_value(schemars::schema_for!(SchemaBundle)).expect("a schema");
    assert!(
        schema
            .to_string()
            .contains(r#""pattern":"^[0-9]+(\\.[0-9]+){0,2}$""#),
        "the version's grammar is in the schema: {schema}"
    );
    assert_eq!(schema["properties"]["schemas"]["minItems"], json!(1));
    assert_eq!(schema["required"], json!(["version", "schemas"]));
}

#[test]
fn a_bundle_refuses_each_malformation_naming_it() {
    let cases = [
        (
            json!({"version": "8", "schemas": [{"id": "a.b@1", "schema": {}}], "owner": "x"}),
            "`owner` is not a key of a bundle",
        ),
        (
            json!({"version": "8.x", "schemas": [{"id": "a.b@1", "schema": {}}]}),
            "version: \"8.x\" is not a version",
        ),
        (
            json!({"version": "8.1.2.3", "schemas": [{"id": "a.b@1", "schema": {}}]}),
            "version: \"8.1.2.3\" is not a version",
        ),
        (
            json!({"version": 8, "schemas": [{"id": "a.b@1", "schema": {}}]}),
            "version: is not a string",
        ),
        (
            json!({"schemas": [{"id": "a.b@1", "schema": {}}]}),
            "version: is missing",
        ),
        (json!({"version": "8", "schemas": []}), "schemas: is empty"),
        (json!({"version": "8"}), "schemas: is missing"),
        (
            json!({"version": "8", "schemas": [{"id": "a.b@1", "schema": {}}, {"id": "a.b@1", "schema": {"type": "string"}}]}),
            "schemas[1].id: `a.b@1` is already declared by schemas[0]",
        ),
        (
            json!({"version": "8", "schemas": [{"id": "a.b@1", "schema": {}}, {"id": "a.b@2"}]}),
            "schemas[1]: is not a registry document",
        ),
        (
            json!({"version": "8", "schemas": [{"id": "not an id", "schema": {}}]}),
            "schemas[0]: is not a registry document",
        ),
        (
            json!({"version": "8", "schemas": [{"id": "a.b@1", "schema": true}]}),
            "schemas[0].schema: is not a JSON Schema object",
        ),
        (json!(["a list"]), "the document is not a JSON object"),
        (
            json!({"version": "8", "description": 3, "schemas": [{"id": "a.b@1", "schema": {}}]}),
            "description: is not a string",
        ),
    ];
    for (document, named) in cases {
        let refused = bundle(document.clone()).expect_err("a malformed bundle is refused");
        assert!(refused.contains(named), "{document}: {refused}");
    }
    let not_json = SchemaBundle::from_json("{").expect_err("not JSON");
    assert!(not_json.to_string().contains("is not JSON"), "{not_json}");
}

fn link(text: &str) -> SchemaLink {
    SchemaLink::parse(text).unwrap_or_else(|failure| panic!("{text}: {failure}"))
}

fn refused(text: &str) -> String {
    let failure = SchemaLink::parse(text).expect_err("the link is refused");
    let message = failure.to_string();
    assert!(
        message.contains(&format!("{text:?}")),
        "the refusal names the link: {message}"
    );
    message
}

#[test]
fn a_link_parses_every_location_and_pin_contract_l_names() {
    let pinned = link("https://example.org/frames.json@8");
    assert_eq!(
        remote(pinned.location()),
        Some("https://example.org/frames.json")
    );
    assert_eq!(pinned.pin().map(ToString::to_string).as_deref(), Some("8"));
    assert_eq!(pinned.to_string(), "https://example.org/frames.json@8");

    let unpinned = link("https://example.org/frames.json");
    assert_eq!(unpinned.pin(), None);

    let deep = link("https://example.org/v1/frames.json@8.1.2");
    assert_eq!(
        deep.pin().map(ToString::to_string).as_deref(),
        Some("8.1.2")
    );

    // An `@` inside a path segment, or after the last `/` without a pin after
    // it, is part of the location.
    let in_segment = link("https://example.org/@scope/frames.json");
    assert_eq!(in_segment.pin(), None);
    assert_eq!(
        remote(in_segment.location()),
        Some("https://example.org/@scope/frames.json")
    );
    let segment_then_pin = link("https://example.org/pkg@2/frames.json@3");
    assert_eq!(
        segment_then_pin.pin().map(ToString::to_string).as_deref(),
        Some("3")
    );
    assert_eq!(
        remote(segment_then_pin.location()),
        Some("https://example.org/pkg@2/frames.json")
    );
    let not_a_pin = link("https://example.org/frames@latest");
    assert_eq!(not_a_pin.pin(), None);
    let user = link("https://token@example.org/frames.json");
    assert_eq!(user.pin(), None);

    for host in ["localhost", "127.0.0.1", "[::1]", "LOCALHOST"] {
        let loopback = link(&format!("http://{host}:8123/frames.json@8"));
        assert!(matches!(loopback.location(), LinkLocation::Remote(_)));
    }
    for host in ["example.org", "10.0.0.1", "localhost.example.org", "[::2]"] {
        let message = refused(&format!("http://{host}/frames.json@8"));
        assert!(message.contains("only for a loopback host"), "{message}");
    }

    let root = std::env::temp_dir().join("frames.json");
    let file_url = link(&format!("file://{}@8", url_path(&root)));
    assert_eq!(file_url.location(), &LinkLocation::File(root.clone()));
    assert_eq!(
        file_url.pin().map(ToString::to_string).as_deref(),
        Some("8")
    );
    let absolute = link(&format!("{}@8.1", root.display()));
    assert_eq!(absolute.location(), &LinkLocation::File(root.clone()));
    let relative = link("schemas/frames.json@8");
    assert_eq!(
        relative.location(),
        &LinkLocation::File(PathBuf::from("schemas/frames.json"))
    );
    let base = std::env::temp_dir();
    let rebased = relative.rebased(&base);
    assert_eq!(
        rebased.location(),
        &LinkLocation::File(base.join("schemas/frames.json"))
    );
    assert_eq!(rebased.pin().map(ToString::to_string).as_deref(), Some("8"));
    // An absolute path and a URL are not moved by a base.
    assert_eq!(absolute.clone().rebased(Path::new("/elsewhere")), absolute);
    assert_eq!(pinned.clone().rebased(Path::new("/elsewhere")), pinned);

    assert!(refused("file://relative/frames.json").contains("absolute path"));
    assert!(refused("ftp://example.org/frames.json").contains("scheme"));
    assert!(refused("https:///frames.json").contains("no host"));
    assert!(refused("").contains("empty"));
    assert!(refused("@8").contains("no location"));
    assert!(refused(" https://example.org/frames.json").contains("whitespace"));
}

/// The URL a remote location fetches, or `None` for a file.
fn remote(location: &LinkLocation) -> Option<&str> {
    match location {
        LinkLocation::Remote(url) => Some(url.as_str()),
        LinkLocation::File(_) => None,
    }
}

/// An absolute path as a `file://` URL's path.
fn url_path(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    }
}

fn version(text: &str) -> BundleVersion {
    text.parse()
        .unwrap_or_else(|failure| panic!("{text}: {failure}"))
}

#[test]
fn the_pin_is_a_prefix_range_with_missing_components_read_as_zero() {
    let cases = [
        ("8", "8", true),
        ("8", "8.1", true),
        ("8", "8.1.2", true),
        ("8", "9", false),
        ("8", "80", false),
        ("8.1", "8.1", true),
        ("8.1", "8.1.7", true),
        ("8.1", "8", false),
        ("8.1", "8.2", false),
        ("8.0", "8", true),
        ("8.0.0", "8", true),
        ("8.1.2", "8.1.2", true),
        ("8.1.2", "8.1.3", false),
        ("8.1.2", "8.1", false),
    ];
    for (pin, declared, admitted) in cases {
        assert_eq!(
            version(pin).admits(&version(declared)),
            admitted,
            "@{pin} against {declared}"
        );
    }
    assert!(version("8.2") > version("8.1.9"));
    assert!(
        version("8.1") > version("8"),
        "the more specific spelling is newer"
    );
}

#[test]
fn a_file_link_is_read_on_every_resolution_and_held_to_its_pin() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("frames.json");
    std::fs::write(&path, frame("8.1").to_string()).expect("written");
    let cache = dir.path().join("cache");
    let resolver =
        LinkResolver::new(Some(cache.clone())).with_ttl(std::time::Duration::from_secs(3600));

    let pinned = link(&format!("{}@8", path.display()));
    let first = resolver
        .resolve(&pinned, Freshness::Window)
        .expect("it resolves");
    assert_eq!(first.outcome, Outcome::Read);
    assert_eq!(first.bundle.version().to_string(), "8.1");

    std::fs::write(&path, frame("8.2").to_string()).expect("rewritten");
    let second = resolver
        .resolve(&pinned, Freshness::Window)
        .expect("it resolves");
    assert_eq!(
        second.bundle.version().to_string(),
        "8.2",
        "read again, not cached"
    );
    assert!(!cache.exists(), "a file link is never cached");

    std::fs::write(&path, frame("9").to_string()).expect("rewritten");
    let failure = resolver
        .resolve(&pinned, Freshness::Window)
        .expect_err("9 does not satisfy @8");
    let message = failure.to_string();
    assert!(matches!(failure, LinkError::PinUnsatisfied { .. }));
    for named in [pinned.to_string().as_str(), "@8", "version 9"] {
        assert!(message.contains(named), "{message} does not name {named}");
    }

    let missing = link(&format!("{}@8", dir.path().join("absent.json").display()));
    let unread = resolver
        .resolve(&missing, Freshness::Window)
        .expect_err("an absent file does not resolve");
    assert!(
        unread.to_string().contains("cannot read the bundle"),
        "{unread}"
    );

    std::fs::write(&path, "{\"version\": \"8\"}").expect("rewritten");
    let not_a_bundle = resolver
        .resolve(&pinned, Freshness::Window)
        .expect_err("not a bundle");
    assert!(
        not_a_bundle.to_string().contains("schemas: is missing"),
        "{not_a_bundle}"
    );
}

#[test]
fn load_parses_links_resolving_nothing_and_refuses_a_malformed_one_by_name() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    // A loopback listener nothing should ever connect to, and a link naming it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    listener.set_nonblocking(true).expect("non-blocking");
    let port = listener.local_addr().expect("an address").port();
    std::fs::write(dir.path().join("frames.json"), frame("8").to_string()).expect("written");
    let path = dir.path().join("onemessagebus.yaml");
    std::fs::write(
        &path,
        format!(
            "version: 1\ntransport: {{kind: memory}}\nschemas:\n  - \"http://127.0.0.1:{port}/frames.json@8\"\n  - \"https://schemas.example.invalid/frames.json@8.1\"\n  - \"frames.json@8\"\n"
        ),
    )
    .expect("written");
    let cache = dir.path().join("cache");
    let config = Config::load(&path).expect("links that are unreachable and uncached still load");
    assert_eq!(config.schemas.len(), 3);
    assert_eq!(
        config.schemas[2].location(),
        &LinkLocation::File(dir.path().join("frames.json")),
        "a relative bare path is resolved against the configuration's directory"
    );
    match listener.accept() {
        Err(failure) if failure.kind() == ErrorKind::WouldBlock => {}
        other => panic!("loading the configuration connected to a linked origin: {other:?}"),
    }

    // Resolving is the explicit call, and the relative link resolves from there.
    let only_file = Config {
        schemas: vec![config.schemas[2].clone()],
        ..config.clone()
    };
    let resolved = only_file
        .resolve_links(&LinkResolver::new(Some(cache.clone())), Freshness::Window)
        .expect("the file link resolves");
    assert_eq!(resolved[0].outcome, Outcome::Read);
    let written = serde_json::to_value(&only_file).expect("it serializes");
    assert_eq!(
        written["schemas"],
        json!([format!("{}@8", dir.path().join("frames.json").display())])
    );

    std::fs::write(
        &path,
        "version: 1\ntransport: {kind: memory}\nschemas:\n  - \"http://example.org/frames.json@8\"\n",
    )
    .expect("written");
    let failure = Config::load(&path).expect_err("a non-loopback http link is refused");
    assert!(matches!(failure, ConfigError::Parse { .. }), "{failure}");
    let message = failure.to_string();
    assert!(
        message.contains("schemas") && message.contains("http://example.org/frames.json@8"),
        "{message}"
    );
    let bare = Config::parse("version: 1\ntransport: {kind: memory}\n").expect("no links");
    assert!(bare.schemas.is_empty());
    assert!(!serde_json::to_string(&bare)
        .expect("json")
        .contains("schemas"));
}
