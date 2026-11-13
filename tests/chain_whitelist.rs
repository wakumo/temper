use dashmap::DashMap;
use enso_temper::{config::Config, errors::handle_rejection, SharedSimulationState};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use warp::Filter;

fn transaction(chain_id: u64) -> Value {
    json!({"chainId": chain_id,
        "from":"0x1000000000000000000000000000000000000001",
        "to":"0x2000000000000000000000000000000000000002"})
}

#[tokio::test]
async fn all_simulation_routes_reject_disallowed_chains_without_rpc() {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let rpc = warp::post().map(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        warp::reply::json(
            &json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"unexpected RPC"}}),
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(warp::serve(rpc).incoming(listener).run());
    temp_env::async_with_vars([("BASE_BLOCKCHAIN_NODE_URL", Some(url.clone()))], async {
        let config = Config {
            fork_url: Some(url),
            ..Config::default()
        };
        let state = Arc::new(SharedSimulationState {
            evms: Arc::new(DashMap::new()),
        });
        let api = enso_temper::api::routes(config, state).recover(handle_rejection);
        let stateful = "/api/v1/simulate-stateful/00000000-0000-0000-0000-000000000001";
        for (path, body) in [
            ("/api/v1/simulate", transaction(10)),
            ("/api/v1/simulate-stateful", json!({"chainId":10})),
            ("/api/v1/simulate-bundle", json!([transaction(10)])),
            ("/api/v2/simulate-bundle", json!([transaction(10)])),
            (stateful, json!([transaction(10)])),
            // Validate the complete bundle before processing even the first allowed call.
            (
                "/api/v1/simulate-bundle",
                json!([transaction(1), transaction(10)]),
            ),
            (
                "/api/v2/simulate-bundle",
                json!([transaction(1), transaction(10)]),
            ),
            (stateful, json!([transaction(1), transaction(10)])),
        ] {
            let response = warp::test::request()
                .method("POST")
                .path(path)
                .json(&body)
                .reply(&api)
                .await;
            assert_eq!(response.status(), 400, "{path}");
            let body: Value = serde_json::from_slice(response.body()).unwrap();
            assert_eq!(body["message"], "CHAIN_ID_NOT_SUPPORTED", "{path}");
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
    })
    .await;
    server.abort();
}

#[tokio::test]
async fn explicit_whitelist_overrides_defaults_and_does_not_block_cleanup() {
    let config = Config {
        whitelist_chain_ids: vec![56],
        ..Config::default()
    };
    let state = Arc::new(SharedSimulationState {
        evms: Arc::new(DashMap::new()),
    });
    let api = enso_temper::api::routes(config, state).recover(handle_rejection);
    let response = warp::test::request()
        .method("POST")
        .path("/api/v1/simulate")
        .json(&transaction(137))
        .reply(&api)
        .await;
    assert_eq!(response.status(), 400);
    let body: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(body["message"], "CHAIN_ID_NOT_SUPPORTED");
    let cleanup = warp::test::request()
        .method("DELETE")
        .path("/api/v1/simulate-stateful/00000000-0000-0000-0000-000000000001")
        .reply(&api)
        .await;
    assert_eq!(cleanup.status(), 404);
    let body: Value = serde_json::from_slice(cleanup.body()).unwrap();
    assert_eq!(body["message"], "STATE_NOT_FOUND");
}
