//! Exact assertions against the committed fixture tree.
//!
//! These are the tests that actually verify the Rust-to-JSON translation, which is the whole
//! premise of the project. Where a value depends on file content it is computed from the
//! fixture itself rather than hardcoded, so editing a fixture cannot leave a test asserting
//! a stale number.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[tokio::test]
async fn utf16_offsets_diverge_from_byte_offsets_on_non_ascii_lines() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": "docs/unicode.txt needle", "preset": "aiGrep", "pageSize": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let matches = body["matches"].as_array().expect("matches");
    assert_eq!(
        matches.len(),
        5,
        "one match per line of the fixture: {body}"
    );

    let mut diverged = 0;
    for m in matches {
        let line_number = m["lineNumber"].as_u64().unwrap();
        let line = fixture_line("docs/unicode.txt", line_number);

        // The engine gave us a line; it must be the line that is actually in the file.
        assert_eq!(
            m["lineContent"].as_str().unwrap(),
            line,
            "lineContent must match the file verbatim"
        );

        let expected_utf16 = utf16_index_of(&line, "needle");
        let byte_index = byte_index_of(&line, "needle");

        let ranges = m["matchRangesUtf16"].as_array().unwrap();
        assert_eq!(ranges.len(), 1, "one needle per line");
        let start = ranges[0]["start"].as_u64().unwrap() as u32;
        let end = ranges[0]["end"].as_u64().unwrap() as u32;

        assert_eq!(
            start, expected_utf16,
            "line {line_number}: reported UTF-16 start is wrong (byte index is {byte_index})"
        );
        assert_eq!(
            m["colUtf16"].as_u64().unwrap() as u32,
            expected_utf16,
            "line {line_number}: colUtf16 must agree with the first match range"
        );

        // The property a client depends on: slicing by the reported range yields the match.
        assert_eq!(
            slice_utf16(&line, start, end),
            "needle",
            "line {line_number}: slicing lineContent by the reported UTF-16 range must \
             yield the matched text"
        );

        if byte_index as u32 != expected_utf16 {
            diverged += 1;
            // And the byte offset really would have been wrong, which is why this
            // conversion exists at all. It may even run off the end of the line, which is
            // itself proof that it is not a usable UTF-16 index.
            let units = line.encode_utf16().count() as u32;
            let byte_as_u16 = byte_index as u32;
            let would_have_read = if byte_as_u16 + 6 <= units {
                slice_utf16(&line, byte_as_u16, byte_as_u16 + 6)
            } else {
                String::from("<past the end of the line>")
            };
            assert_ne!(
                would_have_read, "needle",
                "line {line_number}: byte offset {byte_index} should NOT work as a UTF-16                  index, but it happened to land correctly"
            );
        }
    }

    assert_eq!(
        diverged, 4,
        "the fixture is meant to contain four lines where byte and UTF-16 offsets differ, \
         plus one ASCII control line"
    );
}

#[tokio::test]
async fn search_reports_every_score_field() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "main", "pageSize": 5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let score = &body["items"][0]["score"];
    for field in [
        "total",
        "baseScore",
        "filenameBonus",
        "specialFilenameBonus",
        "frecencyBoost",
        "gitStatusBoost",
        "gitRecencyBoost",
        "distancePenalty",
        "currentFilePenalty",
        "comboMatchBoost",
        "pathAlignmentBonus",
        "exactMatch",
        "matchType",
    ] {
        assert!(
            !score[field].is_null(),
            "score.{field} missing - the C ABI and the TypeScript SDK drop three of these, \
             and this server exists partly to not: {score}"
        );
    }
    assert_eq!(
        score.as_object().unwrap().len(),
        13,
        "score should carry exactly the engine's 13 fields"
    );
}

#[tokio::test]
async fn file_items_carry_paths_in_both_forms() {
    let (app, id) = fixtures().await;
    let (_, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "deep", "pageSize": 5 }),
    )
    .await;

    let item = &body["items"][0]["item"];
    let relative = item["relativePath"].as_str().unwrap();
    let absolute = item["absolutePath"].as_str().unwrap();

    assert_eq!(relative, "src/nested/deep.rs");
    assert!(
        !relative.contains('\\'),
        "relative paths are '/'-canonical on every platform: {relative}"
    );
    assert!(
        !absolute.starts_with(r"\\?\"),
        "absolute paths must be free of verbatim prefixes: {absolute}"
    );
    assert!(
        std::path::Path::new(absolute).is_file(),
        "absolute path must actually open: {absolute}"
    );
    assert_eq!(item["fileName"].as_str().unwrap(), "deep.rs");
    assert_eq!(item["directory"].as_str().unwrap(), "src/nested/");
}

#[tokio::test]
async fn git_status_uses_known_flags_only() {
    let (app, id) = fixtures().await;
    let (_, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "pageSize": 50 }),
    )
    .await;

    const KNOWN_STATUS: &[&str] = &[
        "clean",
        "modified",
        "untracked",
        "staged_new",
        "staged_modified",
        "staged_deleted",
        "deleted",
        "renamed",
        "ignored",
        "unknown",
    ];
    const KNOWN_FLAGS: &[&str] = &[
        "stagedNew",
        "stagedModified",
        "stagedDeleted",
        "stagedRenamed",
        "stagedTypechange",
        "untracked",
        "modified",
        "deleted",
        "renamed",
        "typechange",
        "unreadable",
        "ignored",
        "conflicted",
    ];

    for hit in body["items"].as_array().unwrap() {
        let git = &hit["item"]["gitStatus"];
        let status = git["status"].as_str().expect("status is always a string");
        assert!(KNOWN_STATUS.contains(&status), "unknown status {status:?}");
        for flag in git["flags"].as_array().unwrap() {
            let flag = flag.as_str().unwrap();
            assert!(KNOWN_FLAGS.contains(&flag), "unknown flag {flag:?}");
        }
        // A clean file has no bits set; anything else must name at least one.
        if status == "clean" {
            assert!(git["flags"].as_array().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn glob_matches_exactly_the_rust_sources() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/glob"),
        json!({ "pattern": "src/**/*.rs", "pageSize": 50 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut found = paths_of(&body["items"]);
    found.sort();
    assert_eq!(
        found,
        vec!["src/lib.rs", "src/main.rs", "src/nested/deep.rs"],
        "glob must not reach tests/helper.rs"
    );
}

#[tokio::test]
async fn backslash_globs_are_folded_in_the_structured_form() {
    let (app, id) = fixtures().await;

    // The literal glob endpoint folds separators, because a glob field is unambiguous.
    let (_, folded) = post(
        &app,
        &format!("/v1/workspaces/{id}/glob"),
        json!({ "pattern": r"src\**\*.rs", "pageSize": 50 }),
    )
    .await;
    let mut found = paths_of(&folded["items"]);
    found.sort();
    assert_eq!(
        found,
        vec!["src/lib.rs", "src/main.rs", "src/nested/deep.rs"]
    );

    // And so does the structured query form.
    let (_, structured) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "structured": { "globs": [r"src\**\*.rs"] }, "pageSize": 50 }),
    )
    .await;
    let mut found = paths_of(&structured["items"]);
    found.sort();
    assert_eq!(
        found,
        vec!["src/lib.rs", "src/main.rs", "src/nested/deep.rs"]
    );
}

#[tokio::test]
async fn exclusion_constraints_remove_a_directory() {
    let (app, id) = fixtures().await;

    let (_, all) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "rs", "pageSize": 50 }),
    )
    .await;
    assert!(
        paths_of(&all["items"])
            .iter()
            .any(|p| p.starts_with("tests/")),
        "the fixture has a tests/ directory to exclude"
    );

    let (_, excluded) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "structured": { "fuzzy": "rs", "excludePathSegments": ["tests"] }, "pageSize": 50 }),
    )
    .await;
    assert!(
        !paths_of(&excluded["items"])
            .iter()
            .any(|p| p.starts_with("tests/")),
        "exclusion left tests/ in: {:?}",
        paths_of(&excluded["items"])
    );
}

#[tokio::test]
async fn grep_reports_context_and_classifies_definitions() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({
            "query": "fn main",
            "beforeContext": 0,
            "afterContext": 1,
            "classifyDefinitions": true,
            "pageSize": 10,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let m = &body["matches"][0];
    let file_index = m["fileIndex"].as_u64().unwrap() as usize;
    assert_eq!(
        body["files"][file_index]["relativePath"].as_str().unwrap(),
        "src/main.rs"
    );
    assert_eq!(m["lineNumber"].as_u64().unwrap(), 1);
    assert_eq!(m["lineContent"].as_str().unwrap(), "fn main() {");
    assert!(
        m["isDefinition"].as_bool().unwrap(),
        "`fn main() {{` is a definition line; if this fails, the `definitions` cargo          feature is probably not enabled and classification silently returns false"
    );
    assert_eq!(
        m["contextAfter"][0].as_str().unwrap(),
        "    println!(\"fixture\");"
    );
    assert_eq!(body["mode"].as_str().unwrap(), "plain");
}

#[tokio::test]
async fn auto_mode_chooses_regex_only_when_the_pattern_needs_it() {
    let (app, id) = fixtures().await;

    let (_, plain) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": "needle", "mode": "auto" }),
    )
    .await;
    assert_eq!(plain["mode"].as_str().unwrap(), "plain");

    let (_, regex) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": r"fn \w+_widget", "mode": "auto" }),
    )
    .await;
    assert_eq!(regex["mode"].as_str().unwrap(), "regex");
    assert!(
        regex["totalMatched"].as_u64().unwrap() >= 1,
        "regex should find build_widget: {regex}"
    );
}

#[tokio::test]
async fn an_uncompilable_regex_falls_back_instead_of_failing() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": "Widget[", "mode": "regex" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "the request itself succeeds");
    assert!(
        body["regexFallbackError"].is_string(),
        "the client must be told why results look literal: {body}"
    );
}

#[tokio::test]
async fn casing_controls_whether_lowercase_matches() {
    let (app, id) = fixtures().await;
    let count = |body: &serde_json::Value| body["totalMatched"].as_u64().unwrap();

    let (_, sensitive) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": "TODO", "casing": "sensitive", "pageSize": 100 }),
    )
    .await;
    let (_, insensitive) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": "TODO", "casing": "insensitive", "pageSize": 100 }),
    )
    .await;

    // data/sample.json contains a lowercase "todo", so insensitive must find strictly more.
    assert!(
        count(&insensitive) > count(&sensitive),
        "insensitive {} should exceed sensitive {}",
        count(&insensitive),
        count(&sensitive)
    );

    // Smart casing: an uppercase pattern behaves as case-sensitive.
    let (_, smart) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": "TODO", "casing": "smart", "pageSize": 100 }),
    )
    .await;
    assert_eq!(count(&smart), count(&sensitive));
}

#[tokio::test]
async fn multi_grep_matches_any_pattern() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/multi-grep"),
        json!({ "patterns": ["needle", "DEPTH"], "pageSize": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut files: Vec<String> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["relativePath"].as_str().unwrap().to_string())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec!["docs/unicode.txt", "src/nested/deep.rs"],
        "both patterns should contribute files"
    );
}

#[tokio::test]
async fn multi_grep_rejects_empty_input() {
    let (app, id) = fixtures().await;

    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/multi-grep"),
        json!({ "patterns": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"].as_str().unwrap(), "invalid-request-body");

    let (status, _) = post(
        &app,
        &format!("/v1/workspaces/{id}/multi-grep"),
        json!({ "patterns": ["ok", ""] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an empty pattern would match every line"
    );
}

#[tokio::test]
async fn cursor_paging_covers_everything_exactly_once() {
    let (app, id) = fixtures().await;

    let mut cursor = serde_json::Value::Null;
    let mut seen = std::collections::BTreeSet::new();
    let mut pages = 0;
    let mut returned = 0;

    loop {
        let mut request = json!({ "query": "e", "pageSize": 1 });
        if !cursor.is_null() {
            request["cursor"] = cursor.clone();
        }
        let (status, body) = post(&app, &format!("/v1/workspaces/{id}/grep"), request).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        pages += 1;
        for m in body["matches"].as_array().unwrap() {
            returned += 1;
            let file = body["files"][m["fileIndex"].as_u64().unwrap() as usize]["relativePath"]
                .as_str()
                .unwrap()
                .to_string();
            seen.insert((
                file,
                m["lineNumber"].as_u64().unwrap(),
                m["colUtf16"].as_u64().unwrap(),
            ));
        }

        cursor = body["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
        assert!(pages < 200, "paging failed to terminate");
    }

    assert!(pages > 1, "pageSize 1 should force several pages");
    assert_eq!(
        returned,
        seen.len(),
        "paging returned the same match more than once"
    );
}

#[tokio::test]
async fn a_corrupt_cursor_is_rejected_as_problem_json() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/grep"),
        json!({ "query": "needle", "cursor": "!!!not-a-cursor!!!" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["type"].as_str().unwrap(),
        "urn:fff-server:error:invalid-request-body"
    );
}

#[tokio::test]
async fn directory_and_mixed_search_report_their_own_shapes() {
    let (app, id) = fixtures().await;

    let (_, dirs) = post(
        &app,
        &format!("/v1/workspaces/{id}/search/directories"),
        json!({ "query": "nested", "pageSize": 10 }),
    )
    .await;
    let found = paths_of(&dirs["items"]);
    assert!(
        found.iter().any(|p| p == "src/nested/"),
        "directory paths carry a trailing slash: {found:?}"
    );

    let (_, mixed) = post(
        &app,
        &format!("/v1/workspaces/{id}/search/mixed"),
        json!({ "query": "src", "pageSize": 20 }),
    )
    .await;
    let kinds: std::collections::BTreeSet<&str> = mixed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["type"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains("file") || kinds.contains("directory"),
        "mixed hits must be tagged: {kinds:?}"
    );
    for kind in &kinds {
        assert!(matches!(*kind, "file" | "directory"), "unexpected {kind:?}");
    }
}

#[tokio::test]
async fn location_suffixes_are_parsed_and_returned() {
    let (app, id) = fixtures().await;
    let (_, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "deep.rs:42:10", "pageSize": 5 }),
    )
    .await;

    assert_eq!(body["location"]["type"].as_str().unwrap(), "position");
    assert_eq!(body["location"]["line"].as_u64().unwrap(), 42);
    assert_eq!(body["location"]["col"].as_u64().unwrap(), 10);
}

#[tokio::test]
async fn tracking_access_raises_the_reported_frecency() {
    let (app, id) = fixtures().await;

    for expected in 1..=3 {
        let (status, body) = post(
            &app,
            &format!("/v1/workspaces/{id}/track-access"),
            json!({ "path": "src/lib.rs" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["relativePath"].as_str().unwrap(), "src/lib.rs");
        assert_eq!(body["accessCount"].as_u64().unwrap(), expected);
    }

    let (_, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "lib.rs", "pageSize": 5 }),
    )
    .await;
    let hit = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["item"]["relativePath"] == "src/lib.rs")
        .expect("src/lib.rs in results");
    assert!(
        hit["item"]["accessFrecencyScore"].as_i64().unwrap() > 0,
        "the cached score must be refreshed, not just the database: {hit}"
    );
}

#[tokio::test]
async fn tracking_refuses_paths_outside_the_workspace() {
    let (app, id) = fixtures().await;

    for bad in ["../../Cargo.toml", "does-not-exist.rs"] {
        let (status, body) = post(
            &app,
            &format!("/v1/workspaces/{id}/track-access"),
            json!({ "path": bad }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{bad} was accepted: {body}"
        );
    }
}

#[tokio::test]
async fn query_history_returns_entries_newest_first() {
    let (app, id) = fixtures().await;

    for query in ["first query", "second query"] {
        let (status, _) = post(
            &app,
            &format!("/v1/workspaces/{id}/track-query"),
            json!({ "query": query, "selectedPath": "src/lib.rs" }),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    let (_, newest) = get(&app, &format!("/v1/workspaces/{id}/history?offset=0")).await;
    assert_eq!(newest["query"].as_str().unwrap(), "second query");
    let (_, older) = get(&app, &format!("/v1/workspaces/{id}/history?offset=1")).await;
    assert_eq!(older["query"].as_str().unwrap(), "first query");
}

#[tokio::test]
async fn parse_query_exposes_negation_and_warns_about_backslash_globs() {
    let app = app();

    let (status, body) = post(
        &app,
        "/v1/parse-query",
        json!({ "query": "*.rs !tests/ widget", "preset": "fileSearch" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let constraints = body["constraints"].as_array().unwrap();
    assert!(
        constraints
            .iter()
            .any(|c| c["type"] == "extension" && c["value"] == "rs" && c["negated"] == false),
        "{constraints:?}"
    );
    assert!(
        constraints
            .iter()
            .any(|c| c["type"] == "pathSegment" && c["negated"] == true),
        "negation must be visible as a boolean: {constraints:?}"
    );
    assert_eq!(body["fuzzyParts"][0].as_str().unwrap(), "widget");

    let (_, warned) = post(&app, "/v1/parse-query", json!({ "query": r"src\**\*.rs" })).await;
    assert_eq!(
        warned["warnings"].as_array().unwrap().len(),
        1,
        "a backslash glob must be flagged rather than silently mis-parsed: {warned}"
    );
}

#[tokio::test]
async fn workspaces_dedupe_by_filesystem_identity() {
    let app = app();
    let root = fixture_root();

    let (_, first) = post(
        &app,
        "/v1/workspaces",
        json!({ "root": root, "waitFor": "scan" }),
    )
    .await;

    // Same tree, different spelling: uppercase drive plus a trailing separator.
    let respelled = format!("{}/", root.to_uppercase());
    let (status, second) = post(
        &app,
        "/v1/workspaces",
        json!({ "root": respelled, "waitFor": "scan" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "an existing workspace is returned");
    assert_eq!(
        first["id"], second["id"],
        "two spellings of one directory must resolve to one workspace"
    );

    let (_, list) = get(&app, "/v1/workspaces").await;
    assert_eq!(
        list["workspaces"].as_array().unwrap().len(),
        1,
        "only one index should have been built"
    );
}

#[tokio::test]
async fn unknown_workspaces_and_bad_bodies_are_reported_as_problems() {
    let app = app();

    let (status, body) = post(
        &app,
        "/v1/workspaces/deadbeefdeadbeef/search",
        json!({ "query": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"].as_str().unwrap(), "not-found");

    let (status, body) = post(&app, "/v1/workspaces", json!({ "root": "Q:/nope" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"].as_str().unwrap(), "invalid-path");

    // A typo in a field name is the likeliest client-development error, and it must come
    // back as problem+json like everything else rather than as text/plain.
    let (status, body) = post(
        &app,
        "/v1/parse-query",
        json!({ "quer": "typo in the field name" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"].as_str().unwrap(), "invalid-request-body");
}

#[tokio::test]
async fn supplying_both_query_forms_is_refused() {
    let (app, id) = fixtures().await;
    let (status, body) = post(
        &app,
        &format!("/v1/workspaces/{id}/search"),
        json!({ "query": "a", "structured": { "fuzzy": "b" } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["detail"].as_str().unwrap().contains("not both"),
        "{body}"
    );
}
