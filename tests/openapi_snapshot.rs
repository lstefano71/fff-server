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

/// The shapes that broke C# generation, pinned so they cannot creep back.
///
/// utoipa renders a serde-tagged Rust enum as a `oneOf` of inline variants with no
/// discriminator. Kiota treats any `oneOf` as polymorphic and requires a discriminator whose
/// mapping points at named schemas, so it warned on three of ours and refused outright on the
/// one that was also wrapped in an `allOf`. These three are now flat objects with a closed
/// `type` enum instead.
#[test]
fn schemas_that_broke_codegen_stay_flat() {
    let doc = fff_server::openapi_document();
    let json = serde_json::to_value(&doc).expect("serialise");
    let schemas = &json["components"]["schemas"];

    for name in ["ConstraintDto", "LocationDto", "MixedHit"] {
        let schema = &schemas[name];
        assert!(
            !schema.as_object().unwrap().contains_key("oneOf"),
            "{name} became a oneOf again; C# generation fails or mis-deserialises on those"
        );
        assert!(
            !schema.as_object().unwrap().contains_key("allOf"),
            "{name} became an allOf again; Kiota cannot merge one over an anonymous oneOf"
        );
        assert_eq!(
            schema["type"].as_str(),
            Some("object"),
            "{name} should be a plain object"
        );
        assert!(
            schema["properties"]["type"].is_object(),
            "{name} needs its `type` discriminant as an ordinary property"
        );
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
