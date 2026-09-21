#![allow(dead_code)] // shared by several test binaries; each uses a subset

//! Shared harness. Requests go through the real `axum::Router`, so serialisation, the
//! extractors and the error mapping are all exercised - not just the handler bodies.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The committed fixture tree: known content, so assertions can be exact.
pub fn fixture_root() -> String {
    format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))
}

/// This repository itself. Always present wherever the tests run, and genuinely a git repo,
/// so git-status paths get exercised. Assertions against it must be structural, since its
/// contents change as work proceeds.
pub fn own_repo_root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

/// Every test gets its own database root. Tests run in parallel and would otherwise open the
/// same LMDB environment for the same workspace identity, which the engine rejects with
/// `DbInUse`.
fn unique_db_root() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("fff-server-it-{}-{n}", std::process::id()))
}

pub fn app() -> Router {
    let mut config = fff_server::config::Config::default();
    config.workspaces.db_root = unique_db_root();
    fff_server::build(config).0
}

pub async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(path);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&v).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };

    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "response was not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, value)
}

pub async fn post(router: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(router, "POST", path, Some(body)).await
}

pub async fn get(router: &Router, path: &str) -> (StatusCode, Value) {
    send(router, "GET", path, None).await
}

/// Creates a workspace over `root`, waiting for the content index so fuzzy grep is
/// dependable, and returns the router plus the workspace id.
pub async fn workspace_over(root: &str) -> (Router, String) {
    let router = app();
    let (status, body) = post(
        &router,
        "/v1/workspaces",
        json!({ "root": root, "waitFor": "indexing", "waitForIndexMs": 30000 }),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::ACCEPTED,
        "workspace creation failed: {status} {body}"
    );
    let id = body["id"].as_str().expect("workspace id").to_string();
    (router, id)
}

pub async fn fixtures() -> (Router, String) {
    workspace_over(&fixture_root()).await
}

/// Reads a line from a fixture file. 1-based, matching the engine's line numbers.
pub fn fixture_line(relative: &str, line_number: u64) -> String {
    let path =
        PathBuf::from(fixture_root()).join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    text.lines()
        .nth(line_number as usize - 1)
        .unwrap_or_else(|| panic!("{relative} has no line {line_number}"))
        .to_string()
}

/// UTF-16 code-unit index of `needle` within `line`.
pub fn utf16_index_of(line: &str, needle: &str) -> u32 {
    let byte = line.find(needle).expect("needle present in line");
    line[..byte].encode_utf16().count() as u32
}

/// UTF-8 byte index of `needle` within `line`.
pub fn byte_index_of(line: &str, needle: &str) -> usize {
    line.find(needle).expect("needle present in line")
}

/// Slices `line` by a UTF-16 range, the way a C# client indexing a `string` would.
pub fn slice_utf16(line: &str, start: u32, end: u32) -> String {
    let units: Vec<u16> = line.encode_utf16().collect();
    String::from_utf16(&units[start as usize..end as usize]).expect("valid utf16 slice")
}

pub fn paths_of(items: &Value) -> Vec<String> {
    items
        .as_array()
        .expect("items array")
        .iter()
        .map(|h| h["item"]["relativePath"].as_str().unwrap().to_string())
        .collect()
}
