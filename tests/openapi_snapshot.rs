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
