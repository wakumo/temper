use std::sync::Arc;

use dashmap::DashMap;
use enso_temper::{config::Config, errors::handle_rejection, SharedSimulationState};
use serde_json::json;
use warp::Filter;

fn filter(
    api_key: Option<&str>,
) -> impl Filter<Extract = (impl warp::Reply,), Error = std::convert::Infallible> + Clone {
    let config = Config {
        port: 8080,
        fork_url: None,
        etherscan_key: None,
        api_key: api_key.map(str::to_owned),
    };
    let state = Arc::new(SharedSimulationState {
        evms: Arc::new(DashMap::new()),
    });
    enso_temper::api::routes(config, state).recover(handle_rejection)
}

#[tokio::test]
async fn stateful_bundle_is_registered_under_api_v2() {
    let response = warp::test::request()
        .method("POST")
        .path("/api/v2/simulate_bundle")
        .json(&json!([]))
        .reply(&filter(None))
        .await;
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(body["message"], "BUNDLE_REQUIRES_1_TO_20_CALLS");
}

#[tokio::test]
async fn rejects_old_and_cross_version_bundle_paths() {
    for path in [
        "/api/v1/simulate_bundle_v2",
        "/api/v1/simulate_bundle",
        "/api/v2/simulate_bundle_v2",
        "/api/v2/simulate-bundle",
        "/api/v2/simulate",
    ] {
        let response = warp::test::request()
            .method("POST")
            .path(path)
            .json(&json!([]))
            .reply(&filter(None))
            .await;
        assert_eq!(response.status(), 404, "{path}");
    }
}

#[tokio::test]
async fn preserves_v1_version_and_independent_bundle_routes() {
    let response = warp::test::request()
        .path("/api/v1/version")
        .reply(&filter(None))
        .await;
    assert_eq!(response.status(), 200);
    let response = warp::test::request()
        .method("POST")
        .path("/api/v1/simulate-bundle")
        .json(&json!([]))
        .reply(&filter(None))
        .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn both_versions_require_the_configured_api_key() {
    for path in ["/api/v1/simulate-bundle", "/api/v2/simulate_bundle"] {
        let api = filter(Some("test-key"));
        let missing = warp::test::request()
            .method("POST")
            .path(path)
            .json(&json!([]))
            .reply(&api)
            .await;
        assert_eq!(missing.status(), 401, "{path}");

        let authorized = warp::test::request()
            .method("POST")
            .path(path)
            .header("X-API-KEY", "test-key")
            .json(&json!([]))
            .reply(&api)
            .await;
        // Empty bundles reach the version-specific request validator.
        assert_eq!(authorized.status(), 400, "{path}");
    }
}
