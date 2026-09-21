//! Structural assertions against this repository itself.
//!
//! Self-hosting keeps the suite portable — the target is always present wherever the tests
//! run, and it is genuinely a git repo — and it is a fair smoke test: if the server cannot
//! index its own source, it is broken.
//!
//! Because the target changes as work proceeds, nothing here may assert counts or orderings.
//! Only structural properties.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[tokio::test]
async fn indexes_its_own_source() {
    let (app, id) = workspace_over(&own_repo_root()).await;

    let (status, body) = get(&app, &format!("/v1/workspaces/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["indexedFiles"].as_u64().unwrap() > 5,
        "should have indexed its own sources: {body}"
    );
    assert!(
        body["hasGitRepo"].as_bool().unwrap(),
        "this repo is a git repo, so git discovery should succeed"
    );
    assert!(!body["isNetworkPath"].as_bool().unwrap());
    assert!(
        body["rescanIntervalSecs"].as_u64().unwrap() >= 60,
        "the derived interval must respect its floor"
    );
    assert!(body["idleTimeoutSecs"].as_u64().unwrap() >= 1800);
}

#[tokio::test]
async fn finds_its_own_manifest() {
    let (app, id) = workspace_over(&own_repo_root()).await;
    let (_, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "Cargo.toml", "pageSize": 20 }),
    )
    .await;

    assert!(
        paths_of(&body["items"])
            .iter()
            .any(|p| p.ends_with("Cargo.toml")),
        "expected a Cargo.toml hit, got {:?}",
        paths_of(&body["items"])
    );
}

#[tokio::test]
async fn pages_never_repeat_an_item() {
    let (app, id) = workspace_over(&own_repo_root()).await;

    let page = |n: usize| {
        let app = app.clone();
        let id = id.clone();
        async move {
            let (_, body) = post(
                &app,
                &format!("/v1/workspaces/{id}/search"),
                json!({ "query": "rs", "page": n, "pageSize": 5 }),
            )
            .await;
            body
        }
    };

    let first = page(0).await;
    let second = page(1).await;

    assert_eq!(
        first["totalMatched"], second["totalMatched"],
        "the total must not shift between pages of one query"
    );

    let a: std::collections::BTreeSet<String> = paths_of(&first["items"]).into_iter().collect();
    let b: std::collections::BTreeSet<String> = paths_of(&second["items"]).into_iter().collect();
    if !b.is_empty() {
        assert!(
            a.intersection(&b).next().is_none(),
            "page 0 and page 1 overlap: {:?}",
            a.intersection(&b).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn timestamps_are_rfc3339_and_plausible() {
    let (app, id) = workspace_over(&own_repo_root()).await;
    let (_, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "Cargo.toml", "pageSize": 5 }),
    )
    .await;

    let modified = body["items"][0]["item"]["modified"].as_str().unwrap();
    let parsed =
        time::OffsetDateTime::parse(modified, &time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|e| panic!("modified {modified:?} is not RFC 3339: {e}"));

    // A file in this repo cannot predate the project or sit in the future.
    assert!(
        parsed.year() >= 2020 && parsed.year() <= 2100,
        "implausible timestamp: {modified}"
    );
}

#[tokio::test]
async fn health_counts_live_workspaces() {
    let app = app();

    let (_, before) = get(&app, "/v1/health").await;
    assert_eq!(before["workspaceCount"].as_u64().unwrap(), 0);
    assert_eq!(before["status"].as_str().unwrap(), "ok");

    let (_, ws) = post(
        &app,
        "/v1/workspaces",
        json!({ "root": own_repo_root(), "waitFor": "scan" }),
    )
    .await;
    let id = ws["id"].as_str().unwrap().to_string();

    let (_, during) = get(&app, "/v1/health").await;
    assert_eq!(during["workspaceCount"].as_u64().unwrap(), 1);

    let (status, _) = send(&app, "DELETE", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, after) = get(&app, "/v1/health").await;
    assert_eq!(
        after["workspaceCount"].as_u64().unwrap(),
        0,
        "eviction must release the workspace"
    );
}

#[tokio::test]
async fn git_refresh_and_rescan_are_accepted() {
    let (app, id) = workspace_over(&own_repo_root()).await;

    let (status, body) = post(&app, &format!("/v1/workspaces/{id}/git/refresh"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["updated"].is_u64());

    let (status, body) = post(&app, &format!("/v1/workspaces/{id}/rescan"), json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(
        body["lastScanAt"].is_string(),
        "a rescan should stamp lastScanAt: {body}"
    );
}
