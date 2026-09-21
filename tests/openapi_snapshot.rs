//! Guards the published contract. A generated C# client is built from `openapi.json`, so an
//! unintended change here is a downstream break — this turns it into a failing test instead.
//!
//! Regenerate deliberately with:  UPDATE_OPENAPI=1 cargo test

use std::path::PathBuf;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json")
}

fn current() -> String {
    let doc = fff_server::openapi_document();
    let mut json = serde_json::to_string_pretty(&doc).expect("openapi document serialises");
    json.push('\n');
    json
}

#[test]
fn openapi_matches_committed_snapshot() {
    let path = snapshot_path();
    let current = current();

    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &current).expect("write snapshot");
        eprintln!("updated {}", path.display());
        return;
    }

    let committed = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => panic!(
            "cannot read {}: {e}\nrun `UPDATE_OPENAPI=1 cargo test` to create it",
            path.display()
        ),
    };

    assert_eq!(
        committed.replace("\r\n", "\n"),
        current.replace("\r\n", "\n"),
        "the OpenAPI contract changed.\n\
         If that was intended, run `UPDATE_OPENAPI=1 cargo test` and commit openapi.json \
         so the change is reviewed rather than discovered at runtime."
    );
}

#[test]
fn contract_declares_the_health_route() {
    let doc = fff_server::openapi_document();
    assert!(
        doc.paths.paths.contains_key("/v1/health"),
        "expected /v1/health in the contract, found: {:?}",
        doc.paths.paths.keys().collect::<Vec<_>>()
    );
}

#[test]
fn problem_schema_is_published_for_clients() {
    let doc = fff_server::openapi_document();
    let components = doc.components.expect("components present");
    assert!(
        components.schemas.contains_key("Problem"),
        "RFC 9457 Problem must be in the contract so clients can deserialise errors"
    );
}

/// The unions must stay *properly* discriminated, because that is what makes them
/// generatable at all.
///
/// utoipa emits an OpenAPI `discriminator` only for an enum whose variants are newtypes over
/// named schemas. Inline variants produce an anonymous `oneOf` that no generator can map a
/// discriminator onto - Kiota warned on two of these and refused outright on the third. This
/// asserts the whole shape: a `oneOf` of `$ref`s, a discriminator naming `type`, a mapping
/// covering every variant, and a `type` property present on each referenced schema, which the
/// OpenAPI discriminator object requires.
///
/// `LocationDto` is deliberately not in this list: it is the only union that appears as an
/// optional field, and nesting a discriminated union inside `oneOf: [null, $ref]` makes Kiota
/// lose the inheritance relationship. It stays a flat object instead.
#[test]
fn unions_are_properly_discriminated() {
    let doc = fff_server::openapi_document();
    let json = serde_json::to_value(&doc).expect("serialise");
    let schemas = &json["components"]["schemas"];

    for name in ["ConstraintDto", "MixedHit"] {
        let schema = &schemas[name];

        let variants = schema["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} must be a oneOf of variant refs"));
        assert!(!variants.is_empty(), "{name} has no variants");

        let refs: Vec<&str> = variants
            .iter()
            .map(|v| {
                v["$ref"].as_str().unwrap_or_else(|| {
                    panic!("{name} variant is inline, not a $ref; a discriminator cannot map to it")
                })
            })
            .collect();

        let discriminator = &schema["discriminator"];
        assert_eq!(
            discriminator["propertyName"].as_str(),
            Some("type"),
            "{name} needs a discriminator on `type`"
        );

        let mapping = discriminator["mapping"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} discriminator needs an explicit mapping"));
        assert_eq!(
            mapping.len(),
            refs.len(),
            "{name} mapping must cover every variant"
        );

        for (value, target) in mapping {
            let target = target.as_str().expect("mapping target is a ref string");
            assert!(
                refs.contains(&target),
                "{name} maps {value:?} to {target:?}, which is not one of its variants"
            );

            // Each referenced schema must itself declare the discriminator property.
            let variant_name = target.rsplit('/').next().unwrap();
            let variant = &schemas[variant_name];
            assert!(
                variant["properties"]["type"].is_object(),
                "{variant_name} must declare the `type` property the discriminator reads"
            );
            assert!(
                variant["required"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|f| f == "type")),
                "{variant_name} must require `type`; an optional discriminator is not usable"
            );
        }
    }
}

/// Generators reject a relative server url and then require the base address be wired by
/// hand, which is a papercut for every consumer.
#[test]
fn contract_declares_an_absolute_server_url() {
    let doc = fff_server::openapi_document();
    let json = serde_json::to_value(&doc).expect("serialise");
    let url = json["servers"][0]["url"]
        .as_str()
        .expect("a servers entry is required");
    assert!(
        url.starts_with("http://") || url.starts_with("https://"),
        "server url must be absolute, got {url:?}"
    );
}
