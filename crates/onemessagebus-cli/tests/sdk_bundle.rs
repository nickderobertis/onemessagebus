//! The SDK schema bundle both language packages are generated from, held to the
//! registry the binary builds: `examples/sdk_bundle.rs` prints exactly this
//! bundle, so what it carries is what the generated SDKs carry.

use onemessagebus::{sdk_schema, Open, Vocabulary as _};

#[test]
fn the_bundle_carries_every_message_the_binary_registers_over_the_open_vocabulary() {
    let registry = onemessagebus_cli::registry();
    let bundle = sdk_schema::bundle::<Open>(&registry);

    let registered: Vec<String> = registry.ids().iter().map(ToString::to_string).collect();
    let carried: Vec<String> = bundle.messages.keys().cloned().collect();
    assert_eq!(
        carried, registered,
        "the bundle's messages are not the binary's registry"
    );
    for (id, document) in &bundle.messages {
        let parsed = id.parse().expect("a schema id");
        assert_eq!(
            Some(document),
            registry.schema(&parsed),
            "{id} is carried with a document other than the one registered"
        );
    }
    assert!(
        carried.iter().all(|id| !id.starts_with("agent.")),
        "the bundle carries a product's schema: {carried:?}"
    );

    // The vocabulary the SDKs' envelope, filter and matcher roots are written
    // over is the one profile the binary links.
    assert_eq!(bundle.vocabulary.name, Open::NAME);
    assert!(bundle.vocabulary.reserved.is_empty());
    assert!(bundle.vocabulary.dimensions.is_empty());
    assert_eq!(bundle.vocabulary.default_source, Open::DEFAULT_SOURCE);
}
